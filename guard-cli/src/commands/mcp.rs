//! `guard mcp`: the MCP stdio server (mcp-tools.md) backed by this binary.
//!
//! stdout carries JSON-RPC, so every run happens in a `current_exe()`
//! subprocess whose stdout is captured (never a user shell), and the files it
//! writes (`results/report.json`, the repro result) are read back.

use super::finalize::evidence;
use crate::context::{Context, slash};
use crate::server_client::{ServerClient, ServerError};
use guard_core::catalog::Catalog;
use guard_core::fingerprint::{fingerprint, test_id};
use guard_core::ports::{BaselineOutcome, EvidenceKind, TestStatus};
use guard_core::verdict::{Classification, classify, parse_report};
use guard_mcp::types::{
    AffectedOutput, AffectedRequest, Attempt, BaselineClass, FailureOutput, FailureRequest,
    FailureSource, FailureStatus, FailureSummary, ReproOutput, ReproRequest, TestCounts,
    TestOutput, TestRequest, TestSelection,
};
use guard_mcp::{GuardActions, ToolError};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

/// The tool implementations, rooted at the repository `guard mcp` started in.
pub struct CliActions {
    exe: PathBuf,
    root: PathBuf,
    config_path: PathBuf,
    e2e_dir: PathBuf,
}

/// Output of one `guard` subprocess.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

impl CliActions {
    pub fn new(ctx: &Context) -> Result<Self, String> {
        Ok(Self {
            exe: std::env::current_exe().map_err(|e| e.to_string())?,
            root: ctx.root.clone(),
            config_path: ctx.config_path.clone(),
            e2e_dir: ctx.e2e_dir.clone(),
        })
    }

    fn ctx(&self) -> Context {
        Context {
            root: self.root.clone(),
            in_git: true,
            config_path: self.config_path.clone(),
            e2e_dir: self.e2e_dir.clone(),
        }
    }

    async fn guard(&self, args: &[String], extra_env: &[(&str, &str)]) -> Result<Ran, ToolError> {
        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.arg("--config")
            .arg(&self.config_path)
            .arg("--e2e-dir")
            .arg(&self.e2e_dir)
            .args(args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        // Own process group, so a timeout (the future is dropped) can kill
        // Playwright and its browsers too, not only the guard child.
        #[cfg(unix)]
        cmd.process_group(0);
        #[cfg(windows)]
        {
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let child = cmd
            .spawn()
            .map_err(|e| ToolError::environment(format!("cannot run guard: {e}")))?;
        let mut tree = child.id().map(TreeKill::new);
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| ToolError::environment(format!("cannot run guard: {e}")))?;
        if let Some(tree) = tree.as_mut() {
            tree.disarm();
        }
        Ok(Ran {
            code: output.status.code().unwrap_or(3),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Kills the process tree of `pid` when dropped, unless disarmed: a tool call
/// dropped by the long-run timeout takes the grandchildren (Playwright, the
/// browsers, a test server holding the app port) with it.
struct TreeKill(Option<u32>);

impl TreeKill {
    fn new(pid: u32) -> Self {
        Self(Some(pid))
    }

    /// The process exited on its own: nothing to kill.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TreeKill {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            crate::process::kill_tree(pid);
        }
    }
}

/// Exit 2 / 3 (anything but 0 and 1) as a tool error.
fn failed(ran: &Ran) -> Option<ToolError> {
    let detail = ran
        .stderr
        .lines()
        .filter(|l| l.starts_with("error:") || l.starts_with("hint:"))
        .collect::<Vec<_>>()
        .join("\n");
    let detail = if detail.is_empty() {
        ran.stderr.trim().to_string()
    } else {
        detail
    };
    match ran.code {
        0 | 1 => None,
        2 => Some(ToolError::config(detail)),
        code => Some(ToolError::environment(detail).with_exit_code(code)),
    }
}

/// The tag lines `guard test` prints before running (`@tag   reason`).
pub fn printed_tags(stdout: &str) -> Vec<String> {
    let mut tags: Vec<String> = stdout
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| t.starts_with('@'))
        .map(str::to_string)
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn last_failing(test: &Value) -> Option<&Value> {
    test["results"]
        .as_array()?
        .iter()
        .rev()
        .find(|r| !matches!(r["status"].as_str(), Some("passed") | Some("skipped")))
}

/// Titles of the top-level steps up to (and including) the failing one.
pub fn steps(result: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for step in result["steps"].as_array().into_iter().flatten() {
        out.push(step["title"].as_str().unwrap_or_default().to_string());
        if !step["error"].is_null() {
            break;
        }
    }
    out
}

/// Repo-relative path of a local evidence file of `kind`, if any.
fn local_evidence(test: &Value, e2e_dir: &Path, root: &Path, kind: EvidenceKind) -> Option<String> {
    evidence::local_files(test, e2e_dir)
        .into_iter()
        .rev()
        .find(|f| f.kind == kind)
        .map(|f| slash(f.path.strip_prefix(root).unwrap_or(&f.path)))
}

fn baseline_class(b: BaselineOutcome) -> BaselineClass {
    match b {
        BaselineOutcome::Regression => BaselineClass::Regression,
        BaselineOutcome::PreExisting => BaselineClass::PreExisting,
        BaselineOutcome::Flaky => BaselineClass::Flaky,
        BaselineOutcome::Unavailable => BaselineClass::Unavailable,
    }
}

fn read_report(e2e_dir: &Path) -> Result<(guard_core::verdict::PwReport, Value), ToolError> {
    let path = e2e_dir.join("results").join("report.json");
    let text = std::fs::read_to_string(&path).map_err(|_| {
        ToolError::not_found(format!("no local run: {} does not exist", slash(&path)))
    })?;
    let report = parse_report(&text).map_err(|e| ToolError::environment(e.to_string()))?;
    let raw = serde_json::from_str(&text).map_err(|e| ToolError::environment(e.to_string()))?;
    Ok((report, raw))
}

/// `guard affected --json [--base <b>]`: no `--base` when it is omitted, so
/// guard detects it.
fn affected_args(request: &AffectedRequest) -> Vec<String> {
    let mut args = vec!["affected".to_string(), "--json".to_string()];
    if let Some(base) = &request.base {
        args.extend(["--base".to_string(), base.clone()]);
    }
    args
}

/// The `guard test` arguments of a `guard_test` call.
fn test_args(request: &TestRequest) -> Vec<String> {
    let mut args = vec!["test".to_string()];
    match &request.selection {
        TestSelection::Affected => {
            args.push("--affected".into());
            if let Some(base) = &request.base {
                args.extend(["--base".to_string(), base.clone()]);
            }
        }
        TestSelection::All => args.push("--all".into()),
        TestSelection::Journeys(ids) => {
            for id in ids {
                args.push("--journey".into());
                args.push(id.clone());
            }
        }
    }
    if request.docker {
        args.push("--docker".into());
    }
    args
}

fn server_tool_error(e: ServerError) -> ToolError {
    ToolError::server(e.to_string())
}

#[async_trait::async_trait]
impl GuardActions for CliActions {
    async fn affected(&self, request: AffectedRequest) -> Result<AffectedOutput, ToolError> {
        let ran = self.guard(&affected_args(&request), &[]).await?;
        if let Some(error) = failed(&ran) {
            return Err(error);
        }
        serde_json::from_str(&ran.stdout)
            .map_err(|e| ToolError::environment(format!("unexpected guard affected output: {e}")))
    }

    async fn test(&self, request: TestRequest) -> Result<TestOutput, ToolError> {
        let args = test_args(&request);
        let ran = self.guard(&args, &[]).await?;
        if let Some(error) = failed(&ran) {
            return Err(error);
        }
        let (report, raw) = read_report(&self.e2e_dir)?;
        let tests = classify(&report);
        let counts = guard_core::verdict::count(&tests);
        let raw_tests = evidence::raw_tests(&raw);
        let failures = tests
            .iter()
            .filter(|t| matches!(t.status, Classification::Failed | Classification::Flaky))
            .map(|t| {
                let raw_test = raw_tests
                    .iter()
                    .find(|r| r.file == t.file && r.title == t.title && r.project == t.project)
                    .map(|r| r.test);
                FailureSummary {
                    test_id: test_id(&t.project, &t.file, &t.title),
                    title: t.title.clone(),
                    file: t.file.clone(),
                    line: u64::from(t.line),
                    journey: t.journey.clone(),
                    status: if t.status == Classification::Failed {
                        FailureStatus::Failed
                    } else {
                        FailureStatus::Flaky
                    },
                    error: t.error.clone().unwrap_or_default(),
                    location: raw_test.and_then(|r| evidence::location(r, &self.e2e_dir)),
                    screenshot: raw_test.and_then(|r| {
                        local_evidence(r, &self.e2e_dir, &self.root, EvidenceKind::Screenshot)
                    }),
                }
            })
            .collect();
        Ok(TestOutput {
            exit_code: ran.code,
            counts: TestCounts {
                passed: counts.passed as u64,
                flaky: counts.flaky as u64,
                failed: counts.failed as u64,
                skipped: counts.skipped as u64,
            },
            duration_ms: report.duration_ms(),
            tags: printed_tags(&ran.stdout),
            failures,
        })
    }

    async fn failure(&self, request: FailureRequest) -> Result<FailureOutput, ToolError> {
        match request.pr {
            None => local_failure(&self.e2e_dir, &self.root, &request.test_id),
            Some(pr) => {
                let ctx = self.ctx();
                let config = ctx
                    .read_config()
                    .map_err(|m| ToolError::config(m.join("\n")))?;
                let client = ServerClient::from_config(&config).ok_or_else(|| {
                    ToolError::server_not_configured("target.toml has no [server]")
                })?;
                let context = client.pr_context(pr).await.map_err(server_tool_error)?;
                let gate = context
                    .latest_gate
                    .ok_or_else(|| ToolError::not_found(format!("no gate result for PR {pr}")))?;
                let test = gate
                    .report
                    .tests
                    .iter()
                    .find(|t| t.test_id == request.test_id)
                    .ok_or_else(|| ToolError::not_found(request.test_id.clone()))?;
                let bundle = test.evidence.as_ref();
                let url_of = |kind: EvidenceKind| {
                    bundle.and_then(|b| {
                        b.files.iter().rev().find(|f| f.kind == kind).and_then(|f| {
                            if kind == EvidenceKind::Trace {
                                f.viewer_url.clone().or_else(|| f.url.clone())
                            } else {
                                f.url.clone()
                            }
                        })
                    })
                };
                let jira = test.fingerprint.as_ref().and_then(|fp| {
                    gate.outcome
                        .bugs
                        .iter()
                        .find(|b| &b.fingerprint == fp)
                        .map(|b| b.key.clone())
                });
                Ok(FailureOutput {
                    test_id: test.test_id.clone(),
                    source: FailureSource::Remote,
                    title: test.title.clone(),
                    file: test.file.clone(),
                    line: u64::from(test.line),
                    journey: test.journey.clone(),
                    location: bundle.and_then(|b| b.location.clone()),
                    error: bundle
                        .map(|b| b.error.clone())
                        .or_else(|| test.error.clone())
                        .unwrap_or_default(),
                    steps: bundle.and_then(|b| b.step.clone()).into_iter().collect(),
                    attempts: bundle
                        .map(|b| {
                            b.attempts
                                .iter()
                                .map(|a| Attempt {
                                    retry: a.retry,
                                    status: a.status.clone(),
                                    duration_ms: a.duration_ms,
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    baseline: test.baseline.map(baseline_class),
                    fingerprint: test.fingerprint.as_ref().map(|f| f.as_str().to_string()),
                    screenshot: url_of(EvidenceKind::Screenshot),
                    trace_url: url_of(EvidenceKind::Trace),
                    jira,
                })
            }
        }
    }

    async fn repro(&self, request: ReproRequest) -> Result<ReproOutput, ToolError> {
        let dir = self.root.join(".guard");
        let _ = std::fs::create_dir_all(&dir);
        let result = dir.join(format!("repro-result-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&result);
        let args = vec![
            "repro".to_string(),
            request.test_id,
            "--pr".into(),
            request.pr.to_string(),
            "--result-file".into(),
            result.to_string_lossy().into_owned(),
        ];
        let ran = self.guard(&args, &[]).await?;
        if let Some(error) = failed(&ran) {
            return Err(error);
        }
        let text = std::fs::read_to_string(&result)
            .map_err(|e| ToolError::environment(format!("guard repro wrote no result: {e}")))?;
        let _ = std::fs::remove_file(&result);
        serde_json::from_str(&text).map_err(|e| ToolError::environment(e.to_string()))
    }

    async fn catalog(&self) -> Result<Catalog, ToolError> {
        self.ctx()
            .read_catalog()
            .map_err(|m| ToolError::config(m.join("\n")))
    }
}

/// `guard_failure` without `pr`: the last local run's `results/report.json`.
pub fn local_failure(e2e_dir: &Path, root: &Path, id: &str) -> Result<FailureOutput, ToolError> {
    let (report, raw) = read_report(e2e_dir)?;
    let outcome = classify(&report)
        .into_iter()
        .find(|t| test_id(&t.project, &t.file, &t.title) == id)
        .ok_or_else(|| ToolError::not_found(id.to_string()))?;
    let raw_tests = evidence::raw_tests(&raw);
    let raw_test = raw_tests
        .iter()
        .find(|r| {
            r.file == outcome.file && r.title == outcome.title && r.project == outcome.project
        })
        .map(|r| r.test);
    let failing = raw_test.and_then(last_failing);
    let status = match outcome.status {
        Classification::Failed => TestStatus::Failed,
        Classification::Flaky => TestStatus::Flaky,
        _ => TestStatus::Passed,
    };
    let fp = (status != TestStatus::Passed)
        .then(|| fingerprint(id, outcome.error.as_deref().unwrap_or("")).0);
    Ok(FailureOutput {
        test_id: id.to_string(),
        source: FailureSource::Local,
        title: outcome.title.clone(),
        file: outcome.file.clone(),
        line: u64::from(outcome.line),
        journey: outcome.journey.clone(),
        location: raw_test.and_then(|t| evidence::location(t, e2e_dir)),
        error: failing
            .and_then(|r| r["error"]["message"].as_str())
            .map(strip_ansi)
            .or_else(|| outcome.error.clone())
            .unwrap_or_default(),
        steps: failing.map(steps).unwrap_or_default(),
        attempts: raw_test
            .map(|t| {
                evidence::attempts(t)
                    .into_iter()
                    .map(|a| Attempt {
                        retry: a.retry,
                        status: a.status,
                        duration_ms: a.duration_ms,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        baseline: None,
        fingerprint: fp,
        screenshot: raw_test
            .and_then(|t| local_evidence(t, e2e_dir, root, EvidenceKind::Screenshot)),
        trace_url: raw_test.and_then(|t| local_evidence(t, e2e_dir, root, EvidenceKind::Trace)),
        jira: None,
    })
}

/// `GUARD_MCP_ROLE=coder` hides test sources from `guard_map` (guard-map.md).
pub const MCP_ROLE_ENV: &str = "GUARD_MCP_ROLE";

/// `guard_map` over the guard-impact graph at HEAD (the default backend).
fn map_source(ctx: &Context) -> Result<Arc<dyn guard_map::GraphSource>, String> {
    let config = ctx.load_config().map_err(|e| e.messages.join("; "))?;
    let head = crate::git::head_sha(&ctx.root).map_err(|e| e.to_string())?;
    let source = guard_map::GuardImpactSource::build(&ctx.root, &config, &head)
        .map_err(|e| format!("guard_map: {e}"))?;
    Ok(if std::env::var(MCP_ROLE_ENV).as_deref() == Ok("coder") {
        Arc::new(guard_map::HidePaths::for_coder(source))
    } else {
        Arc::new(source)
    })
}

/// `guard_history` through the read-only reader login, when configured.
async fn history(ctx: &Context) -> Option<guard_mcp::tools::history::HistoryContext> {
    std::env::var(guard_store::MEMORY_READ_DATABASE_URL_ENV).ok()?;
    let config = ctx.load_config().ok()?;
    match guard_store::PgMemoryFactsReader::from_env().await {
        Ok(reader) => Some(guard_mcp::tools::history::HistoryContext {
            reader: Arc::new(reader),
            repo: crate::agent_lane::repo_slug(ctx, &config),
            evidence: None,
        }),
        Err(e) => {
            crate::error::warning(format!("guard_history: {e}"));
            None
        }
    }
}

pub fn run(ctx: &Context) -> crate::error::CliResult<u8> {
    let actions = CliActions::new(ctx).map_err(crate::error::CliError::env)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| crate::error::CliError::env(e.to_string()))?;
    runtime
        .block_on(async {
            let mut handler = guard_mcp::GuardMcpServer::new(
                Arc::new(actions),
                guard_mcp::ServerOptions::default(),
            );
            match map_source(ctx) {
                Ok(source) => handler = handler.with_map(source),
                Err(why) => handler = handler.with_map_unavailable(why),
            }
            if let Some(h) = history(ctx).await {
                handler = handler.with_history(h);
            }
            guard_mcp::serve_handler(handler, tokio::io::stdin(), tokio::io::stdout()).await
        })
        .map_err(|e| crate::error::CliError::env(e.to_string()))?;
    Ok(crate::error::EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_affected_passes_base_only_when_given() {
        let detected = AffectedRequest { base: None };
        assert_eq!(affected_args(&detected), ["affected", "--json"]);
        let given = AffectedRequest {
            base: Some("develop".into()),
        };
        assert_eq!(
            affected_args(&given),
            ["affected", "--json", "--base", "develop"]
        );
        let test = |base: Option<&str>, selection: TestSelection| {
            test_args(&TestRequest {
                selection,
                base: base.map(String::from),
                docker: false,
            })
        };
        assert_eq!(test(None, TestSelection::Affected), ["test", "--affected"]);
        assert_eq!(
            test(Some("dev"), TestSelection::Affected),
            ["test", "--affected", "--base", "dev"]
        );
        assert_eq!(test(Some("dev"), TestSelection::All), ["test", "--all"]);
    }

    #[test]
    fn a_dropped_tree_kill_takes_the_grandchildren_too() {
        use std::io::Read;
        // The shell is the child; the sleeper is a grandchild holding stdout.
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "ping -n 60 127.0.0.1 >NUL & echo done"]);
            c
        } else {
            let mut c = std::process::Command::new("sh");
            c.args(["-c", "sleep 60; echo done"]);
            c
        };
        crate::process::detach(&mut cmd);
        let mut child = cmd.stdout(Stdio::piped()).spawn().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let guard = TreeKill::new(child.id());
        std::thread::sleep(std::time::Duration::from_millis(500));
        drop(guard);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = String::new();
            let _ = stdout.read_to_string(&mut out);
            let _ = tx.send(out);
        });
        let out = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the grandchild was killed, so stdout closed");
        assert!(!out.contains("done"), "{out}");
        let _ = child.wait();
    }

    #[test]
    fn a_disarmed_tree_kill_leaves_the_process_alone() {
        let mut child = std::process::Command::new("git")
            .arg("--version")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut guard = TreeKill::new(child.id());
        assert!(child.wait().unwrap().success());
        guard.disarm();
    }

    #[test]
    fn tags_come_from_the_printed_selection() {
        let out = "base: main (merge-base 1a2b3c4)\nbranch: x\nchanged files: 1\n@smoke         smoke\n@journey:a.b   path f matches g\npassed 1  flaky 0\n";
        assert_eq!(printed_tags(out), vec!["@journey:a.b", "@smoke"]);
    }

    #[test]
    fn exit_codes_map_to_tool_errors() {
        let ran = |code: i32| Ran {
            code,
            stdout: String::new(),
            stderr: "error: bad\nhint: fix\nnoise".into(),
        };
        assert!(failed(&ran(0)).is_none());
        assert!(failed(&ran(1)).is_none());
        let config = serde_json::to_value(failed(&ran(2)).unwrap()).unwrap();
        assert_eq!(config["error"], "config_error");
        assert_eq!(config["exit_code"], 2);
        assert_eq!(config["detail"], "error: bad\nhint: fix");
        let env = serde_json::to_value(failed(&ran(3)).unwrap()).unwrap();
        assert_eq!(env["error"], "environment_error");
    }

    #[test]
    fn steps_stop_at_the_failing_one() {
        let result = serde_json::json!({"steps": [
            {"title": "open cart"}, {"title": "pay", "error": {"message": "x"}}, {"title": "after"}
        ]});
        assert_eq!(steps(&result), vec!["open cart", "pay"]);
    }

    #[test]
    fn local_failures_read_the_last_report() {
        let dir = tempfile::TempDir::new().unwrap();
        let e2e = dir.path().join("e2e-playwright");
        std::fs::create_dir_all(e2e.join("results")).unwrap();
        let report = serde_json::json!({"suites": [{"title": "tests/a.spec.ts", "file": "tests/a.spec.ts",
            "specs": [{"title": "does it", "file": "tests/a.spec.ts", "line": 3, "tags": ["@journey:a.b"],
              "tests": [{"projectName": "chromium", "status": "unexpected", "results": [
                {"retry": 0, "status": "failed", "duration": 5,
                 "error": {"message": "\u{1b}[31mboom\u{1b}[0m\nat x"},
                 "steps": [{"title": "click"}]}]}]}]}]});
        std::fs::write(e2e.join("results/report.json"), report.to_string()).unwrap();
        let out = local_failure(&e2e, dir.path(), "chromium::tests/a.spec.ts::does it").unwrap();
        assert_eq!(out.error, "boom\nat x");
        assert_eq!(out.journey.as_deref(), Some("a.b"));
        assert_eq!(out.steps, vec!["click"]);
        assert_eq!(out.attempts.len(), 1);
        assert!(out.fingerprint.is_some());
        let missing = local_failure(&e2e, dir.path(), "chromium::x::y").unwrap_err();
        assert_eq!(serde_json::to_value(missing).unwrap()["error"], "not_found");
    }
}
