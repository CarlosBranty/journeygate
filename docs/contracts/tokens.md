# Contract: model gateway, caching, spend and the token A/B

The self-hosted LiteLLM proxy is the only model endpoint a sandbox reaches (sandbox.md). Port: `guard_core::ports::ModelGateway` (`mint_key`, `revoke`, `spend_logs`). Adapter: `guard-runtimes` (`LiteLlmGateway`, wiremock-tested). Fake: `FakeModelGateway`. No provider is ever called from tests; everything below is verified only with fakes and recorded fixtures until real credentials exist.

Sources checked on 2026-11-01 (docs.litellm.ai): `cache_control_injection_points` (tutorials/prompt_caching, LiteLLM blog "save Claude Code costs"), `cache_params.mode: default_off` (proxy/caching_controls), `/spend/logs/v2` (proxy/cost_tracking and the API reference), `/key/generate` rate-limit fields (proxy/users). Items marked **spike** are not verified.

## Virtual keys

One key per (run, role, attempt), or per (experiment, arm, task) in the A/B. `KeyRequest` → `POST /key/generate`:

| `KeyRequest` | LiteLLM body |
| --- | --- |
| `alias` (`<run_id>:<role>:<attempt>`) | `key_alias` |
| `models` | `models` |
| `max_budget_usd` | `max_budget` |
| `rpm_limit`, `tpm_limit`, `max_parallel_requests` | same names |
| `duration` | `duration` as `"<seconds>s"` |
| `metadata` (`run_id`, `ticket`, `role`, `arm`, `repo`, `commit`) | `metadata` |

`VirtualKey.token` (`Secret`) is injected into the sandbox as `[gateway].key_env`; `key_id` is used by `revoke` (`POST /key/delete {"keys": [...]}`), always called when the attempt ends. Keys an orchestrator crash left behind are revoked by `guard serve`'s `key reaper` (`GET /key/list`; only keys guard minted: alias `a<unix>-<ticket>:<qa|coder>:<attempt>[:...]` and, when present, matching `run_id` metadata, so other keys on a shared proxy, A/B experiment keys included, are never touched; a key whose run has no live task-queue lease, once older than the longest agent `timeout_secs` + `KEY_GRACE_SECS`, or past `expires`). The test-only fake gateway (`deploy/litellm/fake-gateway.mjs`) answers `/key/list` too. The master key comes from the env var named by `[gateway].master_key_env` and is used only by the orchestrator.

## Spend ingestion

`GET /spend/logs/v2?key_alias=<alias>&start_date=<YYYY-MM-DD HH:MM:SS>&end_date=...&page=<n>&page_size=<<=1000>` (response `{data, total, page, page_size, total_pages}`), paged until `page = total_pages`, upserted into `spend_records` by `request_id` (idempotent). `SpendRecord` normalizes `spend` → `spend_usd`, `prompt_tokens`, `completion_tokens`, cache reads → `cached_tokens` (OpenAI `prompt_tokens_details.cached_tokens`, Anthropic `cache_read_input_tokens`), `cache_creation_input_tokens`, the gateway response-cache flag → `cache_hit`, `startTime`/`endTime` → RFC 3339, key `metadata` (gives `run_id`, `arm`). Runs after every attempt and every 15 minutes; `[scheduler].daily_usd_cap` sums `spend_usd` of the current UTC day. Recorded fixtures: `guard-runtimes/tests/fixtures/litellm/` (to be captured from a local LiteLLM with a fake provider, never a real one).

Where response usage carries cache fields (`usage.prompt_tokens_details.cached_tokens`, `usage.cache_creation_input_tokens`), the runtime adapter copies them into `events.jsonl` `model_call.usage`; spend logs remain the source of truth for cost.

## LiteLLM configuration (deploy)

```yaml
model_list:
  - model_name: anthropic/claude-sonnet-4-5
    litellm_params:
      model: anthropic/claude-sonnet-4-5
      api_key: os.environ/ANTHROPIC_API_KEY
      cache_control_injection_points:      # Anthropic-native deployments only
        - location: message
          role: system
  - model_name: openai/gpt-5
    litellm_params:
      model: openai/gpt-5
      api_key: os.environ/OPENAI_API_KEY   # caching is automatic; we send prompt_cache_key
  - model_name: gemini/gemini-2.5-pro
    litellm_params:
      model: gemini/gemini-2.5-pro
      api_key: os.environ/GEMINI_API_KEY   # implicit caching; NO injection points

litellm_settings:
  cache: true
  cache_params:
    type: redis
    mode: default_off                      # exact-match cache only when a request opts in
    namespace: guard                       # + per-request namespace (below)
  callbacks: ["otel"]

router_settings:
  optional_pre_call_checks: ["prompt_caching"]
```

Rules:

- **Anthropic** (`anthropic/` deployments): `cache_control_injection_points` on the system message. LiteLLM adds `cache_control: {"type": "ephemeral"}` beside client marks and skips points that would exceed Anthropic's 4 cached blocks. Not configured for Bedrock/Vertex Claude until verified.
- **Gemini 2.5+**: no injection points (implicit caching); explicit context caching is out of scope.
- **OpenAI**: automatic prompt caching; runtimes send `prompt_cache_key = "<repo>:<task kind>"` (e.g. `acme/shop:coder`) so requests of one repo and role share a cache shard.
- **Exact-match response cache**: `default_off`; only orchestrator-owned keys (judges, summarizers, triage replays — never agent loops) opt in per request with `cache: {"namespace": "<repo>@<commit>"}` so a hit can never cross repos or commits.
- **Deterministic prefixes**: the system prompt, `AGENTS.md`, the tool list (`task.tools`, fixed per task, stable order) and the facts brief (sorted, memory.md) come first and are byte-identical across attempts of a task; volatile content (failure context, timestamps) comes after them. Changing any of these mid-task is a contract violation the harness asserts (hash of the prefix recorded per attempt).
- **Spike** (before enabling Anthropic context editing): verify LiteLLM forwards the `anthropic-beta` header and the `context_management` body field to native `anthropic/` deployments. Until verified, context editing stays off.
- **OpenHands**: `LLMSummarizingCondenser` with its own `usage_id` (e.g. `condenser`) so its spend is separable — **to confirm in the adapter** against the OpenHands SDK version pinned there.

## Telemetry

Claude Code exports OTel with `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`, `OTEL_EXPORTER_OTLP_ENDPOINT=<orchestrator collector>` (the collector host is an extra egress rule only when OTel is enabled; it is not a model endpoint). LiteLLM's `otel` callback and `guard-server` GenAI spans go to the same collector; GenAI semantic conventions are pinned by commit of `open-telemetry/semantic-conventions` in the server crate. Spans carry `run_id`, `arm` and `key_alias` attributes, never prompt text.

## Output compression

Sandbox images ship `rtk-oss` (not installed on the dev machine; version pinned in the image build). `guard mcp` and `guard test` outputs are **failures-first**: failing tests with file:line and a MinIO/S3 pointer (`evidence_uri`, presigned on demand) come before counts; passing tests are summarized as counts only.

## 4-arm A/B protocol

Pre-registered before any run; the registration (this section's numbers, the task list and the seeds) is committed as `experiments/<id>/preregistration.toml` before the first arm starts and never edited afterwards.

- **Arms (2x2 factorial):** `baseline`; `+memory` (facts brief + `guard_history`); `+map` (`guard_map`); `both`. Everything else identical: runtime, model, prompts, tool set (unavailable tools are absent, not disabled), budget, timeout, image digest.
- **Tasks:** the golden set (resolved tickets replayed at their base commit, judged by the sealed spec + guard) and fixed mini-swe-agent slices. Each (task, arm) runs `k = 3` times with seeds recorded; order randomized per block.
- **Keys:** one virtual key per (experiment, arm, task, repetition), alias `<experiment>:<arm>:<task>:<rep>`, so spend is attributable from `spend_records` alone.
- **Metrics:** resolution rate (guard green with the sealed spec); tokens and USD per resolved task; steps; cache hit rate = `cached_tokens / prompt_tokens`; memory reads (`guard_history` tool calls) and map reads; cited-in-diff share = fraction of `facts_brief_ids` / `guard_map` entries whose files appear in the final diff.
- **Pre-registered thresholds:** an arm "wins" over baseline only if resolution rate does not drop by more than 2 percentage points **and** USD per resolved task drops by at least 15 %, with a 95 % bootstrap CI (10 000 resamples over tasks) excluding 0 for the cost difference. Interaction (`both` vs the better single arm) must show >= 5 % further cost reduction to justify enabling both. Fewer than 30 tasks → result reported as inconclusive.
- **Report:** per-arm table of every metric with CIs, plus the raw `agent_runs` / `spend_records` export. Runs with infrastructure errors are retried once and otherwise counted as failures, never dropped.

### Harness (Phase 4, integrated)

`guard_agents::ab::run_experiment` runs every (task, rep) block with the four arms in a seeded random order, one `TicketPipeline` run per trial (`TicketInput.experiment`: one key alias `<experiment>:<arm>:<task>:r<rep>` for all attempts; brief and `guard_history` only in `memory`/`both`; `guard_map` only in `map`/`both`), writes `agent_runs` (`PgRunLog`, with `arm`, `steps`, `memory_reads`, `facts_brief_ids`, `usage`) and ingests each alias's spend. `guard agents ab-report --experiment <id>` renders the pre-registered report (`guard_store::spend_report`).

It has been run end to end **on fakes only** (`guard-agents/tests/ab_harness.rs`: 30 tasks x 3 reps x 4 arms on live Postgres, a fake runtime whose token use is scripted per arm). That proves the plumbing and the verdict rules, not any saving: **real numbers need real runtimes, a real gateway and provider credentials**, none of which exist on the development machine. `guard_map` reads and map-entry citations still have no `agent_runs` column.
