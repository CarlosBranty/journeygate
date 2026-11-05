//! `guard suspects [--journey <id>] [--report <path>] [--limit <n>] [--json]`:
//! for each failing or flaky journey of the last run, the changes most likely
//! related to it.
//!
//! - Commits since the branch base (`guard affected`'s base detection,
//!   `GUARD_BASE` honoured) that touch the journey's catalog `paths`. Paths
//!   inside a submodule are read from that submodule's own log, since the
//!   pointer its parent records at the parent's base: the same delta
//!   `guard affected` expands. A submodule new since that base (or whose
//!   recorded commit is not fetched) falls back to its own default base.
//!   Ranked by matched files, then newest.
//! - With none on the branch: the latest commits touching those paths,
//!   labelled "recent, not on this branch".
//! - Uncommitted working-tree changes on those paths.
//! - A blame of the line the test failed on (the error location, else the
//!   test's declaration line).
//!
//! Read-only: only `git log`, `status`, `blame`, `ls-files` and ref lookups
//! run. No git, no repository or no base is a warning, never a failure.

use super::explain::{Failure, select_failures};
use crate::cli::SuspectsArgs;
use crate::context::{Context, slash};
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::git::history::{self, BlameLine, BranchBase, LogCommit, Repo};
use guard_core::PathMatcher;
use guard_core::catalog::Catalog;
use guard_core::timefmt::rfc3339_utc;
use guard_core::verdict::{FailureLocation, classify, failure_locations, parse_report};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Commits scanned when a journey glob cannot be written as a pathspec.
const SCAN_DEPTH: usize = 300;

/// One commit that touched journey paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Suspect {
    /// The submodule it belongs to (`orders-api`), `None` for the superproject.
    pub repo: Option<String>,
    pub sha: String,
    pub short: String,
    pub author: String,
    /// Author date, RFC 3339 UTC.
    pub date: String,
    pub timestamp: u64,
    pub subject: String,
    /// The journey-matched files it touched, relative to the superproject.
    pub files: Vec<String>,
    /// Not on the branch: one of the latest commits on those paths.
    pub recent: bool,
}

/// Who last changed the line a test failed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Blame {
    /// Relative to the superproject.
    pub file: String,
    pub line: u32,
    pub sha: Option<String>,
    pub short: Option<String>,
    pub author: Option<String>,
    pub date: Option<String>,
    pub timestamp: Option<u64>,
    pub subject: Option<String>,
    pub content: String,
    pub uncommitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JourneySuspects {
    pub journey: Option<String>,
    pub title: String,
    /// `failed` when any of its tests failed, else `flaky`.
    pub status: String,
    pub tests: usize,
    pub commits: Vec<Suspect>,
    /// Uncommitted files on the journey paths, relative to the superproject.
    pub uncommitted: Vec<String>,
    pub blame: Vec<Blame>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoBase {
    pub repo: Option<String>,
    pub reference: String,
    pub merge_base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct SuspectsReport {
    pub bases: Vec<RepoBase>,
    pub journeys: Vec<JourneySuspects>,
    pub warnings: Vec<String>,
}

pub fn run(ctx: &Context, args: &SuspectsArgs) -> CliResult<u8> {
    let report_path = args
        .report
        .clone()
        .map(|p| super::absolute(&p))
        .unwrap_or_else(|| default_report(ctx));
    let report = collect(ctx, &report_path, args.journey.as_deref(), args.limit)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("suspects serialise")
        );
    } else {
        for warning in &report.warnings {
            crate::error::warning(warning);
        }
        print!("{}", render_text(&report, args.journey.as_deref()));
    }
    Ok(EXIT_OK)
}

pub fn default_report(ctx: &Context) -> PathBuf {
    ctx.e2e_dir.join("results").join("report.json")
}

/// Reads the report and gathers the suspects of every failing or flaky
/// journey (only `journey` when given). Only an unreadable report is an error.
pub fn collect(
    ctx: &Context,
    report_path: &Path,
    journey: Option<&str>,
    limit: usize,
) -> CliResult<SuspectsReport> {
    let text = std::fs::read_to_string(report_path).map_err(|e| {
        CliError::env(format!("cannot read {}: {e}", ctx.display(report_path)))
            .with_hint("run guard test first")
    })?;
    let report = parse_report(&text)
        .map_err(|e| CliError::env(format!("{}: {e}", ctx.display(report_path))))?;
    let failures = select_failures(&classify(&report), journey);
    Ok(gather(ctx, &failures, &failure_locations(&report), limit))
}

/// Failures grouped by journey, in report order.
fn group(failures: &[Failure]) -> Vec<(Option<String>, Vec<&Failure>)> {
    let mut groups: Vec<(Option<String>, Vec<&Failure>)> = Vec::new();
    for failure in failures {
        match groups.iter_mut().find(|(id, _)| *id == failure.journey) {
            Some((_, list)) => list.push(failure),
            None => groups.push((failure.journey.clone(), vec![failure])),
        }
    }
    groups
}

/// A repository level with its lazily read history.
struct Level {
    repo: Repo,
    /// The level whose gitlink this submodule is; `None` for the superproject.
    parent: Option<usize>,
    base_tried: bool,
    base: Option<BranchBase>,
    branch: Option<Vec<LogCommit>>,
    uncommitted: Option<Vec<String>>,
}

impl Level {
    fn name(&self) -> Option<String> {
        let name = self.repo.prefix.trim_end_matches('/');
        (!name.is_empty()).then(|| name.to_string())
    }

    fn label(&self) -> String {
        self.name().unwrap_or_else(|| "superproject".into())
    }
}

fn gather(
    ctx: &Context,
    failures: &[Failure],
    locations: &[FailureLocation],
    limit: usize,
) -> SuspectsReport {
    let mut out = SuspectsReport::default();
    if failures.is_empty() {
        return out;
    }
    let catalog = match ctx.read_catalog() {
        Ok(catalog) => Some(catalog),
        Err(messages) => {
            out.warnings.extend(messages);
            None
        }
    };
    let git_ok = if crate::git::version().is_none() {
        out.warnings.push("git is not on PATH".into());
        false
    } else if !ctx.in_git {
        out.warnings
            .push(format!("not a git repository: {}", slash(&ctx.root)));
        false
    } else {
        true
    };
    let mut levels: Vec<Level> = Vec::new();
    if git_ok {
        for repo in history::repositories(&ctx.root) {
            // Parents come first (depth-first): the longest prefix that
            // contains this one.
            let parent = (!repo.prefix.is_empty())
                .then(|| {
                    levels
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| repo.prefix.starts_with(&l.repo.prefix))
                        .max_by_key(|(_, l)| l.repo.prefix.len())
                        .map(|(i, _)| i)
                })
                .flatten();
            levels.push(Level {
                repo,
                parent,
                base_tried: false,
                base: None,
                branch: None,
                uncommitted: None,
            });
        }
    }
    for (id, tests) in group(failures) {
        let journey = id.as_deref().and_then(|j| find(catalog.as_ref(), j));
        let matcher = PathMatcher::new(journey.map(|j| j.paths.as_slice()).unwrap_or(&[]));
        let (commits, uncommitted) = if matcher.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            changes_for(&mut levels, &matcher, limit, &mut out)
        };
        let mut blame: Vec<Blame> = Vec::new();
        if git_ok {
            for failure in &tests {
                if let Some(b) = blame_for(ctx, &levels, failure, locations)
                    && !blame.iter().any(|x| x.file == b.file && x.line == b.line)
                {
                    blame.push(b);
                }
            }
        }
        out.journeys.push(JourneySuspects {
            title: journey
                .map(|j| j.title.clone())
                .unwrap_or_else(|| tests[0].title.clone()),
            status: if tests.iter().any(|t| t.status == "failed") {
                "failed"
            } else {
                "flaky"
            }
            .into(),
            tests: tests.len(),
            journey: id,
            commits,
            uncommitted,
            blame,
        });
    }
    for level in &levels {
        if let Some(base) = &level.base {
            out.bases.push(RepoBase {
                repo: level.name(),
                reference: base.reference.clone(),
                merge_base: base.merge_base.clone(),
            });
        }
    }
    out
}

fn find<'a>(catalog: Option<&'a Catalog>, id: &str) -> Option<&'a guard_core::catalog::Journey> {
    catalog?.journeys.iter().find(|j| j.id == id)
}

/// Branch (else recent) commits and uncommitted files on the journey paths.
fn changes_for(
    levels: &mut [Level],
    matcher: &PathMatcher,
    limit: usize,
    out: &mut SuspectsReport,
) -> (Vec<Suspect>, Vec<String>) {
    let mut commits = Vec::new();
    let mut uncommitted = Vec::new();
    for i in 0..levels.len() {
        if !matcher.may_match_under(&levels[i].repo.prefix) {
            continue;
        }
        load(levels, i, out);
        let level = &levels[i];
        let prefix = level.repo.prefix.clone();
        for commit in level.branch.as_deref().unwrap_or(&[]) {
            if let Some(suspect) = suspect(level, commit, matcher, false) {
                commits.push(suspect);
            }
        }
        uncommitted.extend(
            level
                .uncommitted
                .iter()
                .flatten()
                .map(|f| format!("{prefix}{f}"))
                .filter(|f| matcher.is_match(f)),
        );
    }
    if commits.is_empty() {
        for level in levels.iter() {
            if !matcher.may_match_under(&level.repo.prefix) {
                continue;
            }
            let found = match matcher.pathspecs_under(&level.repo.prefix) {
                Some(specs) if specs.is_empty() => continue,
                Some(specs) => history::log(&level.repo.dir, None, Some(limit), &specs),
                None => history::log(&level.repo.dir, None, Some(SCAN_DEPTH), &[]),
            };
            match found {
                Ok(log) => {
                    commits.extend(log.iter().filter_map(|c| suspect(level, c, matcher, true)))
                }
                Err(e) => out
                    .warnings
                    .push(format!("{}: git log failed: {e}", level.label())),
            }
        }
    }
    rank(&mut commits);
    commits.truncate(limit);
    (commits, uncommitted)
}

/// Most journey-matched files first, then newest.
fn rank(commits: &mut [Suspect]) {
    commits.sort_by(|a, b| {
        b.files
            .len()
            .cmp(&a.files.len())
            .then(b.timestamp.cmp(&a.timestamp))
    });
}

fn suspect(
    level: &Level,
    commit: &LogCommit,
    matcher: &PathMatcher,
    recent: bool,
) -> Option<Suspect> {
    let files: Vec<String> = commit
        .files
        .iter()
        .map(|f| format!("{}{f}", level.repo.prefix))
        .filter(|f| matcher.is_match(f))
        .collect();
    (!files.is_empty()).then(|| Suspect {
        repo: level.name(),
        sha: commit.sha.clone(),
        short: commit.short.clone(),
        author: commit.author.clone(),
        date: rfc3339_utc(commit.timestamp),
        timestamp: commit.timestamp,
        subject: commit.subject.clone(),
        files,
        recent,
    })
}

/// Resolves a level's base once: the superproject's like `guard affected`
/// (`GUARD_BASE` honoured), a submodule's from the pointer its parent
/// records at the parent's base, else the submodule's own default base.
fn ensure_base(levels: &mut [Level], i: usize, out: &mut SuspectsReport) {
    if levels[i].base_tried {
        return;
    }
    levels[i].base_tried = true;
    let process_env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let no_env = |_: &str| None;
    let resolved = match levels[i].parent {
        None => history::branch_base(&levels[i].repo.dir, None, &process_env),
        Some(p) => {
            ensure_base(levels, p, out);
            match pointer_base(&levels[p], &levels[i], out) {
                Some(base) => Ok(base),
                None => history::branch_base(&levels[i].repo.dir, None, &no_env),
            }
        }
    };
    match resolved {
        Ok(base) => levels[i].base = Some(base),
        Err(warning) => out.warnings.push(format!(
            "{}: {warning}; listing recent commits only",
            levels[i].label()
        )),
    }
}

/// The commit `parent` records for `child` at the parent's base, when the
/// gitlink exists there and the submodule has that commit.
fn pointer_base(parent: &Level, child: &Level, out: &mut SuspectsReport) -> Option<BranchBase> {
    let parent_base = parent.base.as_ref()?;
    let path = child.repo.prefix[parent.repo.prefix.len()..].trim_end_matches('/');
    let sha = history::gitlink_at(&parent.repo.dir, &parent_base.merge_base, path)?;
    if !history::has_commit(&child.repo.dir, &sha) {
        out.warnings.push(format!(
            "{}: the base records commit {} that is not fetched; using its own default base",
            child.label(),
            sha.chars().take(7).collect::<String>()
        ));
        return None;
    }
    // Ref names cannot hold `:`, so the part before it is the
    // superproject's reference.
    let top = parent_base.reference.split(':').next().unwrap_or_default();
    Some(BranchBase {
        reference: format!("{top}:{}", child.repo.prefix.trim_end_matches('/')),
        merge_base: sha,
    })
}

/// Reads a level's base, branch log and uncommitted files once.
fn load(levels: &mut [Level], i: usize, out: &mut SuspectsReport) {
    if levels[i].branch.is_some() {
        return;
    }
    ensure_base(levels, i, out);
    let level = &mut levels[i];
    if let Some(base) = &level.base {
        match history::log(&level.repo.dir, Some(&base.merge_base), None, &[]) {
            Ok(log) => level.branch = Some(log),
            Err(e) => out
                .warnings
                .push(format!("{}: git log failed: {e}", level.label())),
        }
    }
    level.branch.get_or_insert_with(Vec::new);
    match history::uncommitted(&level.repo.dir) {
        Ok(files) => level.uncommitted = Some(files),
        Err(e) => out
            .warnings
            .push(format!("{}: git status failed: {e}", level.label())),
    }
}

/// The blame of the line `failure` failed on, when it is inside the checkout.
fn blame_for(
    ctx: &Context,
    levels: &[Level],
    failure: &Failure,
    locations: &[FailureLocation],
) -> Option<Blame> {
    let (path, line) = locations
        .iter()
        .find(|l| {
            l.title == failure.title
                && l.spec_file == failure.file
                && l.spec_line == failure.line
                && l.project == failure.project
        })
        .map(|l| (PathBuf::from(&l.file), l.line))
        .unwrap_or_else(|| (ctx.e2e_dir.join(&failure.file), failure.line));
    let relative = repo_relative(&ctx.root, &path)?;
    let level = levels
        .iter()
        .filter(|l| relative.starts_with(&l.repo.prefix))
        .max_by_key(|l| l.repo.prefix.len())?;
    let inner = &relative[level.repo.prefix.len()..];
    match history::blame_line(&level.repo.dir, inner, line) {
        Ok(found) => Some(blame_row(relative, line, found)),
        Err(_) => {
            // Not known to git (a new spec): the line itself, uncommitted.
            let text = std::fs::read_to_string(&path).ok()?;
            let content = text.lines().nth(line.checked_sub(1)? as usize)?;
            Some(Blame {
                file: relative,
                line,
                sha: None,
                short: None,
                author: None,
                date: None,
                timestamp: None,
                subject: None,
                content: content.trim().to_string(),
                uncommitted: true,
            })
        }
    }
}

fn blame_row(file: String, line: u32, found: BlameLine) -> Blame {
    let committed = !found.uncommitted;
    Blame {
        file,
        line,
        sha: committed.then(|| found.sha.clone()),
        short: committed.then(|| found.short.clone()),
        author: found.author,
        date: committed.then(|| rfc3339_utc(found.timestamp)),
        timestamp: committed.then_some(found.timestamp),
        subject: committed.then_some(found.subject),
        content: found.content,
        uncommitted: found.uncommitted,
    }
}

/// `path` relative to `root` with `/`, or `None` outside it. Compared as
/// text first (case-insensitively on Windows), then canonicalised.
pub fn repo_relative(root: &Path, path: &Path) -> Option<String> {
    fn strip(root: &str, path: &str) -> Option<String> {
        let root = root.trim_end_matches('/');
        let head = path.get(..root.len())?;
        let same = if cfg!(windows) {
            head.eq_ignore_ascii_case(root)
        } else {
            head == root
        };
        let rest = path[root.len()..].strip_prefix('/')?;
        (same && !rest.is_empty()).then(|| rest.to_string())
    }
    let clean = |p: &Path| slash(p).trim_start_matches("//?/").to_string();
    strip(&clean(root), &clean(path)).or_else(|| {
        let root = std::fs::canonicalize(root).ok()?;
        let path = std::fs::canonicalize(path).ok()?;
        strip(&clean(&root), &clean(&path))
    })
}

fn day(date: &str) -> &str {
    date.get(..10).unwrap_or(date)
}

/// The human output.
pub fn render_text(report: &SuspectsReport, journey: Option<&str>) -> String {
    if report.journeys.is_empty() {
        return match journey {
            Some(j) => format!("Journey {j} has no failing or flaky result.\n"),
            None => "All green: no failing or flaky journeys.\n".to_string(),
        };
    }
    let mut out = String::new();
    if !report.bases.is_empty() {
        let bases: Vec<String> = report
            .bases
            .iter()
            .map(|b| {
                let short: String = b.merge_base.chars().take(7).collect();
                match &b.repo {
                    Some(repo) => format!("{repo}: {} (merge-base {short})", b.reference),
                    None => format!("{} (merge-base {short})", b.reference),
                }
            })
            .collect();
        out.push_str(&format!("base: {}\n\n", bases.join("; ")));
    }
    for j in &report.journeys {
        out.push_str(&format!(
            "{}  {} ({} test{})  {}\n",
            j.journey.as_deref().unwrap_or("(no journey)"),
            j.status,
            j.tests,
            if j.tests == 1 { "" } else { "s" },
            j.title
        ));
        if j.commits.is_empty() && j.uncommitted.is_empty() {
            out.push_str("  Possibly related changes: none found\n");
        } else {
            let recent = j.commits.iter().any(|c| c.recent);
            out.push_str(if recent {
                "  Possibly related changes (recent, not on this branch):\n"
            } else {
                "  Possibly related changes:\n"
            });
            for c in &j.commits {
                let files = match c.files.len() {
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                };
                let place = match &c.repo {
                    Some(repo) => format!("{repo}, {files}"),
                    None => files,
                };
                out.push_str(&format!(
                    "    {}  {}  {}  {}  [{place}]\n",
                    c.short,
                    day(&c.date),
                    c.author,
                    c.subject
                ));
            }
            if !j.uncommitted.is_empty() {
                out.push_str(&format!(
                    "    Uncommitted changes: {}\n",
                    j.uncommitted.join(", ")
                ));
            }
        }
        if !j.blame.is_empty() {
            out.push_str("  Spec line last changed:\n");
            for b in &j.blame {
                let who = if b.uncommitted {
                    "not committed yet".to_string()
                } else {
                    format!(
                        "{}  {}  {}  {}",
                        b.short.as_deref().unwrap_or("-"),
                        b.date.as_deref().map(day).unwrap_or("-"),
                        b.author.as_deref().unwrap_or("-"),
                        b.subject.as_deref().unwrap_or("")
                    )
                };
                out.push_str(&format!("    {}:{}  {who}\n", b.file, b.line));
                if !b.content.is_empty() {
                    out.push_str(&format!("      {}\n", b.content));
                }
            }
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suspect(files: usize, at: u64) -> Suspect {
        Suspect {
            repo: None,
            sha: format!("{at}"),
            short: format!("{at}"),
            author: "a".into(),
            date: rfc3339_utc(at),
            timestamp: at,
            subject: "s".into(),
            files: (0..files).map(|i| format!("f{i}")).collect(),
            recent: false,
        }
    }

    #[test]
    fn rank_puts_more_matched_files_first_then_newest() {
        let mut list = vec![
            suspect(1, 30),
            suspect(2, 10),
            suspect(1, 40),
            suspect(2, 20),
        ];
        rank(&mut list);
        let order: Vec<u64> = list.iter().map(|s| s.timestamp).collect();
        assert_eq!(order, [20, 10, 40, 30]);
    }

    #[test]
    fn repo_relative_strips_the_root_with_either_separator() {
        let root = Path::new("C:/work/repo");
        assert_eq!(
            repo_relative(root, Path::new("C:\\work\\repo\\e2e\\a.spec.ts")).as_deref(),
            Some("e2e/a.spec.ts")
        );
        assert_eq!(repo_relative(root, Path::new("C:/work/repo2/a.ts")), None);
        assert_eq!(repo_relative(root, Path::new("C:/work/repo")), None);
        if cfg!(windows) {
            assert_eq!(
                repo_relative(root, Path::new("c:/WORK/repo/a.ts")).as_deref(),
                Some("a.ts")
            );
        }
    }

    #[test]
    fn text_says_all_green_without_failures() {
        let report = SuspectsReport::default();
        assert_eq!(
            render_text(&report, None),
            "All green: no failing or flaky journeys.\n"
        );
        assert!(render_text(&report, Some("a.b")).contains("Journey a.b"));
    }
}
