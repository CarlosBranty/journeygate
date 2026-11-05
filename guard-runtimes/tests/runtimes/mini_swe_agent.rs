use crate::common::{assert_golden, coder_task, fixture, sandbox_for};
use guard_core::ports::agent::AgentRuntime;
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::SandboxProvider;
use guard_core::ports::{RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_runtimes::clock::rfc3339_from_millis;
use guard_runtimes::mini_swe_agent::{self, MiniSweAgentRuntime, TRAJECTORY_PATH};
use std::sync::Arc;

fn task() -> guard_core::ports::AgentTask {
    let mut t = coder_task();
    t.model = "openai/gpt-5".into();
    t
}

#[test]
fn invocation_is_golden() {
    let inv = mini_swe_agent::invocation(&task()).unwrap();
    assert_golden("mini-swe-agent/coder.json", &inv.to_golden());
}

#[test]
fn config_points_litellm_at_the_gateway() {
    let inv = mini_swe_agent::invocation(&task()).unwrap();
    let cfg: serde_json::Value =
        serde_json::from_str(inv.file("/task/mini/guard.yaml").unwrap()).unwrap();
    assert_eq!(
        cfg["model"]["model_kwargs"]["api_base"],
        "http://llm-gateway:4000"
    );
    assert_eq!(
        cfg["model"]["model_kwargs"]["prompt_cache_key"],
        "acme/shop:coder"
    );
    assert_eq!(cfg["environment"]["cwd"], "/workspace");
    assert!(
        inv.argv
            .windows(2)
            .any(|w| w == ["-m", "litellm_proxy/openai/gpt-5"])
    );
    assert!(inv.argv.iter().any(|a| a == "--exit-immediately"));
    assert_eq!(
        inv.env.get("MSWEA_CONFIGURED").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        guard_runtimes::secret_env_aliases("mini-swe-agent", &task()),
        vec!["LITELLM_PROXY_API_KEY".to_string()]
    );
}

#[test]
fn prompt_cache_key_only_for_openai_models() {
    let mut t = task();
    t.model = "anthropic/claude-sonnet-4-5".into();
    let inv = mini_swe_agent::invocation(&t).unwrap();
    let cfg: serde_json::Value =
        serde_json::from_str(inv.file("/task/mini/guard.yaml").unwrap()).unwrap();
    assert!(
        cfg["model"]["model_kwargs"]
            .get("prompt_cache_key")
            .is_none()
    );
}

#[test]
fn trajectory_maps_to_runtime_events() {
    let parsed = mini_swe_agent::parse_trajectory(
        &fixture("mini-swe-agent/trajectory_submitted.json"),
        "openai/gpt-5",
    )
    .unwrap();
    let first = &parsed.events[0];
    assert_eq!(first.ts, rfc3339_from_millis(1_793_527_200_000));
    let usages: Vec<&Usage> = parsed.events.iter().filter_map(|e| e.usage()).collect();
    assert_eq!(usages.len(), 2);
    assert_eq!(usages[0].cached_input_tokens, 2048);
    let tools: Vec<(bool, u64)> = parsed
        .events
        .iter()
        .filter_map(|e| match &e.event {
            RuntimeEventKind::ToolCall {
                tool,
                ok,
                duration_ms,
            } => {
                assert_eq!(tool, "bash");
                Some((*ok, *duration_ms))
            }
            _ => None,
        })
        .collect();
    assert_eq!(tools, vec![(true, 500), (false, 250)]);
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Badge now reads product.stock.");
    assert_eq!(r.steps, 2);
    assert!((r.cost_usd - 0.002).abs() < 1e-12);
    assert_eq!(r.usage.input_tokens, 6200);
}

#[test]
fn limits_exceeded_is_budget_exceeded() {
    let parsed = mini_swe_agent::parse_trajectory(
        &fixture("mini-swe-agent/trajectory_limits.json"),
        "openai/gpt-5",
    )
    .unwrap();
    let r = parsed.result.unwrap();
    assert_eq!(r.status, TaskStatus::BudgetExceeded);
    assert_eq!(r.error.as_deref(), Some("LimitsExceeded"));
}

#[tokio::test]
async fn run_reads_the_trajectory_file() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let t = task();
    let sb = sandbox_for(provider.as_ref(), &t).await;
    // Simulates `mini -o /task/mini/trajectory.json` inside the sandbox.
    provider
        .put_file(
            &sb,
            TRAJECTORY_PATH,
            fixture("mini-swe-agent/trajectory_submitted.json"),
        )
        .await
        .unwrap();
    let rt = MiniSweAgentRuntime::new(provider.clone());
    let out = rt.run(&sb, &t).await.unwrap();
    assert_eq!(out.runtime, "mini-swe-agent");
    assert_eq!(out.result.status, TaskStatus::Completed);
    assert_eq!(provider.exec_log(&sb.id)[0][0], "mini");
}

#[tokio::test]
async fn run_without_trajectory_is_a_protocol_error() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let t = task();
    let sb = sandbox_for(provider.as_ref(), &t).await;
    let err = MiniSweAgentRuntime::new(provider.clone())
        .run(&sb, &t)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Protocol(_)), "{err:?}");
}
