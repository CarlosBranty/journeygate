//! `claude-code`: Claude Code in print mode (`claude -p`).
//!
//! Sources (checked 2026-11-01; installed CLI `claude --help`, 2.1.316):
//! - Flags `-p`, `--bare`, `--output-format stream-json`, `--verbose`,
//!   `--model`, `--permission-mode dontAsk`, `--allowedTools`,
//!   `--max-budget-usd`, `--no-session-persistence`, `--settings`,
//!   `--mcp-config`, `--strict-mcp-config`,
//!   `--exclude-dynamic-system-prompt-sections`: `claude --help` and
//!   <https://code.claude.com/docs/en/headless> ("Use `--output-format
//!   stream-json` with `--verbose`"; "The last line of the stream is a
//!   `result` message"; `--bare` "is the recommended mode for scripted and SDK
//!   calls"; in bare mode auth is "strictly ANTHROPIC_API_KEY or apiKeyHelper
//!   via --settings").
//! - Gateway pinning: <https://code.claude.com/docs/en/llm-gateway> and
//!   `settings-reference#allowedproviders`: set `allowedProviders` to
//!   `["customEndpoint"]` **in a managed settings file** and put
//!   `ANTHROPIC_BASE_URL` in that file's `env` block; `customEndpoint` is
//!   admitted "only for the exact value a managed env block pins" (v2.1.285+).
//!   Managed settings on Linux live in `/etc/claude-code/` ([`MANAGED_SETTINGS_PATH`]),
//!   which is in the read-only image, not a writable mount: the image (or a
//!   read-only bind) must ship [`managed_settings`]; [`ClaudeCodeRuntime`]
//!   refuses to start (`Unavailable`) when the file is missing or pins another
//!   URL.
//! - Credential: `apiKeyHelper` = `printenv <key_env>` in `--settings`, so the
//!   virtual key is read from the env var the sandbox injects and never
//!   written to disk (`apiKeyHelper` sends it as both `x-api-key` and
//!   `Authorization: Bearer`, llm-gateway-connect).
//! - Stream messages: `system/init`, `assistant` (an Anthropic message; one API
//!   response can arrive as several `assistant` lines with the same
//!   `message.id` and the same `usage`, so usage is counted once per id),
//!   `user` (`tool_result` blocks), `result` (`subtype` `success`,
//!   `error_max_turns`, `error_during_execution`, `error_max_budget_usd`;
//!   `total_cost_usd`, `num_turns`, `usage`, `result`) — Agent SDK
//!   `SDKMessage` types (code.claude.com/docs/en/agent-sdk/typescript).
//!
//! Verified on 2026-11-01 with the installed `claude` 2.1.316 against a local
//! fake Messages server (no provider; fixture
//! `tests/fixtures/claude-code/recorded_fake_gateway.jsonl`): this argv runs
//! to `result/success`; the only request is `POST
//! $ANTHROPIC_BASE_URL/v1/messages?beta=true`; `apiKeySource` is
//! `apiKeyHelper` and the key from `printenv <key_env>` arrives in both
//! `x-api-key` and `Authorization`; Claude Code itself sends a
//! `context_management` body field and the `context-management-2025-06-27`
//! beta (see deploy/litellm/context-editing-spike.md). Not verified: the
//! managed-settings `allowedProviders` refusal (needs a root-owned
//! `/etc/claude-code`).
//!
//! Only Claude models: Anthropic does not route other models through a
//! gateway (llm-gateway). `cost_usd` per `model_call` is 0: the stream has no
//! per-call cost; the result's `total_cost_usd` (client estimate) and
//! `spend_records` carry the cost.

use crate::clock::Clock;
use crate::driver::{ProcessAdapter, process_runtime};
use crate::endpoint::GatewayEndpoint;
use crate::events::{EventLog, Parsed, result, str_at, u64_at};
use crate::invocation::Invocation;
use crate::prompt;
use guard_core::ports::agent::{AgentTask, RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_core::ports::sandbox::{ExecOutput, Sandbox, SandboxError, SandboxProvider};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const NAME: &str = "claude-code";
/// Where Claude Code reads managed settings on Linux.
pub const MANAGED_SETTINGS_PATH: &str = "/etc/claude-code/managed-settings.json";
pub const SETTINGS_PATH: &str = "/task/claude/settings.json";
pub const MCP_CONFIG_PATH: &str = "/task/claude/mcp.json";
/// Built-in tools every task gets, in a stable order (deterministic prefix).
/// `--bare` exposes exactly these three (verified: `system/init.tools` is
/// `["Bash", "Edit", "Read"]` even with `--tools` naming more).
pub const BUILTIN_TOOLS: &[&str] = &["Read", "Edit", "Bash"];

/// The managed settings the image must install at [`MANAGED_SETTINGS_PATH`].
pub fn managed_settings(gateway_root: &str) -> Value {
    json!({
        "allowedProviders": ["customEndpoint"],
        "env": { "ANTHROPIC_BASE_URL": gateway_root }
    })
}

pub fn invocation(task: &AgentTask) -> Result<Invocation, RuntimeError> {
    let ep = GatewayEndpoint::pin(&task.gateway)?;
    if !task.model.to_ascii_lowercase().contains("claude") {
        return Err(RuntimeError::InvalidTask(format!(
            "claude-code runs only Claude models, not {:?}",
            task.model
        )));
    }
    let mut allowed: Vec<String> = BUILTIN_TOOLS.iter().map(|s| s.to_string()).collect();
    allowed.extend(task.tools.iter().map(|t| format!("mcp__guard__{t}")));
    let mut argv: Vec<String> = [
        "claude",
        "-p",
        "--bare",
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        &task.model,
        "--permission-mode",
        "dontAsk",
        "--allowedTools",
        &allowed.join(","),
        "--max-budget-usd",
        &format!("{}", task.budget_usd),
        "--no-session-persistence",
        "--exclude-dynamic-system-prompt-sections",
        "--settings",
        SETTINGS_PATH,
        "--strict-mcp-config",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let settings = json!({ "apiKeyHelper": format!("printenv {}", task.gateway.key_env) });
    let mut inv = Invocation::new(vec![]);
    if !task.tools.is_empty() {
        argv.extend(["--mcp-config".to_string(), MCP_CONFIG_PATH.to_string()]);
        let mcp = json!({
            "mcpServers": { "guard": { "type": "stdio", "command": "guard", "args": ["mcp"] } }
        });
        inv = inv.with_file(MCP_CONFIG_PATH, pretty(&mcp));
    }
    inv.argv = argv;
    inv.stdin = Some(prompt::render(task));
    Ok(inv
        .with_file(SETTINGS_PATH, pretty(&settings))
        .with_env("ANTHROPIC_BASE_URL", ep.root())
        .with_env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"))
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("json");
    s.push('\n');
    s
}

/// Parses `--output-format stream-json` stdout. Lines that are not JSON
/// become `error` events; unknown message types are ignored.
pub fn parse_stream(stdout: &[u8], clock: &dyn Clock) -> Parsed {
    let text = String::from_utf8_lossy(stdout);
    let mut log = EventLog::default();
    let mut seen_messages = BTreeSet::new();
    let mut tool_names: BTreeMap<String, String> = BTreeMap::new();
    let mut last_text = String::new();
    let mut outcome = None;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let now = clock.now_millis();
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            log.push_at(
                now,
                RuntimeEventKind::Error {
                    message: format!("claude stream line is not JSON: {}", line.trim()),
                },
            );
            continue;
        };
        match str_at(&v, "type") {
            Some("assistant") => {
                let msg = &v["message"];
                let id = str_at(msg, "id").unwrap_or_default().to_string();
                if seen_messages.insert(id) {
                    if let Some(u) = msg.get("usage") {
                        log.push_at(
                            now,
                            RuntimeEventKind::ModelCall {
                                model: str_at(msg, "model").unwrap_or("unknown").to_string(),
                                usage: anthropic_usage(u),
                                cost_usd: 0.0,
                            },
                        );
                    }
                }
                for block in msg["content"].as_array().into_iter().flatten() {
                    match str_at(block, "type") {
                        Some("text") => {
                            let t = str_at(block, "text").unwrap_or_default();
                            if !t.trim().is_empty() {
                                last_text = t.to_string();
                                log.push_at(now, RuntimeEventKind::Message { text: t.into() });
                            }
                        }
                        Some("tool_use") => {
                            tool_names.insert(
                                str_at(block, "id").unwrap_or_default().into(),
                                str_at(block, "name").unwrap_or("unknown").into(),
                            );
                        }
                        _ => {}
                    }
                }
            }
            Some("user") => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if str_at(block, "type") == Some("tool_result") {
                        let id = str_at(block, "tool_use_id").unwrap_or_default();
                        let tool = tool_names
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| "unknown".into());
                        let ok = !block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        log.push_at(
                            now,
                            RuntimeEventKind::ToolCall {
                                tool,
                                ok,
                                duration_ms: 0,
                            },
                        );
                    }
                }
            }
            Some("result") => outcome = Some(v),
            _ => {}
        }
    }
    let result = outcome.map(|r| {
        let subtype = str_at(&r, "subtype").unwrap_or("unknown");
        let is_error = r.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let status = match subtype {
            "success" if !is_error => TaskStatus::Completed,
            "error_max_budget_usd" => TaskStatus::BudgetExceeded,
            _ => TaskStatus::Failed,
        };
        let text = str_at(&r, "result").unwrap_or(&last_text).to_string();
        let usage = r
            .get("usage")
            .map(anthropic_usage)
            .unwrap_or_else(|| log.usage_sum());
        let error = (status != TaskStatus::Completed).then(|| {
            if subtype == "success" {
                text.clone()
            } else {
                subtype.to_string()
            }
        });
        result(
            status,
            &text,
            usage,
            r.get("total_cost_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
            u64_at(&r, "num_turns") as u32,
            error,
        )
    });
    Parsed {
        events: log.into_events(),
        result,
    }
}

fn anthropic_usage(u: &Value) -> Usage {
    Usage {
        input_tokens: u64_at(u, "input_tokens"),
        output_tokens: u64_at(u, "output_tokens"),
        cached_input_tokens: u64_at(u, "cache_read_input_tokens"),
        cache_creation_input_tokens: u64_at(u, "cache_creation_input_tokens"),
    }
}

process_runtime!(
    /// `AgentRuntime` for Claude Code (`claude -p`, stream-json).
    ClaudeCodeRuntime
);

#[async_trait::async_trait]
impl ProcessAdapter for ClaudeCodeRuntime {
    fn name(&self) -> &'static str {
        NAME
    }

    fn invocation(&self, task: &AgentTask) -> Result<Invocation, RuntimeError> {
        invocation(task)
    }

    /// The managed settings must pin `customEndpoint` and this task's gateway.
    async fn preflight(
        &self,
        provider: &dyn SandboxProvider,
        sandbox: &Sandbox,
        task: &AgentTask,
    ) -> Result<(), RuntimeError> {
        let root = GatewayEndpoint::pin(&task.gateway)?.root().to_string();
        let bytes = match provider.read_file(sandbox, MANAGED_SETTINGS_PATH).await {
            Ok(b) => b,
            Err(SandboxError::NotFound(_)) => {
                return Err(RuntimeError::Unavailable(format!(
                    "{MANAGED_SETTINGS_PATH} is missing from the image; it must pin allowedProviders [\"customEndpoint\"] and ANTHROPIC_BASE_URL"
                )));
            }
            Err(e) => return Err(e.into()),
        };
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| {
            RuntimeError::Unavailable(format!("{MANAGED_SETTINGS_PATH} is not JSON: {e}"))
        })?;
        if v["allowedProviders"] != json!(["customEndpoint"]) {
            return Err(RuntimeError::Unavailable(format!(
                "{MANAGED_SETTINGS_PATH}: allowedProviders must be exactly [\"customEndpoint\"]"
            )));
        }
        if str_at(&v["env"], "ANTHROPIC_BASE_URL") != Some(root.as_str()) {
            return Err(RuntimeError::Unavailable(format!(
                "{MANAGED_SETTINGS_PATH}: env.ANTHROPIC_BASE_URL must be the gateway {root}"
            )));
        }
        Ok(())
    }

    async fn collect(
        &self,
        _provider: &dyn SandboxProvider,
        _sandbox: &Sandbox,
        _task: &AgentTask,
        output: &ExecOutput,
        clock: &dyn Clock,
    ) -> Result<Parsed, RuntimeError> {
        Ok(parse_stream(&output.stdout, clock))
    }
}
