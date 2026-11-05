//! `mini-swe-agent`: the `mini` CLI of SWE-agent/mini-swe-agent (MIT).
//!
//! Sources (checked 2026-11-01, `SWE-agent/mini-swe-agent` main):
//! - `src/minisweagent/run/mini.py`: `-m/--model`, `-t/--task`, `-y/--yolo`,
//!   `-l/--cost-limit`, `-c/--config` (repeatable, "Multiple configs will be
//!   recursively merged"; setting it drops the default, so `mini.yaml` is
//!   listed first), `-o/--output` (trajectory file), `--exit-immediately`.
//!   `configure_if_first_time()` runs an interactive setup unless
//!   `MSWEA_CONFIGURED` is set.
//! - `src/minisweagent/__init__.py`: `MSWEA_GLOBAL_CONFIG_DIR`,
//!   `MSWEA_SILENT_STARTUP`.
//! - `src/minisweagent/models/litellm_model.py`: `model_kwargs` are passed to
//!   `litellm.completion(model=model_name, **model_kwargs)`; `cost_tracking`
//!   defaults from `MSWEA_COST_TRACKING` (`ignore_errors` because gateway
//!   model names are not in LiteLLM's price map; the virtual key's
//!   `max_budget` is the real cap).
//! - Trajectory format (`docs/usage/output_files.md`): `info.{exit_status,
//!   submission, model_stats.{instance_cost, api_calls}}` and `messages` with
//!   `extra.{actions, cost, timestamp, response}` on assistant messages and
//!   `extra.{returncode, timestamp}` on observations.
//!
//! Pinning: model `litellm_proxy/<model>` with `model_kwargs.api_base` = the
//! gateway root. The `litellm_proxy` provider reads its key only from
//! `LITELLM_PROXY_API_KEY`, so the sandbox carries the virtual key under that
//! name too (`crate::secret_env_aliases`). OpenAI models get
//! `prompt_cache_key = "<repo>:<role>"` (tokens.md).
//!
//! `guard.yaml` is written as JSON, which YAML 1.2 (and PyYAML) reads as-is.

use crate::clock::Clock;
use crate::driver::{ProcessAdapter, process_runtime};
use crate::endpoint::GatewayEndpoint;
use crate::events::{EventLog, Parsed, result, str_at, u64_at};
use crate::invocation::Invocation;
use crate::prompt;
use guard_core::ports::agent::{AgentTask, RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_core::ports::sandbox::{
    ExecOutput, Sandbox, SandboxError, SandboxProvider, WORKSPACE_DIR,
};
use serde_json::{Value, json};

pub const NAME: &str = "mini-swe-agent";
pub const LITELLM_KEY_ENV: &str = "LITELLM_PROXY_API_KEY";
pub const MINI_DIR: &str = "/task/mini";
pub const CONFIG_PATH: &str = "/task/mini/guard.yaml";
pub const TRAJECTORY_PATH: &str = "/task/mini/trajectory.json";

pub fn invocation(task: &AgentTask) -> Result<Invocation, RuntimeError> {
    let ep = GatewayEndpoint::pin(&task.gateway)?;
    let mut model_kwargs = json!({ "api_base": ep.root(), "drop_params": true });
    if task.model.starts_with("openai/") {
        model_kwargs["prompt_cache_key"] = json!(format!("{}:{}", task.repo, task.role.as_str()));
    }
    let config = json!({
        "model": { "model_kwargs": model_kwargs, "cost_tracking": "ignore_errors" },
        "environment": { "cwd": WORKSPACE_DIR, "timeout": 300 }
    });
    let mut cfg = serde_json::to_string_pretty(&config).expect("json");
    cfg.push('\n');
    let argv = vec![
        "mini".to_string(),
        "-c".into(),
        "mini.yaml".into(),
        "-c".into(),
        CONFIG_PATH.into(),
        "-m".into(),
        format!("litellm_proxy/{}", task.model),
        "-t".into(),
        prompt::render(task),
        "-y".into(),
        "--exit-immediately".into(),
        "-l".into(),
        format!("{}", task.budget_usd),
        "-o".into(),
        TRAJECTORY_PATH.into(),
    ];
    Ok(Invocation::new(argv)
        .with_env("MSWEA_CONFIGURED", "true")
        .with_env("MSWEA_SILENT_STARTUP", "1")
        .with_env("MSWEA_GLOBAL_CONFIG_DIR", MINI_DIR)
        .with_env("MSWEA_COST_TRACKING", "ignore_errors")
        .with_file(CONFIG_PATH, cfg))
}

fn ts_millis(extra: &Value) -> Option<u64> {
    extra
        .get("timestamp")
        .and_then(Value::as_f64)
        .filter(|t| t.is_finite() && *t >= 0.0)
        .map(|t| (t * 1000.0).round() as u64)
}

/// Parses the trajectory file `mini -o` writes.
pub fn parse_trajectory(bytes: &[u8], task_model: &str) -> Result<Parsed, String> {
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("trajectory is not JSON: {e}"))?;
    let info = v.get("info").ok_or("trajectory has no info")?;
    let messages = v["messages"]
        .as_array()
        .ok_or("trajectory has no messages")?;
    let mut log = EventLog::default();
    let mut pending: Option<u64> = None;
    let mut last_ts = 0u64;
    for m in messages {
        let extra = &m["extra"];
        let ts = ts_millis(extra).unwrap_or(last_ts);
        last_ts = ts;
        match str_at(m, "role") {
            Some("assistant") => {
                let response = &extra["response"];
                if let Some(u) = response.get("usage") {
                    let cached = u64_at(&u["prompt_tokens_details"], "cached_tokens")
                        .max(u64_at(u, "cache_read_input_tokens"));
                    log.push_at(
                        ts,
                        RuntimeEventKind::ModelCall {
                            model: str_at(response, "model").unwrap_or(task_model).into(),
                            usage: Usage {
                                input_tokens: u64_at(u, "prompt_tokens"),
                                output_tokens: u64_at(u, "completion_tokens"),
                                cached_input_tokens: cached,
                                cache_creation_input_tokens: u64_at(
                                    u,
                                    "cache_creation_input_tokens",
                                ),
                            },
                            cost_usd: extra.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
                        },
                    );
                }
                if let Some(t) = str_at(m, "content").filter(|t| !t.trim().is_empty()) {
                    log.push_at(ts, RuntimeEventKind::Message { text: t.into() });
                }
                let has_action = extra["actions"].as_array().is_some_and(|a| !a.is_empty());
                pending = has_action.then_some(ts);
            }
            Some("user") => {
                if let (Some(start), Some(rc)) = (
                    pending.take(),
                    extra.get("returncode").and_then(Value::as_i64),
                ) {
                    log.push_at(
                        ts,
                        RuntimeEventKind::ToolCall {
                            tool: "bash".into(),
                            ok: rc == 0,
                            duration_ms: ts.saturating_sub(start),
                        },
                    );
                }
            }
            _ => {}
        }
    }
    let exit_status = str_at(info, "exit_status").unwrap_or("unknown");
    let status = match exit_status {
        "Submitted" => TaskStatus::Completed,
        "LimitsExceeded" => TaskStatus::BudgetExceeded,
        _ => TaskStatus::Failed,
    };
    let submission = str_at(info, "submission").unwrap_or_default();
    let summary = if submission.trim().is_empty() {
        format!("mini-swe-agent exited: {exit_status}")
    } else {
        submission.to_string()
    };
    let steps = u64_at(&info["model_stats"], "api_calls") as u32;
    let cost = log.cost_sum();
    let r = result(
        status,
        &summary,
        log.usage_sum(),
        cost,
        steps,
        Some(exit_status.to_string()),
    );
    Ok(Parsed {
        events: log.into_events(),
        result: Some(r),
    })
}

process_runtime!(
    /// `AgentRuntime` for mini-swe-agent (`mini`, trajectory file).
    MiniSweAgentRuntime
);

#[async_trait::async_trait]
impl ProcessAdapter for MiniSweAgentRuntime {
    fn name(&self) -> &'static str {
        NAME
    }

    fn invocation(&self, task: &AgentTask) -> Result<Invocation, RuntimeError> {
        invocation(task)
    }

    async fn collect(
        &self,
        provider: &dyn SandboxProvider,
        sandbox: &Sandbox,
        task: &AgentTask,
        _output: &ExecOutput,
        _clock: &dyn Clock,
    ) -> Result<Parsed, RuntimeError> {
        let bytes = match provider.read_file(sandbox, TRAJECTORY_PATH).await {
            Ok(b) => b,
            Err(SandboxError::NotFound(_)) => {
                return Err(RuntimeError::Protocol(format!(
                    "mini-swe-agent wrote no {TRAJECTORY_PATH}"
                )));
            }
            Err(e) => return Err(e.into()),
        };
        parse_trajectory(&bytes, &task.model).map_err(RuntimeError::Protocol)
    }
}
