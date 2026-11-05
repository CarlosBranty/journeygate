//! Read-only history queries for `guard suspects`: the repositories of a
//! checkout (superproject plus initialized submodules), commit logs with the
//! files they touched, uncommitted paths and one-line blames. Every call goes
//! through the non-interactive git of [`super`] with `LC_ALL=C`, explicit
//! arguments and no external diff; nothing here writes to a repository.

use super::GitError;
use super::base::{self, BaseError, MergeBaseError};
use super::changes::MAX_DEPTH;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// One repository level of a checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// `""` for the superproject, `sub/` (with the slash) for a submodule.
    pub prefix: String,
    pub dir: PathBuf,
}

/// One commit of a log, files relative to its own repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogCommit {
    pub sha: String,
    pub short: String,
    pub author: String,
    /// Author time, unix seconds.
    pub timestamp: u64,
    pub subject: String,
    pub files: Vec<String>,
}

/// Who last changed one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub sha: String,
    pub short: String,
    /// `None` for a line that is not committed yet.
    pub author: Option<String>,
    pub timestamp: u64,
    pub subject: String,
    /// The line itself.
    pub content: String,
    pub uncommitted: bool,
}

/// The base a repository's branch is compared against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchBase {
    pub reference: String,
    pub merge_base: String,
}

/// `git <args>` in `dir` with `LC_ALL=C`.
fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    super::run_with_env(dir, args, &[("LC_ALL", "C")], super::GIT_TIMEOUT)
}

/// The superproject, then every initialized submodule (depth first, at most
/// [`MAX_DEPTH`] levels). A submodule without a working tree is skipped.
pub fn repositories(root: &Path) -> Vec<Repo> {
    let mut out = vec![Repo {
        prefix: String::new(),
        dir: root.to_path_buf(),
    }];
    collect_submodules(root, "", 0, &mut out);
    out
}

fn collect_submodules(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<Repo>) {
    if depth >= MAX_DEPTH {
        return;
    }
    let Ok(bytes) = git(dir, &["ls-files", "--stage", "-z"]) else {
        return;
    };
    for entry in super::nul_list(&bytes) {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        if !meta.starts_with("160000 ") {
            continue;
        }
        let sub = dir.join(path);
        if !sub.join(".git").exists() {
            continue;
        }
        let sub_prefix = format!("{prefix}{path}/");
        out.push(Repo {
            prefix: sub_prefix.clone(),
            dir: sub.clone(),
        });
        collect_submodules(&sub, &sub_prefix, depth + 1, out);
    }
}

/// The repository's base and merge base, resolved like `guard affected`
/// does (`explicit`, then `env`: `GUARD_BASE` and CI, then the repository's
/// own default). The error is a one-line warning.
pub fn branch_base(
    dir: &Path,
    explicit: Option<&str>,
    env: base::Env<'_>,
) -> Result<BranchBase, String> {
    let resolved = base::resolve_base(dir, explicit, env, &mut |_| {}).map_err(|e| match e {
        BaseError::Unknown { reference, .. } => format!("unknown git ref: {reference}"),
        BaseError::NotFound { tried, shallow } => format!(
            "no base branch found{}; tried: {}",
            if shallow {
                " in this shallow clone"
            } else {
                ""
            },
            tried.join(", ")
        ),
        BaseError::Git(e) => format!("git failed: {e}"),
    })?;
    let merge_base = base::merge_base_checked(dir, &resolved.sha).map_err(|e| match e {
        MergeBaseError::NoCommits => "repository has no commits yet".to_string(),
        MergeBaseError::Shallow => format!(
            "shallow clone: {} is not reachable from HEAD",
            resolved.reference
        ),
        MergeBaseError::Unrelated => format!("{} and HEAD share no history", resolved.reference),
        MergeBaseError::Git(e) => format!("git failed: {e}"),
    })?;
    Ok(BranchBase {
        reference: resolved.reference,
        merge_base,
    })
}

/// The commit the gitlink at `path` records in `rev`'s tree of the
/// repository at `dir`, when `path` is a gitlink there.
pub fn gitlink_at(dir: &Path, rev: &str, path: &str) -> Option<String> {
    let out = git(dir, &["ls-tree", "-z", rev, "--", path]).ok()?;
    let entry = super::nul_list(&out).into_iter().next()?;
    let (meta, _) = entry.split_once('\t')?;
    let mut meta = meta.split(' ');
    (meta.next()? == "160000").then_some(())?;
    meta.nth(1).map(str::to_string)
}

/// Whether `sha` is a commit the repository at `dir` has.
pub fn has_commit(dir: &Path, sha: &str) -> bool {
    git(dir, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
}

const LOG_FORMAT: &str = "--format=%x1e%H%x1f%h%x1f%an%x1f%at%x1f%s";

/// `git log` of non-merge commits with the files each touched: `since..HEAD`
/// when `since` is given, else HEAD's history; at most `max` commits;
/// restricted to `pathspecs` when there are any. Submodule pointer changes
/// are left out: the submodule's own log has the files.
pub fn log(
    dir: &Path,
    since: Option<&str>,
    max: Option<usize>,
    pathspecs: &[String],
) -> Result<Vec<LogCommit>, GitError> {
    let range = since.map(|s| format!("{s}..HEAD"));
    let max = max.map(|n| format!("--max-count={n}"));
    let mut args = vec![
        "log",
        "--no-merges",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=all",
        "--name-only",
        LOG_FORMAT,
    ];
    args.extend(max.as_deref());
    args.push(range.as_deref().unwrap_or("HEAD"));
    args.push("--");
    args.extend(pathspecs.iter().map(String::as_str));
    git(dir, &args).map(|bytes| parse_log(&bytes))
}

/// Parses [`LOG_FORMAT`] records followed by `--name-only` lines.
pub fn parse_log(bytes: &[u8]) -> Vec<LogCommit> {
    let text = String::from_utf8_lossy(bytes);
    text.split('\u{1e}')
        .filter_map(|record| {
            let mut lines = record.lines();
            let header = lines.next()?;
            let fields: Vec<&str> = header.split('\u{1f}').collect();
            let [sha, short, author, at, subject] = fields[..] else {
                return None;
            };
            Some(LogCommit {
                sha: sha.to_string(),
                short: short.to_string(),
                author: author.to_string(),
                timestamp: at.trim().parse().unwrap_or(0),
                subject: subject.to_string(),
                files: lines
                    .map(str::trim_end)
                    .filter(|l| !l.is_empty())
                    .map(unquote)
                    .collect(),
            })
        })
        .collect()
}

/// A C-quoted git path (`"a\"b"`) back to its text; other paths unchanged.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Staged, unstaged and untracked paths of the repository at `dir` itself
/// (submodule contents excluded), relative to it.
pub fn uncommitted(dir: &Path) -> Result<Vec<String>, GitError> {
    git(
        dir,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=all",
        ],
    )
    .map(|bytes| parse_status_z(&bytes))
}

/// Parses `git status --porcelain=v1 -z`: `XY path`, a rename or copy
/// followed by its source path.
pub fn parse_status_z(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut entries = bytes.split(|b| *b == 0).filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        out.push(String::from_utf8_lossy(&entry[3..]).into_owned());
        if matches!(entry[0], b'R' | b'C')
            && let Some(source) = entries.next()
        {
            out.push(String::from_utf8_lossy(source).into_owned());
        }
    }
    out
}

/// `git blame --porcelain -L <line>,<line> -- <path>` of the working tree.
pub fn blame_line(dir: &Path, path: &str, line: u32) -> Result<BlameLine, GitError> {
    let range = format!("{line},{line}");
    let bytes = git(dir, &["blame", "--porcelain", "-L", &range, "--", path])?;
    parse_blame(&String::from_utf8_lossy(&bytes))
        .ok_or_else(|| GitError::Failed("git blame printed nothing".into()))
}

/// Parses one `git blame --porcelain` entry.
pub fn parse_blame(text: &str) -> Option<BlameLine> {
    let mut lines = text.lines();
    let sha = lines.next()?.split_whitespace().next()?.to_string();
    let uncommitted = sha.bytes().all(|b| b == b'0');
    let (mut author, mut timestamp, mut subject, mut content) = (None, 0, String::new(), None);
    for line in lines {
        if let Some(code) = line.strip_prefix('\t') {
            content = Some(code.trim().to_string());
            break;
        }
        match line.split_once(' ') {
            Some(("author", name)) => author = Some(name.to_string()),
            Some(("author-time", at)) => timestamp = at.trim().parse().unwrap_or(0),
            Some(("summary", text)) => subject = text.to_string(),
            _ => {}
        }
    }
    Some(BlameLine {
        short: sha.chars().take(7).collect(),
        sha,
        author: if uncommitted { None } else { author },
        timestamp,
        subject,
        content: content.unwrap_or_default(),
        uncommitted,
    })
}
