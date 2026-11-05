//! The shared run loop, exercised through the Codex adapter (no preflight).

use crate::common::{coder_task, fixture, sandbox_for};
use guard_core::ports::agent::{AgentRuntime, RESULT_FILE};
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::{ExecOutput, SandboxProvider};
use guard_core::ports::{AgentOutcome, RuntimeError, Sandbox, TaskStatus};
use guard_runtimes::codex::CodexRuntime;
use std::sync::Arc;
use std::time::Duration;

fn task() -> guard_core::ports::AgentTask {
    let mut t = coder_task();
    t.model = "openai/gpt-5".into();
    t
}

async fn run_with(
    output: ExecOutput,
) -> (
    Arc<FakeSandboxProvider>,
    Sandbox,
    Result<AgentOutcome, RuntimeError>,
) {
    let provider = Arc::new(FakeSandboxProvider::new().with_exec("codex", output));
    let t = task();
    let sb = sandbox_for(provider.as_ref(), &t).await;
    let r = CodexRuntime::new(provider.clone()).run(&sb, &t).await;
    (provider, sb, r)
}

#[tokio::test]
async fn timeout_is_reported_as_timeout() {
    let (_, _, r) = run_with(ExecOutput {
        exit_code: 137,
        timed_out: true,
        ..Default::default()
    })
    .await;
    assert_eq!(r.unwrap_err(), RuntimeError::Timeout { secs: 1800 });
}

#[tokio::test]
async fn crash_without_output_carries_the_stderr_tail() {
    let long = format!("{}TAIL", "x".repeat(5000));
    let (_, _, r) = run_with(ExecOutput {
        exit_code: 2,
        stderr: long.into_bytes(),
        ..Default::default()
    })
    .await;
    match r.unwrap_err() {
        RuntimeError::Crashed {
            exit_code,
            stderr_tail,
        } => {
            assert_eq!(exit_code, Some(2));
            assert!(stderr_tail.ends_with("TAIL"));
            assert!(stderr_tail.len() <= 2000);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn clean_exit_without_terminal_record_is_a_protocol_error() {
    let (_, _, r) = run_with(ExecOutput::default()).await;
    assert!(matches!(r.unwrap_err(), RuntimeError::Protocol(_)));
}

#[tokio::test]
async fn non_zero_exit_with_a_terminal_record_is_still_an_outcome() {
    let (provider, sb, r) = run_with(ExecOutput {
        exit_code: 1,
        stdout: fixture("codex/exec_failed.jsonl"),
        ..Default::default()
    })
    .await;
    let out = r.unwrap();
    assert_eq!(out.exit_code, Some(1));
    assert_eq!(out.result.status, TaskStatus::Failed);
    assert!(provider.read_file(&sb, RESULT_FILE).await.is_ok());
}

#[tokio::test]
async fn invalid_task_never_execs() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let t = task();
    let sb = sandbox_for(provider.as_ref(), &t).await;
    let mut bad = t.clone();
    bad.gateway.key_env = "not-an-env".into();
    let err = CodexRuntime::new(provider.clone())
        .run(&sb, &bad)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::InvalidTask(_)));
    assert!(provider.exec_log(&sb.id).is_empty());
}

#[test]
fn exec_request_carries_timeout_workdir_and_stdin() {
    let t = task();
    let inv = guard_runtimes::codex::invocation(&t).unwrap();
    let req = inv.to_exec(Duration::from_secs(t.timeout_secs));
    assert_eq!(req.timeout, Some(Duration::from_secs(1800)));
    assert_eq!(req.workdir.as_deref(), Some("/workspace"));
    assert_eq!(
        req.stdin.as_deref(),
        inv.stdin.as_deref().map(str::as_bytes)
    );
}
