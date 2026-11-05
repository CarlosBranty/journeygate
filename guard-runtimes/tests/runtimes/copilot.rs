use crate::common::{assert_golden, coder_task, fixture, sandbox_for};
use guard_core::ports::agent::AgentRuntime;
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::ExecOutput;
use guard_core::ports::{RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_runtimes::clock::FixedClock;
use guard_runtimes::copilot::{self, CopilotRuntime};
use std::sync::Arc;

const T0: u64 = 1_793_527_200_000;

fn byok_task() -> guard_core::ports::AgentTask {
    coder_task()
}

fn hosted_task() -> guard_core::ports::AgentTask {
    let mut t = coder_task();
    t.gateway.via_gateway = false;
    t.gateway.key_env = "COPILOT_GITHUB_TOKEN".into();
    t.model = "claude-sonnet-4.5".into();
    t
}

#[test]
fn byok_invocation_is_golden() {
    let inv = copilot::invocation(&byok_task()).unwrap();
    assert_golden("copilot-cli/byok.json", &inv.to_golden());
}

#[test]
fn byok_points_the_provider_at_the_gateway_without_the_key() {
    let inv = copilot::invocation(&byok_task()).unwrap();
    assert_eq!(
        inv.env.get("COPILOT_PROVIDER_BASE_URL").map(String::as_str),
        Some("http://llm-gateway:4000/v1")
    );
    assert_eq!(
        inv.env
            .get("COPILOT_PROVIDER_API_KEY_COMMAND")
            .map(String::as_str),
        Some("printenv GUARD_GATEWAY_KEY")
    );
    assert!(!inv.env.contains_key("COPILOT_PROVIDER_API_KEY"));
    assert!(inv.argv.iter().any(|a| a == "--output-format=json"));
    assert!(inv.argv.iter().any(|a| a.starts_with("--allow-tool=")));
    assert!(inv.argv.iter().any(|a| a.starts_with("--deny-tool=")));
    assert!(!inv.argv.iter().any(|a| a.starts_with("--allow-all")));
    assert!(
        inv.argv
            .iter()
            .any(|a| a == "--model=anthropic/claude-sonnet-4-5")
    );
}

#[test]
fn hosted_exception_invocation_is_golden_and_has_no_endpoint() {
    let inv = copilot::invocation(&hosted_task()).unwrap();
    assert!(inv.env.keys().all(|k| !k.starts_with("COPILOT_PROVIDER_")));
    assert_golden("copilot-cli/hosted.json", &inv.to_golden());
}

#[test]
fn jsonl_maps_to_runtime_events() {
    let parsed = copilot::parse_jsonl(
        &fixture("copilot-cli/prompt_success.jsonl"),
        &FixedClock::new(T0),
    );
    let tools: Vec<(String, bool, u64)> = parsed
        .events
        .iter()
        .filter_map(|e| match &e.event {
            RuntimeEventKind::ToolCall {
                tool,
                ok,
                duration_ms,
            } => Some((tool.clone(), *ok, *duration_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        vec![
            ("bash".to_string(), true, 250),
            ("edit".to_string(), false, 40)
        ]
    );
    assert_eq!(parsed.events[0].ts, "2026-11-01T10:00:00.900Z");
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Implemented the out-of-stock badge.");
    assert_eq!(
        r.usage,
        Usage {
            input_tokens: 18500,
            output_tokens: 190,
            cached_input_tokens: 17000,
            cache_creation_input_tokens: 500
        }
    );
    assert_eq!(r.steps, 1);
    assert_eq!(
        r.cost_usd, 0.0,
        "Copilot `cost` is not USD; spend_records are authoritative"
    );
}

#[test]
fn session_error_is_a_failed_result() {
    let parsed = copilot::parse_jsonl(
        &fixture("copilot-cli/prompt_error.jsonl"),
        &FixedClock::new(T0),
    );
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Failed);
    assert_eq!(r.error.as_deref(), Some("Copilot token missing"));
}

#[tokio::test]
async fn run_uses_the_prompt_flag() {
    let provider = Arc::new(FakeSandboxProvider::new().with_exec(
        "copilot",
        ExecOutput {
            exit_code: 0,
            stdout: fixture("copilot-cli/prompt_success.jsonl"),
            ..Default::default()
        },
    ));
    let task = byok_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let rt = CopilotRuntime::new(provider.clone()).with_clock(Arc::new(FixedClock::new(T0)));
    let out = rt.run(&sb, &task).await.unwrap();
    assert_eq!(out.runtime, "copilot-cli");
    assert_eq!(out.result.status, TaskStatus::Completed);
    let argv = &provider.exec_log(&sb.id)[0];
    assert_eq!(argv[1], "-p");
}

#[test]
fn hosted_exception_requires_the_copilot_token_variable() {
    let mut t = hosted_task();
    t.gateway.key_env = "GUARD_GATEWAY_KEY".into();
    // The token must reach Copilot as COPILOT_GITHUB_TOKEN: either key_env
    // names it, or the sandbox carries the alias (secret_env_aliases).
    assert_eq!(
        guard_runtimes::secret_env_aliases("copilot-cli", &t),
        vec!["COPILOT_GITHUB_TOKEN".to_string()]
    );
    assert!(copilot::invocation(&t).is_ok());
    let err = guard_runtimes::codex::invocation(&t).unwrap_err();
    assert!(
        matches!(err, RuntimeError::InvalidTask(_)),
        "via_gateway=false is copilot-only"
    );
}
