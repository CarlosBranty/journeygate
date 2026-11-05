//! `guard gate finalize`, Phase 2 (finalize-phase2.md): merge and classify,
//! baseline against the base commit, Phase 2 blocking rule, fingerprints,
//! evidence upload, `.guard/gate-report.json`, and reporting.

mod baseline;
mod discover;
pub(crate) mod evidence;

use super::{run_id, unix_now};
use crate::cli::FinalizeArgs;
use crate::context::{Context, slash};
use crate::error::{CliError, CliResult, EXIT_ENV, warning};
use crate::git;
use crate::playwright::{self, Runtime};
use crate::runtime::block_on;
use crate::server_client::ServerClient;
use baseline::Target;
use discover::Env;
use guard_core::baseline::{BlockingFacts, is_blocking};
use guard_core::catalog::Catalog;
use guard_core::config::{RequireTicket, TargetConfig};
use guard_core::durations::{DurationHistory, parse_history, run_durations};
use guard_core::fingerprint::{Fingerprint, fingerprint, test_id};
use guard_core::journey_changes::{ChangeReview, JourneyChange, review};
use guard_core::ports::{
    BaselineOutcome, BugAction, CodeHost, EvidenceBundle, EvidenceFile, EvidenceStore, GateCounts,
    GateReport, GateTest, GateVerdict, IssueTracker, JourneyChangeReport, PrContext, PrRef,
    ReportOutcome, TestStatus,
};
use guard_core::routing::{Reason, SMOKE_TAG, select_affected};
use guard_core::timefmt::rfc3339_utc;
use guard_core::verdict::{Classification, PwReport, TestOutcome, Verdict, VerdictDocument};
use guard_core::verdict::{classify, parse_report};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Flag added when `[tracker].require_ticket` finds no ticket key.
pub fn missing_ticket_flag(mode: RequireTicket) -> String {
    let mode = match mode {
        RequireTicket::Warn => "warn",
        RequireTicket::Block => "block",
    };
    format!("PR has no ticket (require_ticket = {mode})")
}

fn load_history(ctx: &Context) -> CliResult<DurationHistory> {
    match fs::read_to_string(ctx.durations_path()) {
        Ok(text) => parse_history(&text)
            .map_err(|e| CliError::usage(format!("{}: {e}", ctx.display(&ctx.durations_path())))),
        Err(_) => Ok(DurationHistory::default()),
    }
}

/// Phase 1 steps 1-2: blobs exist, `merge-reports` produced a non-empty report.
fn merge(ctx: &Context, blobs: &Path) -> CliResult<(PwReport, serde_json::Value)> {
    if !blobs.is_dir() {
        return Err(CliError::usage(format!(
            "blob dir {} does not exist",
            blobs.display()
        )));
    }
    let has_zip = fs::read_dir(blobs)
        .map(|entries| {
            entries.flatten().any(|e| {
                e.path()
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("zip"))
            })
        })
        .unwrap_or(false);
    if !has_zip {
        return Err(CliError::env(format!(
            "no *.zip blob reports in {}",
            blobs.display()
        )));
    }
    let report_path: PathBuf = ctx.e2e_dir.join("results").join("report.json");
    let _ = fs::remove_file(&report_path);
    let _ = fs::create_dir_all(ctx.e2e_dir.join("results"));
    let args = vec![
        "playwright".to_string(),
        "merge-reports".to_string(),
        "--reporter=json,html".to_string(),
        slash(blobs),
    ];
    let env = vec![(
        "PLAYWRIGHT_JSON_OUTPUT_NAME".to_string(),
        report_path.to_string_lossy().into_owned(),
    )];
    let exit = playwright::run(&ctx.e2e_dir, &args, &env, &Runtime::Local)?;
    if exit != Some(0) {
        return Err(CliError::env(format!(
            "playwright merge-reports failed ({exit:?})"
        )));
    }
    let text = fs::read_to_string(&report_path).map_err(|e| {
        CliError::env(format!(
            "merged report missing at {}: {e}",
            ctx.display(&report_path)
        ))
    })?;
    let report = parse_report(&text).map_err(|e| CliError::env(e.to_string()))?;
    if report.test_count() == 0 {
        return Err(CliError::env("the merged report contains zero tests"));
    }
    let raw = serde_json::from_str(&text).map_err(|e| CliError::env(e.to_string()))?;
    Ok((report, raw))
}

fn status_of(c: Classification) -> TestStatus {
    match c {
        Classification::Passed => TestStatus::Passed,
        Classification::Flaky => TestStatus::Flaky,
        Classification::Failed => TestStatus::Failed,
        Classification::Skipped => TestStatus::Skipped,
    }
}

fn id_of(t: &TestOutcome) -> String {
    test_id(&t.project, &t.file, &t.title)
}

/// Journeys selected by a changed file matching their `paths` (Path reason).
pub fn path_selected(catalog: &Catalog, changed: &[String]) -> BTreeSet<String> {
    select_affected(catalog, changed, None)
        .map(|s| {
            s.journeys
                .into_iter()
                .filter(|j| j.reasons.iter().any(|r| matches!(r, Reason::Path { .. })))
                .map(|j| j.id)
                .collect()
        })
        .unwrap_or_default()
}

/// The last base sha candidate: `git merge-base HEAD <d>`, `<d>` the
/// repository's default base (cli.md "Base resolution" steps 4-6). A merge
/// base equal to HEAD (a run on the default branch itself) is no base: a
/// baseline of HEAD against itself would call every failure pre-existing.
fn fallback_base_sha(root: &std::path::Path) -> Option<String> {
    let default = git::base::detect_repo_default(root)?;
    merge_base_below_head(root, &default.sha)
}

/// `git merge-base HEAD <sha>`, none when it is HEAD itself.
fn merge_base_below_head(root: &std::path::Path, sha: &str) -> Option<String> {
    let merge_base = git::merge_base_of(root, "HEAD", sha).ok()?;
    let head = git::head_sha(root).ok()?;
    (merge_base != head).then_some(merge_base)
}

/// The base sha for `GUARD_BASE`: its merge base with HEAD, the commit
/// `guard test --affected` diffed from. A reference that is not a commit is
/// kept as given, so the caller warns and runs without a base.
fn guard_base_sha(root: &std::path::Path, reference: &str) -> Option<String> {
    match git::resolve_commit(root, reference) {
        Ok(Some(sha)) => merge_base_below_head(root, &sha),
        _ => Some(reference.to_string()),
    }
}

/// A PR API's changed-file list with each submodule pointer replaced by its
/// local expansion (cli.md "Changed files"), or by the subtree entry `<path>/`
/// without a base sha. On a git error the list is kept as given.
fn expand_pr_files(
    root: &std::path::Path,
    base_sha: Option<&str>,
    files: &[String],
) -> Vec<String> {
    match git::changes::expand_gitlinks(root, base_sha, files) {
        Ok(set) => {
            git::print_warnings(&set);
            set.paths
        }
        Err(e) => {
            warning(format!("changed files: {e}"));
            files.to_vec()
        }
    }
}

/// Flag added when the changed files of a PR run cannot be determined.
pub const CHANGED_FILES_UNKNOWN_FLAG: &str =
    "changed files unknown: every flaky test blocks and journey changes were not reviewed";

/// Step 2 changed files: `pr.changed_files` (its submodule pointers replaced
/// by `expand`), else `diff(base)`, else empty outside a PR run. `None`
/// (unknown) when the diff fails, or when a PR run has neither a PR context
/// nor a base: the gate then fails closed.
fn changed_files_of(
    pr: Option<&PrContext>,
    base: Option<&str>,
    is_pr_run: bool,
    diff: impl FnOnce(&str) -> Result<Vec<String>, String>,
    expand: impl FnOnce(&[String]) -> Vec<String>,
) -> Option<Vec<String>> {
    match (pr, base) {
        (Some(pr), _) => Some(expand(&pr.changed_files)),
        (None, Some(base)) => match diff(base) {
            Ok(files) => Some(files),
            Err(e) => {
                warning(format!("changed files: {e}"));
                None
            }
        },
        (None, None) if is_pr_run => None,
        (None, None) => Some(Vec::new()),
    }
}

/// The facts the Phase 2 blocking rule needs for one test. `by_path = None`
/// means the changed files are unknown: every test counts as path-affected.
pub fn blocking_facts(
    test: &TestOutcome,
    baseline: Option<BaselineOutcome>,
    by_path: Option<&BTreeSet<String>>,
) -> BlockingFacts {
    BlockingFacts {
        status: status_of(test.status),
        baseline,
        smoke: test.tags.iter().any(|t| t == SMOKE_TAG),
        affected_by_path: match by_path {
            Some(by_path) => test.journey.as_ref().is_some_and(|j| by_path.contains(j)),
            None => true,
        },
    }
}

/// Splits the flags of `journey_changes::review` into the global flags (kept in
/// `GateReport.change_flags`) and one report per changed journey whose flags
/// have the `<id>: ` prefix stripped (the renderers add it back).
fn split_flags(
    changes: &[JourneyChange],
    flags: Vec<String>,
) -> (Vec<String>, Vec<JourneyChangeReport>) {
    let mut reports: Vec<JourneyChangeReport> = changes
        .iter()
        .map(|c| JourneyChangeReport {
            journey: c.id.clone(),
            file: c.file.clone(),
            flags: Vec::new(),
        })
        .collect();
    let mut global = Vec::new();
    for flag in flags {
        // The longest matching id wins, so `a.bc: ...` never lands on `a.b`.
        let owner = reports
            .iter_mut()
            .filter(|r| flag.starts_with(&format!("{}: ", r.journey)))
            .max_by_key(|r| r.journey.len());
        match owner {
            Some(report) => {
                let rest = flag[report.journey.len() + 2..].to_string();
                report.flags.push(rest);
            }
            None => global.push(flag),
        }
    }
    (global, reports)
}

/// The stdout `journeys:` lines: global flags (minus the ticket flag, printed
/// as `ticket:`), then each journey's flags as `<id>: <flag>`.
fn journey_lines(global: &[String], reports: &[JourneyChangeReport]) -> Vec<String> {
    let mut lines: Vec<String> = global
        .iter()
        .filter(|f| !f.starts_with("PR has no ticket"))
        .map(|f| format!("journeys: {f}"))
        .collect();
    for report in reports {
        for flag in &report.flags {
            lines.push(format!("journeys: {}: {flag}", report.journey));
        }
    }
    lines
}

/// Step 3: every changed journey spec, compared against the base commit.
fn journey_changes(
    ctx: &Context,
    base_sha: Option<&str>,
    changed: &[String],
) -> Vec<JourneyChange> {
    let e2e_rel = ctx
        .e2e_dir
        .strip_prefix(&ctx.root)
        .map(slash)
        .unwrap_or_else(|_| "e2e-playwright".into());
    let prefix = format!("{e2e_rel}/tests/journeys/");
    changed
        .iter()
        .filter(|f| f.starts_with(&prefix))
        .filter_map(|path| {
            let head = git::show_file(&ctx.root, "HEAD", path)
                .or_else(|| fs::read_to_string(ctx.root.join(path)).ok())?;
            let base = base_sha.and_then(|b| git::show_file(&ctx.root, b, path));
            guard_impact::journey_change(path, base.as_deref(), &head)
        })
        .collect()
}

/// Screenshots for direct Jira reporting, read from the local files.
struct LocalScreenshots(HashMap<String, PathBuf>);

#[async_trait::async_trait]
impl guard_tracker::report::ScreenshotSource for LocalScreenshots {
    async fn read(&self, file: &EvidenceFile) -> Option<Vec<u8>> {
        tokio::fs::read(self.0.get(&file.name)?).await.ok()
    }
}

/// What the summary prints about reporting.
#[derive(Default)]
struct Reported {
    jira_line: Option<String>,
    pr_line: Option<String>,
}

pub fn run(ctx: &Context, args: &FinalizeArgs) -> CliResult<u8> {
    let env: Env<'_> = &discover::process_env;
    let run_id = run_id()?;
    let blobs = super::absolute(&args.blobs);
    let config = ctx.load_config()?;
    let catalog = ctx.load_catalog(&config)?;
    let mut history = load_history(ctx)?;
    let event = discover::github_event(env);
    let git_head = || git::head_sha(&ctx.root).ok();
    // Before the PR context is known (merge failures): event, CI, then HEAD.
    let early_head_sha =
        discover::head_sha(None, env, event.as_ref(), git_head).unwrap_or_default();
    let pr_number = discover::pr_number(args.pr, env, event.as_ref());
    let code_host: Option<Arc<dyn CodeHost>> = if discover::has_code_host_credentials(&config, env)
    {
        config
            .code_host
            .as_ref()
            .and_then(|c| match guard_codehost::from_env(c.kind) {
                Ok(host) => Some(host),
                Err(e) => {
                    warning(format!("code host: {e}"));
                    None
                }
            })
    } else {
        None
    };

    let (report, raw) = match merge(ctx, &blobs) {
        Ok(merged) => merged,
        Err(error) => {
            if error.code == EXIT_ENV && !args.no_report {
                publish_infrastructure(
                    &config,
                    code_host.as_deref(),
                    &run_id,
                    &early_head_sha,
                    &error,
                );
            }
            return Err(error);
        }
    };
    let mut tests = classify(&report);

    // ---- step 2: context
    let server = ServerClient::from_config(&config);
    let ticket_regex = config.ticket_regex().ok().flatten();
    let mut pr: Option<PrContext> = None;
    let mut server_flow_change: Option<bool> = None;
    if let Some(n) = pr_number {
        if let Some(client) = &server {
            match block_on(client.pr_context(n)) {
                Ok(answer) => {
                    server_flow_change = answer.ticket.as_ref().map(|t| t.flow_change);
                    pr = Some(answer.pr);
                }
                Err(e) => warning(format!("pr context: {e}")),
            }
        } else if let Some(host) = &code_host {
            match block_on(host.pr_context(PrRef { number: n }, ticket_regex.as_ref())) {
                Ok(context) => pr = Some(context),
                Err(e) => warning(format!("pr context: {e}")),
            }
        }
    }
    let head_sha =
        discover::head_sha(pr.as_ref(), env, event.as_ref(), git_head).unwrap_or_default();
    let base_sha = discover::base_sha(
        args.base.as_deref(),
        env,
        pr.as_ref(),
        event.as_ref(),
        |reference| guard_base_sha(&ctx.root, reference),
        || fallback_base_sha(&ctx.root),
    );
    // A discovered base must be a commit of this clone (shallow CI clones need
    // `fetch-depth: 0`); recorded as its full sha.
    let base_sha = base_sha.and_then(|base| match git::resolve_commit(&ctx.root, &base) {
        Ok(Some(sha)) => Some(sha),
        _ => {
            warning(format!(
                "baseline: base {base} is not a commit of this clone (fetch the full history)"
            ));
            None
        }
    });
    let changed_known = changed_files_of(
        pr.as_ref(),
        base_sha.as_deref(),
        pr_number.is_some(),
        |base| git::committed_changes(&ctx.root, base).map_err(|e| e.to_string()),
        |files| expand_pr_files(&ctx.root, base_sha.as_deref(), files),
    );
    let changed_files: Vec<String> = changed_known.clone().unwrap_or_default();
    let branch = discover::branch(env, git::current_branch(&ctx.root).ok().flatten());
    let ticket_key = pr.as_ref().and_then(|p| p.ticket_key.clone()).or_else(|| {
        ticket_regex
            .as_ref()
            .and_then(|re| guard_core::ticket::extract_ticket(re, branch.as_deref()))
    });
    let jira = direct_jira(&config, env, server.is_some());
    let ticket_is_flow_change = match (server_flow_change, &jira, &ticket_key) {
        (Some(flow), _, _) => flow,
        (None, Some(jira), Some(key)) => block_on(jira.get_issue(key))
            .map(|issue| issue.labels.iter().any(|l| l == "flow-change"))
            .unwrap_or_else(|e| {
                warning(format!("ticket {key}: {e}"));
                false
            }),
        _ => false,
    };

    // ---- step 3: journey changes
    let changes = journey_changes(ctx, base_sha.as_deref(), &changed_files);
    let review_flags = match review(&changes, ticket_is_flow_change) {
        ChangeReview::Clean => Vec::new(),
        ChangeReview::Flagged(flags) => flags,
    };
    let (mut change_flags, journey_reports) = split_flags(&changes, review_flags);
    if changed_known.is_none() {
        change_flags.push(CHANGED_FILES_UNKNOWN_FLAG.to_string());
    }

    // ---- step 4: baseline
    let targets: Vec<Target> = tests
        .iter()
        .filter(|t| t.status == Classification::Failed)
        .map(|t| Target {
            test_id: id_of(t),
            file: t.file.clone(),
            title: t.title.clone(),
            project: t.project.clone(),
        })
        .collect();
    let outcomes: Option<BTreeMap<String, BaselineOutcome>> = if args.no_baseline {
        None
    } else {
        match &base_sha {
            None => {
                if !targets.is_empty() {
                    println!("baseline: no base commit, failures stay blocking");
                }
                Some(BTreeMap::new())
            }
            Some(base) if !targets.is_empty() => {
                Some(baseline::run(ctx, &config, base, &run_id, &targets))
            }
            Some(_) => Some(BTreeMap::new()),
        }
    };

    // ---- step 5: blocking
    let by_path = changed_known
        .as_ref()
        .map(|files| path_selected(&catalog, files));
    for test in &mut tests {
        let baseline = outcomes.as_ref().and_then(|o| o.get(&id_of(test)).copied());
        test.blocking = is_blocking(blocking_facts(test, baseline, by_path.as_ref()));
    }
    let mut red = tests.iter().any(|t| t.blocking);
    if let (Some(tracker), Some(_)) = (&config.tracker, pr_number) {
        let author = pr.as_ref().map(|p| p.author.as_str());
        if ticket_key.is_none() && !discover::is_bot(author) {
            let flag = missing_ticket_flag(tracker.require_ticket);
            if tracker.require_ticket == RequireTicket::Block {
                red = true;
                println!("ticket: {flag}");
            } else {
                warning(&flag);
            }
            change_flags.push(flag);
        }
    }

    // ---- steps 6-7: fingerprints and evidence
    let repo = discover::repo_name(pr.as_ref(), env, &config);
    let store: Option<Arc<dyn EvidenceStore>> =
        discover::process_env(&config.evidence.storage_url_env).and_then(|url| {
            match guard_store::evidence_store_from_url(&url) {
                Ok(store) => Some(store),
                Err(e) => {
                    warning(format!("evidence: {e}"));
                    None
                }
            }
        });
    let raw_tests = evidence::raw_tests(&raw);
    let ttl = evidence::link_ttl(config.evidence.retention_days);
    let mut taken_names = BTreeSet::new();
    let mut local_paths: HashMap<String, PathBuf> = HashMap::new();
    let mut uploaded = 0usize;
    let mut needs_evidence = false;
    let mut store_down = false;
    let mut gate_tests = Vec::with_capacity(tests.len());
    for test in &tests {
        let id = id_of(test);
        let baseline = outcomes.as_ref().and_then(|o| o.get(&id).copied());
        let fp: Option<Fingerprint> =
            matches!(test.status, Classification::Failed | Classification::Flaky)
                .then(|| fingerprint(&id, test.error.as_deref().unwrap_or("")));
        let wants_evidence = test.status == Classification::Failed
            || (test.status == Classification::Flaky && test.blocking);
        let mut bundle = None;
        if let (true, Some(fp)) = (wants_evidence, &fp) {
            needs_evidence = true;
            let raw_test = raw_tests
                .iter()
                .find(|r| r.file == test.file && r.title == test.title && r.project == test.project)
                .map(|r| r.test);
            let mut files = Vec::new();
            for local in raw_test
                .map(|t| evidence::local_files(t, &ctx.e2e_dir))
                .unwrap_or_default()
            {
                let base_name = local
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "file".into());
                let name = evidence::unique_name(&base_name, &mut taken_names);
                local_paths.insert(name.clone(), local.path.clone());
                let file = match &store {
                    Some(store) if !store_down => {
                        let key = format!("{repo}/{run_id}/{}/{name}", fp.as_str());
                        match block_on(evidence::upload(store.as_ref(), &key, &local, &name, ttl)) {
                            Ok(file) => {
                                uploaded += 1;
                                file
                            }
                            Err(e) => {
                                warning(format!("evidence: {e}"));
                                if e.store_down {
                                    // Every later upload would cost another timeout.
                                    warning("evidence: storage unreachable, the rest stays local");
                                    store_down = true;
                                }
                                evidence::local_only(&local, &name)
                            }
                        }
                    }
                    _ => evidence::local_only(&local, &name),
                };
                files.push(file);
            }
            bundle = Some(EvidenceBundle {
                test_id: id.clone(),
                fingerprint: fp.clone(),
                title: test.title.clone(),
                journey: test.journey.clone(),
                location: raw_test.and_then(|t| evidence::location(t, &ctx.e2e_dir)),
                error: test.error.clone().unwrap_or_default(),
                step: None,
                seed: raw_test.and_then(evidence::seed),
                commit: head_sha.clone(),
                attempts: raw_test.map(evidence::attempts).unwrap_or_default(),
                baseline,
                files,
            });
        }
        gate_tests.push(GateTest {
            test_id: id,
            title: test.title.clone(),
            file: test.file.clone(),
            line: test.line,
            project: test.project.clone(),
            tags: test.tags.clone(),
            journey: test.journey.clone(),
            status: status_of(test.status),
            blocking: test.blocking,
            attempts: test.attempts,
            duration_ms: test.duration_ms,
            error: test.error.clone(),
            baseline,
            fingerprint: fp,
            evidence: bundle,
        });
    }
    let mut report_url = None;
    if let Some(store) = &store {
        if needs_evidence && !store_down {
            report_url = upload_html_report(ctx, store.as_ref(), &repo, &run_id, ttl);
        }
    }

    // ---- step 8: files
    let counts = guard_core::verdict::count(&tests);
    let regressions = gate_tests
        .iter()
        .filter(|t| t.baseline == Some(BaselineOutcome::Regression))
        .count();
    let pre_existing = gate_tests
        .iter()
        .filter(|t| t.baseline == Some(BaselineOutcome::PreExisting))
        .count();
    let verdict = if red { Verdict::Red } else { Verdict::Green };
    let generated_at = rfc3339_utc(unix_now());
    let gate_report = GateReport {
        version: 2,
        run_id: run_id.clone(),
        generated_at: generated_at.clone(),
        provider: discover::provider(&config),
        repo: Some(repo.clone()),
        pr: pr.clone(),
        ticket_key: ticket_key.clone(),
        ticket_is_flow_change,
        head_sha: head_sha.clone(),
        base_sha: base_sha.clone(),
        verdict: match verdict {
            Verdict::Green => GateVerdict::Green,
            Verdict::Red => GateVerdict::Red,
        },
        counts: GateCounts {
            passed: counts.passed,
            flaky: counts.flaky,
            failed: counts.failed,
            skipped: counts.skipped,
            regressions,
            pre_existing,
        },
        tests: gate_tests,
        journey_changes: journey_reports,
        change_flags: change_flags.clone(),
        report_url,
        ci_url: discover::ci_url(env),
    };
    let run = run_durations(&tests);
    let mut document = VerdictDocument::new(run_id.clone(), generated_at, tests);
    document.verdict = verdict;
    document.not_implemented = Vec::new();
    document.baseline = outcomes.as_ref().map(|o| {
        serde_json::json!({
            "base_sha": base_sha,
            "outcomes": o,
        })
    });
    fs::create_dir_all(ctx.guard_dir())
        .map_err(|e| CliError::env(format!("cannot create .guard/: {e}")))?;
    let write = |name: &str, body: String| {
        fs::write(ctx.guard_dir().join(name), body)
            .map_err(|e| CliError::env(format!("cannot write .guard/{name}: {e}")))
    };
    write(
        "verdict.json",
        serde_json::to_string_pretty(&document).expect("verdict serialises") + "\n",
    )?;
    write(
        "gate-report.json",
        serde_json::to_string_pretty(&gate_report).expect("report serialises") + "\n",
    )?;
    history.merge(&run);
    fs::write(ctx.durations_path(), history.to_json() + "\n")
        .map_err(|e| CliError::env(format!("cannot write .guard/durations.json: {e}")))?;

    // ---- step 9: report
    let mut outcome = ReportOutcome::default();
    let mut reported = Reported::default();
    if !args.no_report {
        reported = publish(
            &config,
            &gate_report,
            server.as_ref(),
            jira.as_deref(),
            code_host.as_deref(),
            pr_number,
            LocalScreenshots(local_paths),
            &mut outcome,
        );
    }

    // ---- step 10: print
    print!("{}", summary(&document, &gate_report, &outcome));
    for line in journey_lines(&gate_report.change_flags, &gate_report.journey_changes) {
        println!("{line}");
    }
    if needs_evidence {
        if store.is_some() {
            let noun = if uploaded == 1 { "file" } else { "files" };
            println!("evidence: {uploaded} {noun} uploaded");
        } else {
            println!("evidence: storage not configured, nothing uploaded");
        }
    }
    if let Some(line) = reported.jira_line {
        println!("{line}");
    }
    if let Some(line) = reported.pr_line {
        println!("{line}");
    }
    Ok(u8::try_from(document.verdict.exit_code()).unwrap_or(EXIT_ENV))
}

/// The direct-mode Jira adapter: no `[server]`, `[tracker].kind = "jira"`
/// with a project, and the three `JIRA_*` variables.
fn direct_jira(
    config: &TargetConfig,
    env: Env<'_>,
    has_server: bool,
) -> Option<Arc<dyn IssueTracker>> {
    if has_server || !discover::has_jira_credentials(env) {
        return None;
    }
    let project = config.tracker.as_ref()?.project.clone()?;
    match guard_tracker::JiraConfig::from_env(&project, guard_tracker::DEFAULT_TOKEN_VAR)
        .and_then(guard_tracker::JiraTracker::new)
    {
        Ok(jira) => Some(Arc::new(jira)),
        Err(e) => {
            warning(format!("reporting: jira: {e}"));
            None
        }
    }
}

/// Uploads `playwright-report/`; returns the presigned `index.html`.
fn upload_html_report(
    ctx: &Context,
    store: &dyn EvidenceStore,
    repo: &str,
    run_id: &str,
    ttl: std::time::Duration,
) -> Option<String> {
    let dir = ctx.e2e_dir.join("playwright-report");
    if !dir.join("index.html").exists() {
        return None;
    }
    let Some(files) = evidence::report_files(&dir) else {
        warning("evidence: playwright-report/ is over 500 files or 200 MiB, not uploaded");
        return None;
    };
    let prefix = format!("{repo}/{run_id}/report");
    let ok = block_on(async {
        for (rel, path) in &files {
            let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
            store
                .put_object(
                    &format!("{prefix}/{rel}"),
                    bytes,
                    evidence::content_type_of(rel),
                )
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok::<(), String>(())
    });
    match ok {
        Ok(()) => store.presign_get(&format!("{prefix}/index.html"), ttl).ok(),
        Err(e) => {
            warning(format!("evidence: HTML report upload failed: {e}"));
            None
        }
    }
}

/// Step 9: Jira (server or direct), PR comment, status. Never fails.
#[allow(clippy::too_many_arguments)]
fn publish(
    config: &TargetConfig,
    report: &GateReport,
    server: Option<&ServerClient>,
    jira: Option<&dyn IssueTracker>,
    code_host: Option<&dyn CodeHost>,
    pr_number: Option<u64>,
    screenshots: LocalScreenshots,
    outcome: &mut ReportOutcome,
) -> Reported {
    let mut reported = Reported::default();
    if let Some(client) = server {
        match block_on(client.gate_result(report)) {
            Ok(answer) => {
                *outcome = answer.outcome;
                reported.jira_line = Some(jira_line(outcome));
            }
            Err(e) => warning(format!("reporting: server: {e}")),
        }
    } else if let Some(jira) = jira {
        let options =
            crate::commands::serve::report_options(config).expect("direct_jira requires a project");
        *outcome = block_on(guard_tracker::report::report_gate(
            jira,
            &screenshots,
            report,
            &options,
        ));
        reported.jira_line = Some(jira_line(outcome));
    } else {
        reported.jira_line = Some("jira: not configured".into());
    }
    for w in &outcome.warnings {
        warning(format!("reporting: {w}"));
    }

    if let Some(host) = code_host {
        let mut parts = Vec::new();
        if let Some(n) = pr_number {
            let body = guard_codehost::report::render_pr_comment(report, outcome);
            match block_on(host.comment_pr(PrRef { number: n }, &body)) {
                Ok(_) => {
                    outcome.pr_comment = true;
                    parts.push("comment updated".to_string());
                }
                Err(e) => warning(format!("reporting: pr comment: {e}")),
            }
        }
        if !report.head_sha.is_empty() {
            let status = guard_codehost::report::gate_status(report, None);
            match block_on(host.publish_status(&status)) {
                Ok(()) => {
                    outcome.status_published = true;
                    let state = serde_json::to_value(status.state)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default();
                    parts.push(format!("status {}={state}", status.name));
                }
                Err(e) => warning(format!("reporting: status: {e}")),
            }
        }
        if !parts.is_empty() {
            reported.pr_line = Some(format!("pr: {}", parts.join(", ")));
        }
    }
    reported
}

fn jira_line(outcome: &ReportOutcome) -> String {
    let created = outcome
        .bugs
        .iter()
        .filter(|b| b.action == BugAction::Created)
        .count();
    let commented = outcome.bugs.len() - created;
    let mut line = format!("jira: {created} created, {commented} commented");
    if let Some(key) = &outcome.ticket_comment {
        line.push_str(&format!(", summary on {key}"));
    }
    line
}

/// Exit 3 of the main run: the `guard` status says infrastructure error.
fn publish_infrastructure(
    config: &TargetConfig,
    host: Option<&dyn CodeHost>,
    run_id: &str,
    head_sha: &str,
    error: &CliError,
) {
    let Some(host) = host else {
        return;
    };
    if head_sha.is_empty() {
        return;
    }
    let report = GateReport {
        version: 2,
        run_id: run_id.to_string(),
        generated_at: rfc3339_utc(unix_now()),
        provider: discover::provider(config),
        repo: None,
        pr: None,
        ticket_key: None,
        ticket_is_flow_change: false,
        head_sha: head_sha.to_string(),
        base_sha: None,
        verdict: GateVerdict::Red,
        counts: GateCounts::default(),
        tests: Vec::new(),
        journey_changes: Vec::new(),
        change_flags: Vec::new(),
        report_url: None,
        ci_url: discover::ci_url(&discover::process_env),
    };
    let reason = error.messages.join("; ");
    let status = guard_codehost::report::gate_status(&report, Some(&reason));
    if let Err(e) = block_on(host.publish_status(&status)) {
        warning(format!("reporting: status: {e}"));
    }
}

fn label(test: &GateTest) -> &str {
    test.journey.as_deref().unwrap_or(&test.title)
}

/// The stdout summary (finalize-phase2.md, "stdout").
pub fn summary(document: &VerdictDocument, report: &GateReport, outcome: &ReportOutcome) -> String {
    let c = &report.counts;
    let mut out = format!(
        "verdict: {}  (passed {}, flaky {}, failed {}, skipped {}; regressions {}, pre-existing {})\n",
        document.verdict.as_str(),
        c.passed,
        c.flaky,
        c.failed,
        c.skipped,
        c.regressions,
        c.pre_existing
    );
    for test in report
        .tests
        .iter()
        .filter(|t| t.status == TestStatus::Failed)
    {
        let prefix = match test.baseline {
            Some(BaselineOutcome::Regression) => "REGRESSION",
            Some(BaselineOutcome::PreExisting) => "PRE-EXISTING",
            Some(BaselineOutcome::Flaky) => "FLAKY (base)",
            Some(BaselineOutcome::Unavailable) => "UNAVAILABLE",
            None => "FAILED",
        };
        let mut line = format!("{prefix:<12} {}:{}  {}", test.file, test.line, label(test));
        if let Some(fp) = &test.fingerprint {
            line.push_str(&format!("  {}", fp.label()));
            if let Some(bug) = outcome.bugs.iter().find(|b| &b.fingerprint == fp) {
                let action = match bug.action {
                    BugAction::Created => "created",
                    BugAction::Commented => "commented",
                };
                line.push_str(&format!("  {} ({action})", bug.key));
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    for test in report
        .tests
        .iter()
        .filter(|t| t.status == TestStatus::Flaky)
    {
        let prefix = if !test.blocking {
            "FLAKY".to_string()
        } else if test.tags.iter().any(|t| t == SMOKE_TAG) {
            "FLAKY (blocking: smoke)".to_string()
        } else {
            "FLAKY (blocking: path)".to_string()
        };
        out.push_str(&format!(
            "{prefix}  {}:{}  {}\n",
            test.file,
            test.line,
            label(test)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::catalog::parse_catalog;

    fn outcome(status: Classification, tags: &[&str], journey: Option<&str>) -> TestOutcome {
        TestOutcome {
            title: "t".into(),
            file: "tests/a.spec.ts".into(),
            line: 1,
            project: "chromium".into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            journey: journey.map(str::to_string),
            status,
            blocking: false,
            attempts: 3,
            duration_ms: 1,
            error: Some("boom".into()),
        }
    }

    #[test]
    fn path_selection_ignores_smoke_only_journeys() {
        let catalog = parse_catalog(
            r#"{"version": 1, "journeys": [
              {"id": "a.b", "area": "a", "title": "t", "impact_question": "q", "paths": ["src/a/**"], "smoke": true, "quarantined": false},
              {"id": "c.d", "area": "a", "title": "t", "impact_question": "q", "paths": ["src/c/**"], "smoke": true, "quarantined": false}
            ]}"#,
        )
        .unwrap();
        let selected = path_selected(&catalog, &["src/a/x.ts".to_string()]);
        assert_eq!(selected, BTreeSet::from(["a.b".to_string()]));
    }

    #[test]
    fn blocking_follows_the_phase_two_rule() {
        let by_path = BTreeSet::from(["a.b".to_string()]);
        let failed = outcome(Classification::Failed, &[], Some("x.y"));
        let pre = blocking_facts(&failed, Some(BaselineOutcome::PreExisting), Some(&by_path));
        assert!(!is_blocking(pre));
        assert!(is_blocking(blocking_facts(&failed, None, Some(&by_path))));
        assert!(is_blocking(blocking_facts(
            &failed,
            Some(BaselineOutcome::Regression),
            Some(&by_path)
        )));
        let flaky_path = outcome(Classification::Flaky, &[], Some("a.b"));
        assert!(is_blocking(blocking_facts(
            &flaky_path,
            None,
            Some(&by_path)
        )));
        let flaky_other = outcome(Classification::Flaky, &[], Some("x.y"));
        assert!(!is_blocking(blocking_facts(
            &flaky_other,
            None,
            Some(&by_path)
        )));
        let flaky_smoke = outcome(Classification::Flaky, &["@smoke"], None);
        assert!(is_blocking(blocking_facts(
            &flaky_smoke,
            None,
            Some(&by_path)
        )));
    }

    fn change(id: &str) -> JourneyChange {
        JourneyChange {
            id: id.into(),
            file: format!("e2e/tests/journeys/{id}.spec.ts"),
            assertions_before: 5,
            assertions_after: 3,
            skips_added: 0,
        }
    }

    #[test]
    fn journey_flags_are_split_into_global_and_per_journey_lists() {
        let flags = vec![
            "journeys modified, but the ticket is not labeled flow-change".to_string(),
            "a.b: assertions 5 -> 3".to_string(),
            "a.bc: 1 skip/fixme added".to_string(),
        ];
        let (global, reports) = split_flags(&[change("a.b"), change("a.bc")], flags);
        assert_eq!(
            global,
            vec!["journeys modified, but the ticket is not labeled flow-change"]
        );
        assert_eq!(reports[0].journey, "a.b");
        assert_eq!(reports[0].flags, vec!["assertions 5 -> 3"]);
        assert_eq!(reports[1].flags, vec!["1 skip/fixme added"]);
    }

    #[test]
    fn rendered_reports_show_each_journey_flag_once() {
        let flags = vec!["a.b: assertions 5 -> 3".to_string()];
        let (global, reports) = split_flags(&[change("a.b")], flags);
        let lines = journey_lines(&global, &reports);
        assert_eq!(lines, vec!["journeys: a.b: assertions 5 -> 3"]);
    }

    #[test]
    fn changed_files_are_unknown_only_when_they_cannot_be_determined() {
        let ok = |_: &str| Ok(vec!["src/a.ts".to_string()]);
        let err = |_: &str| Err("git failed".to_string());
        let same = |files: &[String]| files.to_vec();
        assert_eq!(
            changed_files_of(None, Some("b"), true, ok, same),
            Some(vec!["src/a.ts".to_string()])
        );
        assert_eq!(changed_files_of(None, Some("b"), false, err, same), None);
        // A PR run whose PR context failed and with no base: unknown.
        assert_eq!(changed_files_of(None, None, true, ok, same), None);
        // No PR and no base (a local run): nothing changed.
        assert_eq!(
            changed_files_of(None, None, false, ok, same),
            Some(Vec::new())
        );
    }

    #[test]
    fn guard_base_is_its_merge_base_with_head_or_kept_for_the_warning() {
        let run = |dir: &std::path::Path, args: &[&str]| {
            let mut all = vec!["-c", "user.name=t", "-c", "user.email=t@t"];
            all.extend_from_slice(args);
            let out = git::git_in(dir, &all).unwrap();
            String::from_utf8_lossy(&out).trim().to_string()
        };
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        run(root, &["init", "-q", "-b", "develop"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);
        run(root, &["checkout", "-q", "-b", "feature"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "work"]);
        run(root, &["checkout", "-q", "develop"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "later"]);
        run(root, &["checkout", "-q", "feature"]);
        assert_eq!(guard_base_sha(root, "develop"), Some(base));
        // Not a commit: kept, so the caller warns and runs without a base.
        assert_eq!(guard_base_sha(root, "nope"), Some("nope".to_string()));
        // HEAD's own branch: no base, like the final fallback.
        assert_eq!(guard_base_sha(root, "feature"), None);
    }

    #[test]
    fn final_fallback_uses_detected_default_not_origin_head() {
        let run = |dir: &std::path::Path, args: &[&str]| {
            let mut all = vec!["-c", "user.name=t", "-c", "user.email=t@t"];
            all.extend_from_slice(args);
            let out = git::git_in(dir, &all).unwrap();
            String::from_utf8_lossy(&out).trim().to_string()
        };
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        run(root, &["init", "-q", "-b", "dev"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let base = run(root, &["rev-parse", "HEAD"]);
        run(
            root,
            &["remote", "add", "upstream", "https://example.invalid/u.git"],
        );
        run(root, &["update-ref", "refs/remotes/upstream/dev", "HEAD"]);
        run(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/upstream/HEAD",
                "refs/remotes/upstream/dev",
            ],
        );
        run(root, &["checkout", "-q", "-b", "feature"]);
        run(root, &["branch", "-q", "-D", "dev"]);
        run(root, &["commit", "-q", "--allow-empty", "-m", "work"]);
        assert_eq!(fallback_base_sha(root), Some(base));
        // On the default branch itself the merge base is HEAD: no base.
        run(root, &["update-ref", "refs/remotes/upstream/dev", "HEAD"]);
        assert_eq!(fallback_base_sha(root), None);
        let lone = tempfile::TempDir::new().unwrap();
        run(lone.path(), &["init", "-q", "-b", "feature"]);
        run(lone.path(), &["config", "init.defaultBranch", "main"]);
        run(lone.path(), &["commit", "-q", "--allow-empty", "-m", "x"]);
        assert_eq!(fallback_base_sha(lone.path()), None);
    }

    #[test]
    fn pr_changed_files_gitlink_is_expanded_locally() {
        let pr = PrContext {
            provider: guard_core::ports::CodeHostProvider::Github,
            repo: "acme/shop".into(),
            number: 1,
            title: "t".into(),
            url: "u".into(),
            author: "dev".into(),
            ticket_key: None,
            branch: "b".into(),
            base_branch: "main".into(),
            base_sha: "b".into(),
            head_sha: "h".into(),
            changed_files: vec!["frontend".into(), "web/a.ts".into()],
        };
        let unused = |_: &str| -> Result<Vec<String>, String> { panic!("no diff with a PR") };
        let expand = |files: &[String]| -> Vec<String> {
            files
                .iter()
                .map(|f| {
                    if f == "frontend" {
                        "frontend/src/login.tsx".to_string()
                    } else {
                        f.clone()
                    }
                })
                .collect()
        };
        assert_eq!(
            changed_files_of(Some(&pr), Some("b"), true, unused, expand),
            Some(vec![
                "frontend/src/login.tsx".to_string(),
                "web/a.ts".to_string()
            ])
        );
    }

    #[test]
    fn unknown_changed_files_make_every_flaky_test_block() {
        let flaky = outcome(Classification::Flaky, &[], Some("x.y"));
        assert!(is_blocking(blocking_facts(&flaky, None, None)));
        let no_journey = outcome(Classification::Flaky, &[], None);
        assert!(is_blocking(blocking_facts(&no_journey, None, None)));
        assert!(CHANGED_FILES_UNKNOWN_FLAG.starts_with("changed files unknown"));
    }

    #[test]
    fn missing_ticket_flags_name_the_mode() {
        assert_eq!(
            missing_ticket_flag(RequireTicket::Block),
            "PR has no ticket (require_ticket = block)"
        );
    }

    #[test]
    fn jira_lines_count_actions() {
        let fp = Fingerprint("1a2b3c4d".into());
        let outcome = ReportOutcome {
            bugs: vec![guard_core::ports::ReportedBug {
                fingerprint: fp,
                key: "SHOP-1".into(),
                url: "u".into(),
                action: BugAction::Created,
            }],
            ticket_comment: Some("SHOP-9".into()),
            ..ReportOutcome::default()
        };
        assert_eq!(
            jira_line(&outcome),
            "jira: 1 created, 0 commented, summary on SHOP-9"
        );
    }
}
