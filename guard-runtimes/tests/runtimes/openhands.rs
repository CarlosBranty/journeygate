use crate::common::{assert_golden, coder_task, fixture, sandbox_for};
use guard_core::ports::agent::{AgentRuntime, EVENTS_FILE, RESULT_FILE};
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::SandboxProvider;
use guard_core::ports::{RuntimeError, TaskStatus};
use guard_runtimes::openhands::{self, CONFIG_PATH, DRIVER_PATH, DRIVER_PY, OpenHandsRuntime};
use std::sync::Arc;

fn task() -> guard_core::ports::AgentTask {
    let mut t = coder_task();
    t.model = "openai/gpt-5".into();
    t
}

#[test]
fn invocation_is_golden() {
    let inv = openhands::invocation(&task()).unwrap();
    assert_golden("openhands/coder.json", &inv.to_golden());
}

#[test]
fn condenser_has_its_own_usage_id() {
    let inv = openhands::invocation(&task()).unwrap();
    let cfg: serde_json::Value = serde_json::from_str(inv.file(CONFIG_PATH).unwrap()).unwrap();
    assert_eq!(cfg["agent_usage_id"], "agent");
    assert_eq!(cfg["condenser"]["usage_id"], "condenser");
    assert_eq!(cfg["base_url"], "http://llm-gateway:4000");
    assert_eq!(cfg["key_env"], "GUARD_GATEWAY_KEY");
    assert!(DRIVER_PY.contains("LLMSummarizingCondenser("));
    assert!(DRIVER_PY.contains("\"usage_id\": cfg[\"condenser\"][\"usage_id\"]"));
    assert_eq!(inv.file(DRIVER_PATH), Some(DRIVER_PY));
}

/// The driver is Python run inside the image; here we only check it parses.
/// Needs `python` on PATH: `cargo test -p guard-runtimes -- --include-ignored`.
#[test]
#[ignore = "needs python on PATH"]
fn driver_is_valid_python() {
    let dir = std::env::temp_dir().join(format!("guard-oh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("driver.py");
    std::fs::write(&file, DRIVER_PY).unwrap();
    let python = if cfg!(windows) { "python" } else { "python3" };
    let out = std::process::Command::new(python)
        .args([
            "-I",
            "-c",
            "import ast,sys; ast.parse(open(sys.argv[1], encoding='utf-8').read())",
        ])
        .arg(&file)
        .output()
        .expect("python on PATH");
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn run_reads_the_native_file_contract() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let t = task();
    let sb = sandbox_for(provider.as_ref(), &t).await;
    provider
        .put_file(&sb, EVENTS_FILE, fixture("openhands/events.jsonl"))
        .await
        .unwrap();
    provider
        .put_file(&sb, RESULT_FILE, fixture("openhands/result.json"))
        .await
        .unwrap();
    let rt = OpenHandsRuntime::new(provider.clone());
    let out = rt.run(&sb, &t).await.unwrap();
    assert_eq!(out.runtime, "openhands");
    assert_eq!(out.result.status, TaskStatus::Completed);
    assert_eq!(out.events.len(), 4);
    assert_eq!(
        provider.exec_log(&sb.id)[0],
        vec!["python3", DRIVER_PATH, CONFIG_PATH]
    );
}

#[tokio::test]
async fn non_increasing_seq_is_a_protocol_error() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let t = task();
    let sb = sandbox_for(provider.as_ref(), &t).await;
    let events = String::from_utf8(fixture("openhands/events.jsonl"))
        .unwrap()
        .replace("\"seq\":3", "\"seq\":2");
    provider
        .put_file(&sb, EVENTS_FILE, events.into_bytes())
        .await
        .unwrap();
    provider
        .put_file(&sb, RESULT_FILE, fixture("openhands/result.json"))
        .await
        .unwrap();
    let err = OpenHandsRuntime::new(provider.clone())
        .run(&sb, &t)
        .await
        .unwrap_err();
    assert!(
        matches!(err, RuntimeError::Protocol(ref m) if m.contains("seq")),
        "{err:?}"
    );
}
