use crate::common::{assert_golden, coder_task, fixture, sandbox_for};
use guard_core::ports::agent::{AgentRuntime, EVENTS_FILE};
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::{ExecOutput, SandboxProvider};
use guard_core::ports::{RuntimeEventKind, TaskStatus, Usage};
use guard_runtimes::clock::FixedClock;
use guard_runtimes::codex::{self, CodexRuntime};
use std::sync::Arc;

const T0: u64 = 1_793_527_200_000;

#[test]
fn coder_invocation_is_golden() {
    let mut t = coder_task();
    t.model = "openai/gpt-5".into();
    let inv = codex::invocation(&t).unwrap();
    assert_golden("codex/coder.json", &inv.to_golden());
}

#[test]
fn config_toml_pins_the_gateway_provider_and_never_approves() {
    let mut t = coder_task();
    t.model = "openai/gpt-5".into();
    let inv = codex::invocation(&t).unwrap();
    let cfg: toml::Table = toml::from_str(inv.file("/task/codex/config.toml").unwrap()).unwrap();
    assert_eq!(cfg["model"].as_str(), Some("openai/gpt-5"));
    assert_eq!(cfg["model_provider"].as_str(), Some("guard_gateway"));
    assert_eq!(cfg["approval_policy"].as_str(), Some("never"));
    assert_eq!(cfg["web_search"].as_str(), Some("disabled"));
    let p = &cfg["model_providers"]["guard_gateway"];
    assert_eq!(p["base_url"].as_str(), Some("http://llm-gateway:4000/v1"));
    assert_eq!(p["env_key"].as_str(), Some("GUARD_GATEWAY_KEY"));
    assert_eq!(p["wire_api"].as_str(), Some("responses"));
    let mcp = &cfg["mcp_servers"]["guard"];
    assert_eq!(mcp["command"].as_str(), Some("guard"));
    let tools: Vec<&str> = mcp["enabled_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        t.tools.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert_eq!(
        inv.env.get("CODEX_HOME").map(String::as_str),
        Some("/task/codex")
    );
    assert!(inv.stdin.as_deref().unwrap().contains(&t.summary));
    assert_eq!(inv.argv.last().map(String::as_str), Some("-"));
}

#[test]
fn exec_json_maps_to_runtime_events() {
    let parsed = codex::parse_jsonl(
        &fixture("codex/exec_success.jsonl"),
        "openai/gpt-5",
        &FixedClock::new(T0),
    );
    let tools: Vec<(String, bool)> = parsed
        .events
        .iter()
        .filter_map(|e| match &e.event {
            RuntimeEventKind::ToolCall { tool, ok, .. } => Some((tool.clone(), *ok)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        vec![
            ("shell".to_string(), true),
            ("apply_patch".to_string(), true),
            ("guard_test".to_string(), false),
            ("shell".to_string(), false),
        ]
    );
    let usage = Usage {
        input_tokens: 24763,
        output_tokens: 122,
        cached_input_tokens: 24448,
        cache_creation_input_tokens: 0,
    };
    assert!(parsed.events.iter().any(|e| e.usage() == Some(&usage)));
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Badge implemented; one test still failing.");
    assert_eq!(r.usage, usage);
    assert_eq!(r.steps, 1);
}

#[test]
fn turn_failed_is_a_failed_result() {
    let parsed = codex::parse_jsonl(
        &fixture("codex/exec_failed.jsonl"),
        "openai/gpt-5",
        &FixedClock::new(T0),
    );
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Failed);
    assert_eq!(
        r.error.as_deref(),
        Some("unexpected status 401 Unauthorized")
    );
    assert_eq!(
        parsed
            .events
            .iter()
            .filter(|e| matches!(e.event, RuntimeEventKind::Error { .. }))
            .count(),
        2
    );
}

/// The virtual key's `max_budget` is Codex's only spend limit: LiteLLM's
/// rejection (synthetic fixture shaped like LiteLLM's `budget_exceeded`
/// error) is budget exhaustion, not a retryable failure.
#[test]
fn a_gateway_budget_rejection_is_budget_exceeded() {
    let parsed = codex::parse_jsonl(
        &fixture("codex/exec_budget_exceeded.jsonl"),
        "openai/gpt-5",
        &FixedClock::new(T0),
    );
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::BudgetExceeded);
    assert!(r.error.unwrap().contains("Budget has been exceeded"));
}

#[tokio::test]
async fn run_writes_config_and_parses_stdout() {
    let provider = Arc::new(FakeSandboxProvider::new().with_exec(
        "codex",
        ExecOutput {
            exit_code: 0,
            stdout: fixture("codex/exec_success.jsonl"),
            ..Default::default()
        },
    ));
    let mut task = coder_task();
    task.model = "openai/gpt-5".into();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let rt = CodexRuntime::new(provider.clone()).with_clock(Arc::new(FixedClock::new(T0)));
    let out = rt.run(&sb, &task).await.unwrap();
    assert_eq!(out.runtime, "codex");
    assert_eq!(out.result.status, TaskStatus::Completed);
    let cfg = provider
        .read_file(&sb, "/task/codex/config.toml")
        .await
        .unwrap();
    assert!(
        String::from_utf8(cfg)
            .unwrap()
            .contains("approval_policy = \"never\"")
    );
    assert!(provider.read_file(&sb, EVENTS_FILE).await.is_ok());
}

/// Recorded from the real `codex` 0.159.2 against a local fake Responses
/// server (no provider), with this adapter's config.toml (base_url adapted).
#[test]
fn recorded_stream_from_the_real_cli_parses() {
    let parsed = codex::parse_jsonl(
        &fixture("codex/recorded_fake_gateway.jsonl"),
        "openai/gpt-5",
        &FixedClock::new(T0),
    );
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Fake reply: done.");
    assert_eq!(
        r.usage,
        Usage {
            input_tokens: 120,
            output_tokens: 5,
            cached_input_tokens: 64,
            cache_creation_input_tokens: 0
        }
    );
    // The model-metadata warning is an error event, not a failure.
    assert!(parsed.events.iter().any(|e| matches!(
        &e.event,
        RuntimeEventKind::Error { message } if message.contains("Model metadata")
    )));
}
