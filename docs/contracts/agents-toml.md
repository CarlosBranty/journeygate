# Contract: `agents.toml`

Configuration of the agent lane (architecture section 16): gateway, which agent plays each role, per-agent runtime/model/budget, and the scheduler. Lives at the root of the app repo next to `target.toml`. Parsed and validated by `guard-agents`; the JSON form of the document must satisfy `schemas/agents.schema.json` (example: `schemas/examples/agents.example.json`). Unknown keys are errors. Credentials appear only as env var **names**.

```toml
version = 1

[gateway]
url = "http://llm-gateway:4000"         # the only model endpoint a sandbox can reach
master_key_env = "LITELLM_MASTER_KEY"   # orchestrator only: mints and revokes virtual keys
key_env = "GUARD_GATEWAY_KEY"           # env var the virtual key is injected as (default)
key_duration_secs = 7200                # virtual key `duration` (default 7200)
rpm_limit = 60                          # defaults for every key; an agent may override
tpm_limit = 400000
max_parallel_requests = 2

[roles]
qa = "qa"                               # the [agents.<name>] that plays each role
coder = "coder"

[agents.qa]
role = "qa"
runtime = "claude-code"
model = "claude-opus-5-5"
budget_usd = 2.0
max_attempts = 1

[agents.coder]
role = "coder"
runtime = "mini-swe-agent"
model = "openai/gpt-5"                  # any model the gateway exposes
budget_usd = 5.0
max_attempts = 3                        # 1..=3, default 3
timeout_secs = 1800                     # default 1800

[agents.coder-copilot]                  # optional second coder for best-of-N
role = "coder"
runtime = "copilot-cli"
model = "anthropic/claude-sonnet-4-5"   # BYOK through the gateway
via_gateway = true                      # false = Copilot-hosted models, an explicit exception
budget_usd = 5.0

[scheduler]
host_slots = 4                          # concurrent sandboxes on this host
daily_usd_cap = 50.0                    # UTC day, summed from spend_records
provider_slots = { anthropic = 3, openai = 3 }
lease_secs = 300                        # task_queue lease; heartbeat at lease/3
image = "ghcr.io/acme/guard-sandbox@sha256:<64 hex>"

[scheduler.best_of_n]                   # optional
agents = ["coder", "coder-copilot"]     # at least 2
label = "agent-best-of-n"               # only tickets with this label; absent = never
select = "first_green_smallest_diff"
```

## Keys

| Key | Type | Rule |
| --- | --- | --- |
| `version` | int | `1` |
| `[gateway].url` | URL | `http(s)://`; becomes the single `gateway` egress rule (sandbox.md) and `task.json` `gateway.base_url` |
| `[gateway].master_key_env` | env name | Read by the orchestrator only; never enters a sandbox |
| `[gateway].key_env` | env name | Default `GUARD_GATEWAY_KEY` |
| `[gateway].key_duration_secs` | int >= 60 | `KeyRequest.duration` |
| `[gateway].rpm_limit`, `tpm_limit`, `max_parallel_requests` | int >= 1 | Defaults of `KeyRequest`; `[agents.<name>]` may override each |
| `[roles].qa`, `[roles].coder` | agent name | Must name an agent whose `role` matches |
| `[agents.<name>]` | table | Name `^[a-z][a-z0-9-]*$` |
| `.role` | `qa` \| `coder` | |
| `.runtime` | enum | `claude-code`, `codex`, `copilot-cli`, `mini-swe-agent`, `openhands`, `fake` (tests only; rejected by `guard agents` outside `GUARD_ALLOW_FAKE_RUNTIME=1`) |
| `.model` | string | Gateway model name. `claude-code` accepts only Claude models (Anthropic does not route other models through a gateway) |
| `.budget_usd` | number > 0 | `max_budget` of the virtual key minted per attempt |
| `.max_attempts` | 1..=3 | `AttemptPolicy`; default 3. QA usually 1 |
| `.timeout_secs` | int >= 60 | `task.json` `timeout_secs` and the sandbox wall clock |
| `.via_gateway` | bool | Default `true`. `false` is allowed only with `runtime = "copilot-cli"` and requires `copilot_token_env` (normally `COPILOT_GITHUB_TOKEN`); it adds the `copilot_exception` egress rule and the budget is Copilot's org policy, not the key |
| `.copilot_token_env` | env name | See above |
| `.image` | `repo@sha256:<64 hex>` | Overrides `[scheduler].image` |
| `[scheduler].host_slots` | int >= 1 | Slot 1 of 3 (section 16) |
| `[scheduler].daily_usd_cap` | number > 0 | Slot 3: no new attempt starts once today's spend reaches it; running attempts finish |
| `[scheduler].provider_slots` | map provider → int | Slot 2: concurrent attempts per model prefix (`anthropic/...` → `anthropic`; a bare Claude model counts as `anthropic`) |
| `[scheduler].lease_secs` | int >= 30 | `TaskQueue::claim` lease |
| `[scheduler].best_of_n.agents` | 2+ unique agent names, same role | Run the same ticket on each; the winner is the first to pass the guard, ties broken by the smallest diff (changed lines). Doubles the cost |

Validation beyond the schema (Rust, `guard-agents`): `[roles]` targets exist and match roles; `best_of_n` agents share a role; `via_gateway = false` only with `copilot-cli` and `copilot_token_env`; at least one agent per role.

## `guard agents` subcommands

`guard agents` (Phase 4, `guard-cli/src/commands/agents.rs`):

| Command | Does | Exit |
| --- | --- | --- |
| `guard agents check` | Validates `agents.toml` (`GUARD_AGENTS_TOML`, default `<repo>/agents.toml`); warns about provisional runtimes | 0 / 2 |
| `guard agents sync [--check] [--runtime <name>]...` | Renders `agents/skills/**/SKILL.md` and `AGENTS.md` per runtime (default: the runtimes `agents.toml` uses, else claude-code, codex, copilot-cli) into `.guard/agents/rendered/<runtime>/` plus the `guard mcp` registrations; a copy is mounted read-only at `/agents` in each sandbox (sandbox.md). The renderer is part of `guard`, not installed in the sandbox | 0; `--check` 1 when out of date |
| `guard agents run <TICKET> [--runtime <name>] [--fake] [--ticket-file <json>] [--area <key>]... [--enqueue-only] [--timeout <s>] [--json]` | Enqueues `(ticket, qa)` and runs a worker in-process until the PR branch (0) or the human lane / failure (1). The ticket comes from Jira (`[tracker]`) or `--ticket-file` (an `Issue` as JSON); the Coder may write only the paths of its `area:<key>` labels / `--area` | 0 / 1 / 2 / 3 |
| `guard agents status [--ticket <KEY>] [--json]` | Queue tasks and `agent_runs` of the repo | 0 / 3 |
| `guard agents ab-report --experiment <id> [--out <file>]` | The 4-arm A/B report (tokens.md) from `agent_runs` + `spend_records` | 0 / 3 |

`--fake` is the explicit opt-in that `GUARD_ALLOW_FAKE_RUNTIME=1` gives, and it also switches every agent to the scripted `fake` runtime (plans from `GUARD_AGENT_FAKE_PLANS/<KEY>/<role>-<attempt>.json`, else `<role>.json`). `guard serve` works lane-A tickets only with `agents.toml`, Postgres and thresholds in active mode; `guard serve --fake` (or `GUARD_AGENTS_FAKE=1`) uses the fake runtime.

Environment of the agent lane (names only, never values in files): `DATABASE_URL` (Postgres, required), `GUARD_MEMORY_READ_DATABASE_URL` (a `guard_memory_reader` login for the pipeline's reader and `guard_history`; without it the pipeline reads through the writer's `read_only` view), `[gateway].master_key_env`, `GUARD_GATEWAY_ADMIN_URL` (where the orchestrator reaches the gateway admin API when it differs from the in-sandbox `[gateway].url`), `GUARD_SANDBOX_EGRESS_NETWORK`, `GUARD_SANDBOX_RUNTIME` (`auto` / `runc` / `runsc`), `GUARD_AGENT_WORK_DIR`, `GUARD_AGENT_KEEP_WORK` (`1`: keep each attempt dir after the attempt; removed by default), `GUARD_REPO` (else `GITHUB_REPOSITORY`, `GITLAB_PROJECT`, the origin URL, `local/<app name>`). Runtimes that read the key from a fixed variable get it there too (`guard_runtimes::secret_env_aliases`, e.g. `LITELLM_PROXY_API_KEY` for mini-swe-agent).
