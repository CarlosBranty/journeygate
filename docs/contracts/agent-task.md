# Contract: in-sandbox file contract (`/task`)

Every runtime (Claude Code, Codex CLI, Copilot CLI, mini-swe-agent, OpenHands, and the `fake` runtime used in tests) speaks the same three files inside the sandbox. The orchestrator writes the input, the runtime writes the outputs, and the orchestrator reads them after the process exits. The diff is never reported by the runtime: the orchestrator extracts it from the worktree (`SandboxProvider::extract_diff`, sandbox.md).

| File | Writer | Shape | Rust type | Schema |
| --- | --- | --- | --- | --- |
| `/task/task.json` | orchestrator, before start | one JSON object | `guard_core::ports::AgentTask` | `schemas/agent-task.schema.json` |
| `/task/events.jsonl` | runtime, appended per step | one JSON object per line | `RuntimeEvent` | `schemas/agent-event.schema.json` |
| `/task/result.json` | runtime, once at the end | one JSON object | `TaskResult` | `schemas/agent-result.schema.json` |

Examples (and the fixtures of the Rust round-trip test `ports::fakes::phase4_tests::task_json_example_round_trips`): `schemas/examples/agent-task.example.json`, `agent-events.example.jsonl`, `agent-result.example.json`. A change to a field is a change to the Rust type, the schema and the example in one commit; the Rust test and `node --test "schemas/*.test.mjs"` both fail otherwise.

`/task` is the only writable mount besides the role's code mount (sandbox.md). Paths are constants in `guard_core::ports::agent`: `TASK_DIR`, `TASK_FILE`, `EVENTS_FILE`, `RESULT_FILE`.

## `task.json`

All fields are required; nullable ones are written as `null`.

| Field | Meaning |
| --- | --- |
| `version` | `1` (`TASK_SCHEMA_VERSION`) |
| `run_id`, `ticket`, `repo` | Identity of the run; `repo` is `owner/name` |
| `role` | `qa` or `coder` |
| `attempt`, `attempt_policy.max_attempts` | 1-based attempt; `1 <= max_attempts <= 3` (`MAX_ATTEMPTS`) |
| `base_commit` | Commit the worktree was checked out at (lowercase hex) |
| `summary`, `acceptance_criteria` | From the ticket (markup stripped, `triage::strip_markup`) |
| `allowed_paths.write`, `.deny` | Globs (globset, `literal_separator = true`) the diff may touch; `deny` wins. Built with `AllowedPaths::qa` / `AllowedPaths::coder` (qa-coder-protocol.md) |
| `sealed_spec` | `{path, sha256}` for `coder`; `null` for `qa` |
| `model`, `budget_usd`, `timeout_secs` | From `[agents.<name>]` (agents-toml.md) |
| `gateway.base_url`, `.key_env`, `.via_gateway` | Model endpoint; **name** of the env var holding the virtual key; `false` only for the Copilot exception |
| `tools` | The fixed tool set of the task, in a stable order (part of the cached prompt prefix, tokens.md) |
| `facts_brief` | `{text, fact_ids, truncated}` or `null`; `text` at most 4096 UTF-8 bytes (memory.md) |
| `failure_context` | Retries only: summary and trace excerpt of what failed last attempt. Never journey sources |

`AgentTask::validate` (called by every runtime before starting, and by `FakeAgentRuntime`) additionally enforces: `coder` has a `sealed_spec` with a 64-hex digest and its path in `allowed_paths.deny`; `qa` has none; `budget_usd` positive and finite; `attempt` allowed by the policy; `key_env` is an env var name (`^[A-Z][A-Z0-9_]*$`). A violation is `RuntimeError::InvalidTask` and the runtime never starts.

**No credential ever appears in `task.json`.** The virtual key reaches the process as the env var named by `gateway.key_env` (sandbox `secrets`, never written to disk).

## `events.jsonl`

One object per line, UTF-8, `\n`-terminated, `seq` strictly increasing from `1`, `ts` RFC 3339 UTC. `kind` selects the fields; unknown kinds and extra fields are invalid.

| `kind` | Fields |
| --- | --- |
| `model_call` | `model`, `usage {input_tokens, output_tokens, cached_input_tokens, cache_creation_input_tokens}`, `cost_usd` |
| `tool_call` | `tool`, `ok`, `duration_ms` |
| `message` | `text` (truncated by the runtime to 2000 bytes) |
| `error` | `message` |

Usage semantics: `cached_input_tokens` = cache reads (OpenAI `prompt_tokens_details.cached_tokens`, Anthropic `cache_read_input_tokens`); `cache_creation_input_tokens` = Anthropic cache writes; both `0` when the provider reports nothing. The runtime adapter maps each runtime's native stream (e.g. `claude -p --output-format stream-json`, Copilot `--output-format=json`) into these events. Tool calls to `guard_history` / `guard_map` are what the A/B protocol counts as memory and map reads (tokens.md).

## `result.json`

| Field | Meaning |
| --- | --- |
| `version` | `1` |
| `status` | `completed` (agent believes it is done; the gate decides), `failed`, `budget_exceeded`, `timed_out`, `refused` |
| `summary` | At most 4000 characters, shown in the PR body |
| `usage`, `cost_usd`, `steps` | Totals; the orchestrator cross-checks against `spend_records` |
| `memory_proposals` | Optional, at most 5 `{topic_key, body, evidence?}`. They only reach the human-approval candidate queue (memory.md) |
| `error` | Optional, present when `status != completed` |

A missing or invalid `result.json` after the process exits is `RuntimeError::Protocol`; a process killed at `timeout_secs` is `RuntimeError::Timeout`; a crash with no result is `RuntimeError::Crashed { exit_code, stderr_tail }`. `AgentOutcome` = `{runtime, result, events, exit_code, duration_ms}`.
