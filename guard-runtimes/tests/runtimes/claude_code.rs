use crate::common::{assert_golden, coder_task, fixture, qa_task, sandbox_for};
use guard_core::ports::agent::{AgentRuntime, EVENTS_FILE, RESULT_FILE, TASK_FILE};
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::{ExecOutput, SandboxProvider};
use guard_core::ports::{RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_runtimes::claude_code::{self, ClaudeCodeRuntime, MANAGED_SETTINGS_PATH};
use guard_runtimes::clock::FixedClock;
use std::sync::Arc;

const T0: u64 = 1_793_527_200_000;

#[test]
fn qa_invocation_is_golden() {
    let inv = claude_code::invocation(&qa_task()).unwrap();
    assert_golden("claude-code/qa.json", &inv.to_golden());
}

#[test]
fn invocation_pins_the_gateway_and_never_carries_the_key() {
    let inv = claude_code::invocation(&qa_task()).unwrap();
    assert_eq!(inv.argv[0], "claude");
    assert!(inv.argv.iter().any(|a| a == "-p"));
    assert!(
        inv.argv
            .windows(2)
            .any(|w| w == ["--output-format", "stream-json"])
    );
    assert!(
        inv.argv.iter().any(|a| a == "--verbose"),
        "stream-json with -p requires --verbose"
    );
    assert_eq!(
        inv.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some("http://llm-gateway:4000")
    );
    assert!(!inv.env.contains_key("ANTHROPIC_API_KEY"));
    assert!(!inv.env.contains_key("ANTHROPIC_AUTH_TOKEN"));
    let settings = inv.file("/task/claude/settings.json").unwrap();
    assert!(
        settings.contains("printenv GUARD_GATEWAY_KEY"),
        "{settings}"
    );
}

#[test]
fn managed_settings_allow_only_the_custom_endpoint() {
    let v = claude_code::managed_settings("http://llm-gateway:4000");
    assert_eq!(v["allowedProviders"], serde_json::json!(["customEndpoint"]));
    assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://llm-gateway:4000");
}

#[test]
fn non_claude_models_are_rejected() {
    let mut t = qa_task();
    t.model = "openai/gpt-5".into();
    let err = claude_code::invocation(&t).unwrap_err();
    assert!(matches!(err, RuntimeError::InvalidTask(_)), "{err:?}");
}

#[test]
fn stream_json_maps_to_runtime_events() {
    let parsed = claude_code::parse_stream(
        &fixture("claude-code/stream_success.jsonl"),
        &FixedClock::new(T0),
    );
    let kinds: Vec<&RuntimeEventKind> = parsed.events.iter().map(|e| &e.event).collect();
    let seqs: Vec<u64> = parsed.events.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, (1..=parsed.events.len() as u64).collect::<Vec<_>>());
    assert!(parsed.events.iter().all(|e| e.ts == "2026-11-01T10:00:00Z"));

    // msg_01 is split over two stream lines with the same usage: counted once.
    let model_calls: Vec<&Usage> = parsed.events.iter().filter_map(|e| e.usage()).collect();
    assert_eq!(model_calls.len(), 4);
    assert_eq!(
        *model_calls[0],
        Usage {
            input_tokens: 3,
            output_tokens: 20,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 5000
        }
    );
    assert_eq!(model_calls[1].cached_input_tokens, 5000);

    let tools: Vec<(&str, bool)> = kinds
        .iter()
        .filter_map(|k| match k {
            RuntimeEventKind::ToolCall { tool, ok, .. } => Some((tool.as_str(), *ok)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools,
        vec![
            ("Read", true),
            ("Edit", false),
            ("mcp__guard__guard_test", true)
        ]
    );
    assert!(matches!(
        kinds.last().unwrap(),
        RuntimeEventKind::Message { text } if text == "Done: the badge reads product.stock."
    ));

    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Done: the badge reads product.stock.");
    assert_eq!(r.cost_usd, 0.0421);
    assert_eq!(r.steps, 4);
    assert_eq!(
        r.usage,
        Usage {
            input_tokens: 23,
            output_tokens: 85,
            cached_input_tokens: 15150,
            cache_creation_input_tokens: 5000
        }
    );
    assert_eq!(r.error, None);
}

#[test]
fn budget_result_and_garbage_lines() {
    let parsed = claude_code::parse_stream(
        &fixture("claude-code/stream_budget.jsonl"),
        &FixedClock::new(T0),
    );
    assert!(parsed.events.iter().any(|e| matches!(
        &e.event,
        RuntimeEventKind::Error { message } if message.contains("not JSON")
    )));
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::BudgetExceeded);
    assert_eq!(r.cost_usd, 2.01);
    assert_eq!(r.error.as_deref(), Some("error_max_budget_usd"));
}

async fn installed(provider: &FakeSandboxProvider, sb: &guard_core::ports::Sandbox, url: &str) {
    provider
        .put_file(
            sb,
            MANAGED_SETTINGS_PATH,
            serde_json::to_vec(&claude_code::managed_settings(url)).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn run_executes_claude_and_writes_the_file_contract() {
    let provider = Arc::new(FakeSandboxProvider::new().with_exec(
        "claude",
        ExecOutput {
            exit_code: 0,
            stdout: fixture("claude-code/stream_success.jsonl"),
            stderr: vec![],
            timed_out: false,
        },
    ));
    let task = qa_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    installed(&provider, &sb, "http://llm-gateway:4000").await;
    let rt = ClaudeCodeRuntime::new(provider.clone()).with_clock(Arc::new(FixedClock::new(T0)));
    assert_eq!(rt.name(), "claude-code");

    let out = rt.run(&sb, &task).await.unwrap();
    assert_eq!(out.runtime, "claude-code");
    assert_eq!(out.result.status, TaskStatus::Completed);
    assert_eq!(out.exit_code, Some(0));

    let log = provider.exec_log(&sb.id);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0], claude_code::invocation(&task).unwrap().argv);

    let written: guard_core::ports::AgentTask =
        serde_json::from_slice(&provider.read_file(&sb, TASK_FILE).await.unwrap()).unwrap();
    assert_eq!(written, task);
    let events = String::from_utf8(provider.read_file(&sb, EVENTS_FILE).await.unwrap()).unwrap();
    assert_eq!(events.lines().count(), out.events.len());
    assert!(events.ends_with('\n'));
    let result: guard_core::ports::TaskResult =
        serde_json::from_slice(&provider.read_file(&sb, RESULT_FILE).await.unwrap()).unwrap();
    assert_eq!(result, out.result);
}

#[tokio::test]
async fn missing_or_foreign_managed_settings_make_the_runtime_unavailable() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let task = qa_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let rt = ClaudeCodeRuntime::new(provider.clone());
    let err = rt.run(&sb, &task).await.unwrap_err();
    assert!(matches!(err, RuntimeError::Unavailable(_)), "{err:?}");

    installed(&provider, &sb, "https://api.anthropic.com").await;
    let err = rt.run(&sb, &task).await.unwrap_err();
    assert!(
        matches!(err, RuntimeError::Unavailable(ref m) if m.contains("ANTHROPIC_BASE_URL")),
        "{err:?}"
    );
    assert!(provider.exec_log(&sb.id).is_empty(), "claude never started");
}

#[test]
fn coder_invocation_is_golden_when_the_model_is_claude() {
    let mut t = coder_task();
    t.model = "anthropic/claude-sonnet-4-5".into();
    let inv = claude_code::invocation(&t).unwrap();
    assert_golden("claude-code/coder.json", &inv.to_golden());
}

/// Recorded from the real `claude` 2.1.316 against a local fake Messages
/// server (no provider), with this adapter's argv (paths adapted).
#[test]
fn recorded_stream_from_the_real_cli_parses() {
    let parsed = claude_code::parse_stream(
        &fixture("claude-code/recorded_fake_gateway.jsonl"),
        &FixedClock::new(T0),
    );
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Fake reply: done.");
    assert_eq!(r.steps, 1);
    assert_eq!(r.usage.input_tokens, 12);
    assert_eq!(
        r.usage.output_tokens, 5,
        "result usage wins over message_start usage"
    );
    assert!(r.cost_usd > 0.0);
    assert_eq!(
        parsed.events.iter().filter(|e| e.usage().is_some()).count(),
        1
    );
}
