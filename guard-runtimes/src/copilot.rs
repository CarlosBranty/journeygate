//! `copilot-cli`: GitHub Copilot CLI in programmatic mode (`copilot -p`).
//!
//! Admission: **provisional**. The CLI is not installed on the dev machine;
//! nothing here was run against a real binary.
//!
//! Sources (checked 2026-11-01):
//! - Flags, from the CLI command reference
//!   (docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference)
//!   and the programmatic reference (`.../cli-programmatic-reference`):
//!   `-p`, `--output-format=json` ("outputs JSONL: one JSON object per line"),
//!   `--model=MODEL`, `--allow-tool=TOOL ...` / `--deny-tool=TOOL ...`
//!   (comma-separated; kinds `shell`, `write`, `read`, `url`, `memory`,
//!   `<mcp-server>`; deny wins), `--no-ask-user`, `--no-auto-update`,
//!   `--disable-builtin-mcps`, `--disable-client-telemetry`,
//!   `--secret-env-vars=VAR ...`, `--additional-mcp-config=@FILE`,
//!   `--log-level`, `--log-dir`, `--no-custom-instructions`.
//! - Env: `COPILOT_GITHUB_TOKEN` (auth, precedence over `GH_TOKEN`),
//!   `COPILOT_HOME`, `COPILOT_AUTO_UPDATE=false`,
//!   `COPILOT_CUSTOM_INSTRUCTIONS_DIRS`, `COPILOT_PROVIDER_API_KEY_COMMAND`
//!   ("Shell command that prints a fresh bring-your-own-key (BYOK) API key
//!   before each provider request").
//! - **Unconfirmed**: `COPILOT_PROVIDER_BASE_URL` and `COPILOT_PROVIDER_TYPE`.
//!   The reference only calls them "the legacy `COPILOT_PROVIDER_*` environment
//!   variables" and documents `providers.json` (`baseUrl`, `type`, `wireApi`)
//!   instead; `providers.json` holds keys as plain text ("Values such as
//!   `$MY_API_KEY` are literal strings"), so it is not used. Whether BYOK mode
//!   still needs a GitHub login is also unconfirmed.
//! - **Unconfirmed**: the JSONL record shapes. The docs do not specify them;
//!   the parser follows the Copilot SDK session events
//!   (`github/copilot-sdk` `nodejs/src/generated/session-events.ts`, main on
//!   2026-11-01): `{type, data, id, timestamp, parentId}` with
//!   `assistant.usage {model, inputTokens, outputTokens, cacheReadTokens,
//!   cacheWriteTokens, cost}`, `tool.execution_start {toolCallId, toolName}`,
//!   `tool.execution_complete {toolCallId, success}`, `assistant.message
//!   {content, parentToolCallId?}`, `assistant.turn_start`, `session.error
//!   {message}`, `session.idle`. `cost` is Copilot's request multiplier, not
//!   USD, so `cost_usd` is 0 and `spend_records` are authoritative.
//!
//! Two modes (agents-toml.md):
//! - `via_gateway = true` (BYOK): an OpenAI-compatible provider at the gateway
//!   `/v1`, key read with `printenv <key_env>`.
//! - `via_gateway = false` (the explicit exception): Copilot-hosted models,
//!   no endpoint override, token in `COPILOT_GITHUB_TOKEN` (an alias when
//!   `key_env` differs, `crate::secret_env_aliases`); the budget is Copilot's
//!   org policy. The sandbox needs the `copilot_exception` egress rule.

use crate::clock::{Clock, millis_from_rfc3339, rfc3339_from_millis};
use crate::driver::{ProcessAdapter, process_runtime};
use crate::endpoint::GatewayEndpoint;
use crate::events::{EventLog, Parsed, result, str_at, u64_at};
use crate::invocation::Invocation;
use crate::prompt;
use guard_core::ports::agent::{AgentTask, RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_core::ports::sandbox::{ExecOutput, SKILLS_DIR, Sandbox, SandboxProvider};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const NAME: &str = "copilot-cli";
/// The only token variable the hosted mode reads.
pub const GITHUB_TOKEN_ENV: &str = "COPILOT_GITHUB_TOKEN";
pub const COPILOT_HOME: &str = "/task/copilot";
pub const MCP_CONFIG_PATH: &str = "/task/copilot/mcp.json";

fn endpoint(task: &AgentTask) -> Result<Option<GatewayEndpoint>, RuntimeError> {
    if task.gateway.via_gateway {
        GatewayEndpoint::pin(&task.gateway).map(Some)
    } else {
        Ok(None)
    }
}

pub fn invocation(task: &AgentTask) -> Result<Invocation, RuntimeError> {
    let ep = endpoint(task)?;
    let mut allow = vec!["shell".to_string(), "write".to_string(), "read".to_string()];
    if !task.tools.is_empty() {
        allow.push("guard".into());
    }
    let mut secret_vars = vec![task.gateway.key_env.clone()];
    if task.gateway.key_env != GITHUB_TOKEN_ENV {
        secret_vars.push(GITHUB_TOKEN_ENV.into());
    }
    let mut argv: Vec<String> = vec![
        "copilot".into(),
        "-p".into(),
        prompt::render(task),
        "--output-format=json".into(),
        format!("--model={}", task.model),
        "--no-ask-user".into(),
        "--no-auto-update".into(),
        "--disable-builtin-mcps".into(),
        "--disable-client-telemetry".into(),
        "--no-custom-instructions".into(),
        format!("--allow-tool={}", allow.join(",")),
        "--deny-tool=url,memory".into(),
        format!("--secret-env-vars={}", secret_vars.join(",")),
        "--log-level=error".into(),
        format!("--log-dir={COPILOT_HOME}/logs"),
    ];
    let mut inv = Invocation::new(vec![]);
    if !task.tools.is_empty() {
        argv.push(format!("--additional-mcp-config=@{MCP_CONFIG_PATH}"));
        let mcp = json!({
            "mcpServers": {
                "guard": { "type": "local", "command": "guard", "args": ["mcp"], "tools": task.tools }
            }
        });
        let mut s = serde_json::to_string_pretty(&mcp).expect("json");
        s.push('\n');
        inv = inv.with_file(MCP_CONFIG_PATH, s);
    }
    inv.argv = argv;
    inv = inv
        .with_env("COPILOT_HOME", COPILOT_HOME)
        .with_env("COPILOT_AUTO_UPDATE", "false")
        .with_env("COPILOT_CUSTOM_INSTRUCTIONS_DIRS", SKILLS_DIR);
    if let Some(ep) = ep {
        inv = inv
            .with_env("COPILOT_PROVIDER_BASE_URL", ep.v1())
            .with_env("COPILOT_PROVIDER_TYPE", "openai")
            .with_env(
                "COPILOT_PROVIDER_API_KEY_COMMAND",
                format!("printenv {}", task.gateway.key_env),
            );
    }
    Ok(inv)
}

/// Parses `copilot -p --output-format=json` stdout (see the module docs for
/// the assumed record shapes). End of stream after `session.idle` or any
/// assistant message counts as completion.
pub fn parse_jsonl(stdout: &[u8], clock: &dyn Clock) -> Parsed {
    let text = String::from_utf8_lossy(stdout);
    let mut log = EventLog::default();
    let mut starts: BTreeMap<String, (String, Option<u64>)> = BTreeMap::new();
    let mut last_message = String::new();
    let mut turns = 0u32;
    let mut settled = false;
    let mut failure: Option<String> = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            log.push_at(
                clock.now_millis(),
                RuntimeEventKind::Error {
                    message: format!("copilot event line is not JSON: {}", line.trim()),
                },
            );
            continue;
        };
        let ts_ms = str_at(&v, "timestamp").and_then(millis_from_rfc3339);
        let ts = ts_ms
            .map(rfc3339_from_millis)
            .unwrap_or_else(|| rfc3339_from_millis(clock.now_millis()));
        let data = &v["data"];
        match str_at(&v, "type") {
            Some("assistant.turn_start") => turns += 1,
            Some("assistant.usage") => log.push_ts(
                ts,
                RuntimeEventKind::ModelCall {
                    model: str_at(data, "model").unwrap_or("unknown").into(),
                    usage: Usage {
                        input_tokens: u64_at(data, "inputTokens"),
                        output_tokens: u64_at(data, "outputTokens"),
                        cached_input_tokens: u64_at(data, "cacheReadTokens"),
                        cache_creation_input_tokens: u64_at(data, "cacheWriteTokens"),
                    },
                    cost_usd: 0.0,
                },
            ),
            Some("tool.execution_start") => {
                starts.insert(
                    str_at(data, "toolCallId").unwrap_or_default().into(),
                    (str_at(data, "toolName").unwrap_or("unknown").into(), ts_ms),
                );
            }
            Some("tool.execution_complete") => {
                let id = str_at(data, "toolCallId").unwrap_or_default();
                let (tool, start) = starts
                    .remove(id)
                    .unwrap_or_else(|| ("unknown".into(), None));
                let duration_ms = match (start, ts_ms) {
                    (Some(a), Some(b)) => b.saturating_sub(a),
                    _ => 0,
                };
                let ok = data
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                log.push_ts(
                    ts,
                    RuntimeEventKind::ToolCall {
                        tool,
                        ok,
                        duration_ms,
                    },
                );
            }
            Some("assistant.message")
                if data.get("parentToolCallId").is_none_or(Value::is_null) =>
            {
                let t = str_at(data, "content").unwrap_or_default();
                if !t.trim().is_empty() {
                    last_message = t.to_string();
                    settled = true;
                    log.push_ts(ts, RuntimeEventKind::Message { text: t.into() });
                }
            }
            Some("session.idle") => settled = true,
            Some("session.error") => {
                let m = str_at(data, "message")
                    .unwrap_or("session error")
                    .to_string();
                log.push_ts(ts, RuntimeEventKind::Error { message: m.clone() });
                failure = Some(m);
            }
            _ => {}
        }
    }
    let usage = log.usage_sum();
    let result = match failure {
        Some(e) => Some(result(
            TaskStatus::Failed,
            &last_message,
            usage,
            0.0,
            turns,
            Some(e),
        )),
        None if settled => Some(result(
            TaskStatus::Completed,
            &last_message,
            usage,
            0.0,
            turns,
            None,
        )),
        None => None,
    };
    Parsed {
        events: log.into_events(),
        result,
    }
}

process_runtime!(
    /// `AgentRuntime` for Copilot CLI (`copilot -p --output-format=json`). Provisional.
    CopilotRuntime
);

#[async_trait::async_trait]
impl ProcessAdapter for CopilotRuntime {
    fn name(&self) -> &'static str {
        NAME
    }

    fn endpoint(&self, task: &AgentTask) -> Result<Option<GatewayEndpoint>, RuntimeError> {
        endpoint(task)
    }

    fn invocation(&self, task: &AgentTask) -> Result<Invocation, RuntimeError> {
        invocation(task)
    }

    async fn collect(
        &self,
        _provider: &dyn SandboxProvider,
        _sandbox: &Sandbox,
        _task: &AgentTask,
        output: &ExecOutput,
        clock: &dyn Clock,
    ) -> Result<Parsed, RuntimeError> {
        Ok(parse_jsonl(&output.stdout, clock))
    }
}
