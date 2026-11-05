//! The baseline run of `guard gate finalize` (finalize-phase2.md, "Baseline
//! run"): tests that failed 3/3 on the PR head are re-run against the base
//! commit, app and tests at base, in a second worktree.

use crate::context::{Context, slash};
use crate::error::warning;
use crate::git;
use crate::http;
use crate::playwright;
use crate::process;
use guard_core::baseline::{BaseRun, MAX_BASELINE_TESTS, classify};
use guard_core::config::TargetConfig;
use guard_core::ports::BaselineOutcome;
use guard_core::runner_env::{RunnerEnvInput, runner_env};
use guard_core::verdict::{Classification, TestOutcome, parse_report};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The whole baseline (install, `guard up`, every project run) gets this long.
pub const BASELINE_BUDGET: Duration = Duration::from_secs(15 * 60);

/// One failed test to re-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub test_id: String,
    pub file: String,
    pub title: String,
    pub project: String,
}

/// How a base-commit result maps to [`BaseRun`]: absent or skipped = missing.
pub fn base_run(found: Option<Classification>) -> BaseRun {
    match found {
        None | Some(Classification::Skipped) => BaseRun::Missing,
        Some(Classification::Passed) => BaseRun::Passed,
        Some(Classification::Failed) => BaseRun::Failed,
        Some(Classification::Flaky) => BaseRun::Flaky,
    }
}

/// Maps the base report of one project onto the targets of that project.
pub fn map_results(targets: &[&Target], base: &[TestOutcome]) -> BTreeMap<String, BaseRun> {
    targets
        .iter()
        .map(|t| {
            let found = base
                .iter()
                .find(|b| b.file == t.file && b.title == t.title && b.project == t.project)
                .map(|b| b.status);
            (t.test_id.clone(), base_run(found))
        })
        .collect()
}

/// Every target past [`MAX_BASELINE_TESTS`] is `Unavailable` without running.
pub fn split_cap(targets: &[Target]) -> (&[Target], &[Target]) {
    targets.split_at(targets.len().min(MAX_BASELINE_TESTS))
}

/// Executable names to try for a Node tool: `pnpm` resolves `pnpm.exe` on
/// Windows, the `.cmd` shim is the fallback (npm installs).
fn programs(name: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![name.to_string(), format!("{name}.cmd")]
    } else {
        vec![name.to_string()]
    }
}

/// `pnpm install --frozen-lockfile` at the root when `pnpm-lock.yaml` is there,
/// else `npm ci` in `e2e-playwright/` when it has a `package-lock.json`.
/// `Ok(false)` when there is nothing to install. The install counts against
/// the baseline deadline; its output goes to `<worktree>.install.log`.
fn install(worktree: &Path, deadline: Instant) -> Result<bool, String> {
    let (dir, candidates, args): (PathBuf, Vec<String>, Vec<&str>) =
        if worktree.join("pnpm-lock.yaml").exists() {
            (
                worktree.to_path_buf(),
                std::env::var("GUARD_PNPM")
                    .map(|p| vec![p])
                    .unwrap_or_else(|_| programs("pnpm")),
                vec!["install", "--frozen-lockfile"],
            )
        } else if worktree.join("e2e-playwright/package-lock.json").exists() {
            (worktree.join("e2e-playwright"), programs("npm"), vec!["ci"])
        } else {
            return Ok(false);
        };
    let log = worktree.with_extension("install.log");
    run_install(&dir, &candidates, &args, &log, deadline).map(|()| true)
}

/// Runs the first candidate that exists with `args` in `dir`, output to `log`,
/// killing its process tree at `deadline`.
fn run_install(
    dir: &Path,
    candidates: &[String],
    args: &[&str],
    log: &Path,
    deadline: Instant,
) -> Result<(), String> {
    let open_log =
        || std::fs::File::create(log).map_err(|e| format!("cannot write {}: {e}", slash(log)));
    for candidate in candidates {
        let out = open_log()?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut child = match Command::new(candidate)
            .args(args)
            .current_dir(dir)
            .env("CI", "1")
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
        {
            Ok(child) => child,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("cannot run {candidate}: {e}")),
        };
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if Instant::now() >= deadline {
                process::kill_tree(child.id());
                let _ = child.wait();
                return Err(format!(
                    "{candidate} {} timed out (baseline budget)",
                    args.join(" ")
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if status.success() {
            return Ok(());
        }
        let text = std::fs::read_to_string(log).unwrap_or_default();
        return Err(format!(
            "{candidate} {} failed: {}",
            args.join(" "),
            process::tail(&text, 5).join(" | ")
        ));
    }
    Err(format!("cannot run {}: not found", candidates.join(" or ")))
}

/// The first top-level error of a base report, ignoring Playwright's
/// "No tests found" (a test absent at base is a regression). A config or
/// compile error at base means the run proves nothing: infrastructure.
fn base_report_error(report: &guard_core::verdict::PwReport) -> Option<String> {
    report
        .errors
        .iter()
        .map(|e| {
            e.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string())
        })
        .find(|m| !m.contains("No tests found"))
        .map(|m| m.lines().next().unwrap_or_default().to_string())
}

/// Runs `current_exe() <args>` in `dir`; the stdout goes to our stderr.
fn guard_self(dir: &Path, args: &[&str], timeout: Duration) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::from(std::io::stderr()));
    match playwright::wait_with_deadline(cmd, Instant::now() + timeout)? {
        Some(0) => Ok(()),
        other => Err(format!("guard {} exited with {other:?}", args.join(" "))),
    }
}

/// The baseline outcome of every target.
pub fn run(
    ctx: &Context,
    config: &TargetConfig,
    base_sha: &str,
    run_id: &str,
    targets: &[Target],
) -> BTreeMap<String, BaselineOutcome> {
    let (to_run, capped) = split_cap(targets);
    let mut outcomes: BTreeMap<String, BaselineOutcome> = capped
        .iter()
        .map(|t| (t.test_id.clone(), BaselineOutcome::Unavailable))
        .collect();
    if to_run.is_empty() {
        return outcomes;
    }
    if !capped.is_empty() {
        warning(format!(
            "baseline: only the first {MAX_BASELINE_TESTS} failed tests are re-run; {} stay unavailable",
            capped.len()
        ));
    }
    let infrastructure = |outcomes: &mut BTreeMap<String, BaselineOutcome>, reason: &str| {
        warning(format!("baseline: {reason}"));
        for t in to_run {
            outcomes
                .entry(t.test_id.clone())
                .or_insert_with(|| classify(BaseRun::Infrastructure));
        }
    };

    // Never test the wrong app: something already answers on base_url.
    if http::is_ready(&config.ready_url(), Duration::from_secs(2)).is_ok() {
        infrastructure(
            &mut outcomes,
            &format!(
                "{} is already answering; another app is running",
                config.ready_url()
            ),
        );
        return outcomes;
    }

    let worktree = ctx.guard_dir().join("baseline");
    if let Err(reason) = prepare_worktree(&ctx.root, &worktree, base_sha, &[]) {
        infrastructure(&mut outcomes, &reason);
        return outcomes;
    }
    let deadline = Instant::now() + BASELINE_BUDGET;
    let result = run_in_worktree(ctx, config, &worktree, run_id, to_run, deadline);
    let _ = guard_self(&worktree, &["down"], Duration::from_secs(120));
    git::worktree_remove(&ctx.root, &worktree);
    let _ = std::fs::remove_dir_all(&worktree);

    match result {
        Ok(mapped) => {
            for (id, run) in mapped {
                outcomes.insert(id, classify(run));
            }
            for t in to_run {
                outcomes
                    .entry(t.test_id.clone())
                    .or_insert(BaselineOutcome::Unavailable);
            }
        }
        Err((partial, reason)) => {
            for (id, run) in partial {
                outcomes.insert(id, classify(run));
            }
            infrastructure(&mut outcomes, &reason);
        }
    }
    outcomes
}

/// A fresh detached worktree at `base_sha`; when `.gitmodules` exists at
/// the base, its submodules are initialized too (finalize-phase2.md). On
/// failure the worktree is removed again. `git_options` go before the
/// `submodule` command (tests allow the local `file` transport there).
fn prepare_worktree(
    root: &Path,
    worktree: &Path,
    base_sha: &str,
    git_options: &[&str],
) -> Result<(), String> {
    git::worktree_remove(root, worktree);
    let _ = std::fs::remove_dir_all(worktree);
    git::worktree_add(root, worktree, base_sha)
        .map_err(|e| format!("git worktree add failed: {e}"))?;
    if git::show_file(root, base_sha, ".gitmodules").is_some() {
        let mut args = git_options.to_vec();
        args.extend(["submodule", "update", "--init", "--recursive"]);
        if let Err(e) = git::git_in(worktree, &args) {
            git::worktree_remove(root, worktree);
            let _ = std::fs::remove_dir_all(worktree);
            return Err(format!("git submodule update failed: {e}"));
        }
    }
    Ok(())
}

type Partial = (BTreeMap<String, BaseRun>, String);

fn run_in_worktree(
    ctx: &Context,
    config: &TargetConfig,
    worktree: &Path,
    run_id: &str,
    targets: &[Target],
    deadline: Instant,
) -> Result<BTreeMap<String, BaseRun>, Partial> {
    let mut mapped = BTreeMap::new();
    let fail = |mapped: &BTreeMap<String, BaseRun>, reason: String| Err((mapped.clone(), reason));
    if let Err(e) = install(worktree, deadline) {
        return fail(&mapped, e);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if let Err(e) = guard_self(worktree, &["up"], remaining) {
        return fail(&mapped, format!("guard up at base failed: {e}"));
    }

    // The base worktree's own target.toml drives the runner env.
    let base_ctx = Context::at(worktree);
    let base_config = base_ctx.load_config().unwrap_or_else(|_| config.clone());
    let token = std::env::var(&base_config.testability.token_env)
        .ok()
        .filter(|v| !v.is_empty());
    let e2e = worktree.join(
        ctx.e2e_dir
            .strip_prefix(&ctx.root)
            .unwrap_or(Path::new("e2e-playwright")),
    );

    let projects: BTreeSet<&str> = targets.iter().map(|t| t.project.as_str()).collect();
    for project in projects {
        let of_project: Vec<&Target> = targets.iter().filter(|t| t.project == project).collect();
        let files: BTreeSet<&str> = of_project.iter().map(|t| t.file.as_str()).collect();
        let out = ctx.guard_dir().join(format!("baseline-{project}.json"));
        let _ = std::fs::remove_file(&out);
        let mut env: Vec<(String, String)> = runner_env(&RunnerEnvInput {
            config: &base_config,
            e2e_tags: "",
            run_id,
            token: token.as_deref(),
            docker: false,
        })
        .into_iter()
        .filter(|(k, _)| k != "E2E_TAGS" && k != "GUARD_VIDEO")
        .collect();
        env.push(("CI".into(), "1".into()));
        env.push(("GUARD_VIDEO".into(), "off".into()));
        env.push((
            "PLAYWRIGHT_JSON_OUTPUT_NAME".into(),
            out.to_string_lossy().into_owned(),
        ));
        let mut args = vec!["playwright".to_string(), "test".to_string()];
        args.extend(files.iter().map(|f| f.to_string()));
        args.push(format!("--project={project}"));
        args.push("--retries=2".into());
        args.push("--reporter=json".into());

        let mut cmd = Command::new(playwright::npx_program());
        cmd.args(&args)
            .current_dir(&e2e)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .env_remove("E2E_TAGS")
            .env_remove("GUARD_SHARD_PLAN")
            .env_remove("GUARD_SHARD_INDEX")
            .stdout(std::process::Stdio::from(std::io::stderr()));
        let exit = match playwright::wait_with_deadline(cmd, deadline) {
            Ok(exit) => exit,
            Err(e) => return fail(&mapped, format!("playwright at base: {e}")),
        };
        if !matches!(exit, Some(0 | 1)) {
            return fail(
                &mapped,
                format!("playwright at base exited with {exit:?} (project {project})"),
            );
        }
        let report = std::fs::read_to_string(&out)
            .ok()
            .and_then(|t| parse_report(&t).ok());
        let Some(report) = report else {
            return fail(&mapped, format!("no JSON report at {}", slash(&out)));
        };
        if let Some(error) = base_report_error(&report) {
            return fail(
                &mapped,
                format!("playwright at base reported an error (project {project}): {error}"),
            );
        }
        let base_tests = guard_core::verdict::classify(&report);
        mapped.extend(map_results(&of_project, &base_tests));
    }
    Ok(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git with a fixed identity and local file transport for submodules.
    fn g(dir: &Path, args: &[&str]) -> String {
        let mut all = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ];
        all.extend_from_slice(args);
        let out = git::git_in(dir, &all).unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        String::from_utf8_lossy(&out).trim().to_string()
    }

    const FILE_TRANSPORT: &[&str] = &["-c", "protocol.file.allow=always"];

    #[test]
    fn baseline_worktree_initializes_submodules_or_marks_unavailable() {
        let source = tempfile::TempDir::new().unwrap();
        g(source.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(
            source.path().join("login.tsx"),
            "x
",
        )
        .unwrap();
        g(source.path(), &["add", "-A"]);
        g(source.path(), &["commit", "-q", "-m", "init"]);
        let top = tempfile::TempDir::new().unwrap();
        let root = top.path();
        g(root, &["init", "-q", "-b", "main"]);
        let url = source.path().to_string_lossy().replace('\\', "/");
        g(root, &["submodule", "add", "-q", &url, "frontend"]);
        g(root, &["commit", "-q", "-m", "umbrella"]);
        let base = g(root, &["rev-parse", "HEAD"]);
        let worktree = root.join(".guard").join("baseline");
        prepare_worktree(root, &worktree, &base, FILE_TRANSPORT).unwrap();
        assert!(worktree.join("frontend/login.tsx").is_file());
        git::worktree_remove(root, &worktree);
        assert!(!worktree.exists(), "a worktree with submodules is removed");

        // A submodule that cannot be fetched: the baseline is unavailable.
        g(
            root,
            &[
                "config",
                "-f",
                ".gitmodules",
                "submodule.frontend.url",
                "./gone",
            ],
        );
        g(root, &["commit", "-q", "-am", "broken url"]);
        g(root, &["submodule", "sync", "-q"]);
        g(root, &["config", "--remove-section", "submodule.frontend"]);
        let base = g(root, &["rev-parse", "HEAD"]);
        let error = prepare_worktree(root, &worktree, &base, FILE_TRANSPORT).unwrap_err();
        assert!(error.contains("submodule"), "{error}");
        assert!(!worktree.exists());
    }

    fn target(id: &str) -> Target {
        Target {
            test_id: id.into(),
            file: format!("tests/{id}.spec.ts"),
            title: id.into(),
            project: "chromium".into(),
        }
    }

    fn outcome(id: &str, status: Classification) -> TestOutcome {
        TestOutcome {
            title: id.into(),
            file: format!("tests/{id}.spec.ts"),
            line: 1,
            project: "chromium".into(),
            tags: vec![],
            journey: None,
            status,
            blocking: false,
            attempts: 1,
            duration_ms: 1,
            error: None,
        }
    }

    #[test]
    fn base_results_map_by_file_title_and_project() {
        let targets = [
            target("a"),
            target("b"),
            target("c"),
            target("d"),
            target("e"),
        ];
        let refs: Vec<&Target> = targets.iter().collect();
        let base = vec![
            outcome("a", Classification::Passed),
            outcome("b", Classification::Failed),
            outcome("c", Classification::Flaky),
            outcome("d", Classification::Skipped),
        ];
        let mapped = map_results(&refs, &base);
        assert_eq!(mapped["a"], BaseRun::Passed);
        assert_eq!(mapped["b"], BaseRun::Failed);
        assert_eq!(mapped["c"], BaseRun::Flaky);
        assert_eq!(mapped["d"], BaseRun::Missing);
        assert_eq!(mapped["e"], BaseRun::Missing);
        // Another project with the same file and title does not count.
        let mut other = outcome("a", Classification::Failed);
        other.project = "firefox".into();
        let mapped = map_results(&refs[..1], &[other]);
        assert_eq!(mapped["a"], BaseRun::Missing);
    }

    #[test]
    fn base_reports_with_config_errors_are_infrastructure() {
        let compile = parse_report(
            r#"{"suites": [], "errors": [{"message": "SyntaxError: Unexpected token in playwright.config.ts"}]}"#,
        )
        .unwrap();
        assert!(base_report_error(&compile).unwrap().contains("SyntaxError"));
        // A test file that does not exist at base is a regression, not an error.
        let missing =
            parse_report(r#"{"suites": [], "errors": [{"message": "Error: No tests found"}]}"#)
                .unwrap();
        assert_eq!(base_report_error(&missing), None);
        let clean = parse_report(r#"{"suites": [], "errors": []}"#).unwrap();
        assert_eq!(base_report_error(&clean), None);
    }

    fn sleeper() -> (String, Vec<&'static str>) {
        if cfg!(windows) {
            ("ping".into(), vec!["-n", "30", "127.0.0.1"])
        } else {
            ("sleep".into(), vec!["30"])
        }
    }

    #[test]
    fn a_hung_install_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let (prog, args) = sleeper();
        let started = Instant::now();
        let err = run_install(
            dir.path(),
            &[prog],
            &args,
            &dir.path().join("install.log"),
            Instant::now() + Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(20));
    }

    #[test]
    fn install_falls_back_to_the_next_candidate_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("install.log");
        let deadline = Instant::now() + Duration::from_secs(60);
        let candidates = ["guard-no-such-program-xyz".to_string(), "git".to_string()];
        run_install(dir.path(), &candidates, &["--version"], &log, deadline).unwrap();
        let err = run_install(
            dir.path(),
            &["git".to_string()],
            &["no-such-subcommand-xyz"],
            &log,
            deadline,
        )
        .unwrap_err();
        assert!(
            err.starts_with("git no-such-subcommand-xyz failed"),
            "{err}"
        );
        let err = run_install(
            dir.path(),
            &["guard-no-such-program-xyz".to_string()],
            &[],
            &log,
            deadline,
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn only_the_first_twenty_failures_are_re_run() {
        let targets: Vec<Target> = (0..23).map(|i| target(&format!("t{i}"))).collect();
        let (run, capped) = split_cap(&targets);
        assert_eq!(run.len(), 20);
        assert_eq!(capped.len(), 3);
        assert_eq!(capped[0].test_id, "t20");
    }
}
