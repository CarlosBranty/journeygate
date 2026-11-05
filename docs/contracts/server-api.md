# Contract: `guard serve` HTTP API

`guard serve` is the service mode (section 8, deployment 2): Jira/GitHub/GitLab webhooks, triage, the impact API used by `guard test --affected`, the gate results reported by `guard gate finalize`, and the read-only dashboard (section 15). Crate: `guard-server` (axum). It talks to the outside world only through `guard_core::ports`.

## Running

```text
guard serve [--listen <addr>] [--repo <path>] [--thresholds <path>] [--fake]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--listen` | `GUARD_LISTEN`, else `0.0.0.0:8080` | Bind address. |
| `--repo` | current directory | Checkout of the app repo: `target.toml`, `e2e-playwright/catalog/journeys.json` and the source tree the `ImpactAnalyzer` indexes. |
| `--thresholds` | see thresholds-toml.md | `thresholds.toml`. |
| `--fake` | off | Fake mode (below): `GUARD_JEV_FAKE=1`, `GUARD_JIRA_FAKE=1` and, unless `DATABASE_URL` is set, `DATABASE_URL=memory://`. |

Startup fails (exit `2`) when `target.toml`, the catalog or `thresholds.toml` is invalid, or `GUARD_SERVER_TOKEN` is unset. A missing adapter configuration does not stop the server: the routes that need it answer `501 not_configured` (except the engine, see `/api/impact`).

### Environment

Secrets only come from the environment, never from files in the repo.

| Variable | Used by | Required |
| --- | --- | --- |
| `GUARD_SERVER_TOKEN` | Bearer token for every `/api/*` route | yes |
| `GUARD_DASHBOARD_TOKEN` | Dashboard auth (below) | no |
| `DATABASE_URL` | `DecisionLog`, `GateHistory` (Postgres); with Postgres also the memory writers and the staleness job (below) | yes for triage and gate results |
| `GUARD_STALENESS_INTERVAL` | memory staleness job period: `900`, `900s`, `15m`, `2h` (default 15 min; an unreadable value warns and keeps the default) | no |
| `GUARD_STORAGE_URL` | `EvidenceStore` (format in finalize-phase2.md) | no |
| `TYPESAFE_API_KEY`, `JEV_URL` | `DecisionEngine` | key yes for triage/impact |
| `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN` | `IssueTracker` | yes for Jira |
| `JIRA_SUGGESTED_FIELD` | Shadow Mode suggestion field (e.g. `customfield_10123`) | no |
| `GUARD_JIRA_WEBHOOK_SECRET` | `/webhooks/jira` | yes for that route |
| `GITHUB_TOKEN`, `GITHUB_REPOSITORY` (`owner/name`), `GITHUB_API_URL` (default `https://api.github.com`) | `CodeHost` (GitHub) | when `[code_host].kind = "github"` |
| `GUARD_GITHUB_WEBHOOK_SECRET` | `/webhooks/github` | yes for that route |
| `GITLAB_TOKEN`, `GITLAB_PROJECT` (path or numeric id), `GITLAB_API_URL` (default `https://gitlab.com/api/v4`) | `CodeHost` (GitLab) | when `[code_host].kind = "gitlab"` |
| `GUARD_GITLAB_WEBHOOK_SECRET` | `/webhooks/gitlab` | yes for that route |

### Fake mode

For demos and end-to-end tests without a Jev key, a Jira site or a database. Never on by default; `guard serve` prints what it wired (`engine fake, tracker fake, store memory, ...`).

| Variable | Effect |
| --- | --- |
| `GUARD_JEV_FAKE=1` | Deterministic engine: triage answers the first area option whose key the ticket text names (else the first option), kind `bugfix`, low risk, every Noul on the agent side of the default thresholds (confidence 0.9); impact is `0.9` when a changed path contains the journey's area (the id before the first `.`), else `0.05`. |
| `GUARD_JIRA_FAKE=1` | In-memory Jira (`guard_core::ports::fakes::InMemoryTracker`). `/webhooks/jira` still requires `GUARD_JIRA_WEBHOOK_SECRET`; the delivered issue is stored so `get_issue` finds it. Bugs get keys `<PROJECT>-1001`, ... |
| `DATABASE_URL=memory://` | `guard_store::MemoryStore` for `DecisionLog` and `GateHistory` (lost on exit). |
| `GUARD_STORAGE_URL=file:///<dir>` | `guard_store::FsEvidenceStore` (not fake-only; also usable in CI-native mode). |

Without `TYPESAFE_API_KEY` and without `GUARD_JEV_FAKE` the server still starts: every engine call fails with `EngineError::Unconfigured`, so triage records `engine_error` and goes to the human lane, and `/api/impact` answers `503`.

### Agent lane (Phase 4)

With `agents.toml` present, a Postgres `DATABASE_URL` and thresholds in **active** mode, `guard serve` also runs the agent lane: `AppState.agent_lane` (`guard_server::AgentLane`, implemented by guard-cli's `QueueLane`) receives every *audited* lane-A ticket after triage and enqueues `(ticket, qa)`; a worker loop in the same process runs QA, Coder, gate and PR (qa-coder-protocol.md). Shadow Mode, the human lane and a decision the log refused never reach it; a failed hand-off is logged and leaves the ticket to humans. Every agent PR requires human approval; nothing is merged. `--fake` (or `GUARD_AGENTS_FAKE=1`) uses the scripted fake runtime. The startup line reports `agents active`, `agents fake runtime` or `agents disabled`, and `agents: PRs: ...` says where agent PRs go (pr-publisher.md, "Fake and wiring").

**Memory and periodic jobs.** With a Postgres `DATABASE_URL`, `guard serve` registers the memory writers (`AppState::with_memory`, memory.md "Writers": failure, fingerprint and flaky facts after `POST /api/gate-results`, `decision/<ticket>` after a human label overrides triage; written in the background, errors only logged) and the `memory staleness` job (memory.md "Staleness", every `GUARD_STALENESS_INTERVAL`). The default branch of the `--repo` checkout is the default base (cli.md "Base resolution" steps 4-6), else the current branch, else HEAD; the job does not fetch, so keep the checkout current. With the agent lane it also runs `sandbox reaper`, `key reaper` (revokes gateway keys of finished or unknown lane runs once older than the longest agent `timeout_secs` + 600 s) and `network reaper` every 5 minutes. Each job runs once at startup; the startup line reports `memory postgres` or `memory disabled`, then `serve: jobs: <names>`. Job failures, and summaries that changed since the previous run, go to stderr as `jobs: <name>: ...`.

### Rust surface (`guard-server`)

```rust
pub struct AppState {
    pub engine: Arc<dyn DecisionEngine>,
    pub tracker: Option<Arc<dyn IssueTracker>>,
    pub code_host: Option<Arc<dyn CodeHost>>,
    pub decisions: Option<Arc<dyn DecisionLog>>,
    pub gates: Option<Arc<dyn GateHistory>>,
    pub evidence: Option<Arc<dyn EvidenceStore>>,
    pub impact: Arc<dyn ImpactAnalyzer>,
    pub config: TargetConfig,
    pub catalog: Catalog,
    pub thresholds: Thresholds,
    pub api_token: String,
    pub dashboard_token: Option<String>,
}

pub fn router(state: AppState) -> axum::Router;          // every route below; tests drive it with tower::ServiceExt::oneshot
pub async fn serve(options: ServeOptions, state: AppState) -> Result<(), ServeError>; // binds, serves until Ctrl-C
```

`guard-server` depends on no adapter crate. `guard-cli serve` reads the environment, builds the adapters (`guard-jev`; `guard-tracker` with `JiraTracker::load_self_account_id` at startup; `guard-codehost`; `guard-store` with migrations; `guard_impact::ImpactIndex::build`), plugs `guard_tracker::report::report_gate` in as the `GateReporter` (screenshots read back from the `EvidenceStore`, `JIRA_PRE_EXISTING_PRIORITY` honored) and calls `guard_server::serve`. `AppState` also carries `reporter`, `dashboard_auth`, `clock` and `tasks` (injection points with defaults; see `AppState::new` and its `with_*` builders). A `DATABASE_URL` that cannot be reached or migrated is exit `3`.

## Conventions

- JSON in and out (`application/json`), UTF-8. Request bodies over the limit: `413`.
- Body limits: webhooks and `/api/impact` 1 MiB; `/api/gate-result` 8 MiB.
- Every error from `/api/*` and `/webhooks/*` is `{"error": "<code>", "detail": "<human text>"}`:

| HTTP | `error` | When |
| --- | --- | --- |
| 400 | `bad_request` | Malformed JSON, schema mismatch, missing query parameter. |
| 401 | `unauthorized` | Missing/invalid bearer token, webhook signature or webhook token. |
| 404 | `not_found` | Unknown PR, unknown route. |
| 413 | `payload_too_large` | Body over the limit. |
| 501 | `not_configured` | The adapter the route needs is not configured. |
| 503 | `engine_unavailable` | Any `EngineError` (fail closed: the caller runs every journey). |
| 503 | `store_unavailable` | `StoreError::Unavailable`. |
| 500 | `internal` | Anything else. Never leaks secrets. |

- Tokens and secrets are compared in constant time (`subtle`).

## Auth

| Routes | Mechanism |
| --- | --- |
| `/api/*` | `Authorization: Bearer <GUARD_SERVER_TOKEN>`. The CLI reads the same variable. |
| `/webhooks/jira` | `Authorization: Bearer <GUARD_JIRA_WEBHOOK_SECRET>` (Jira Automation "Send web request" header) **or** query `?secret=<GUARD_JIRA_WEBHOOK_SECRET>` (Jira admin webhooks, which cannot set headers). Implemented in `IssueTracker::parse_webhook`. |
| `/webhooks/github` | `X-Hub-Signature-256: sha256=<hex HMAC-SHA256(raw body, GUARD_GITHUB_WEBHOOK_SECRET)>`. `CodeHost::verify_webhook`. |
| `/webhooks/gitlab` | `X-Gitlab-Token: <GUARD_GITLAB_WEBHOOK_SECRET>`. `CodeHost::verify_webhook`. |
| `/healthz` | none |
| Dashboard | If `GUARD_DASHBOARD_TOKEN` is set: `Authorization: Bearer <token>`, or cookie `guard_dashboard=<token>`, or `?token=<token>` (sets that cookie `HttpOnly; SameSite=Strict` and redirects to the same path without the query). Unset: open (deploy behind the internal network). GitHub/GitLab OAuth login is Phase 3. |

## Webhooks

### `POST /webhooks/jira`

| Jira event | Server action | Response |
| --- | --- | --- |
| `jira:issue_created` | Triage the issue in a background task (flow in `guard_core::triage` module docs): `get_issue` → `resolve_mentions` → `fan_out` → `triage` → `decide` → `record_decision` → `add_labels` (active) or `suggest_labels` (shadow). | `202 {"accepted": true, "event": "issue_created", "key": "SHOP-7"}` |
| `jira:issue_updated` with a `labels` change by someone other than guard's own Jira user | `record_human_label(triage::human_label_from_labels(...))`. Label changes made by guard's API user come back as `Ignored` (the adapter compares `user.accountId` with `GET /rest/api/3/myself`), so guard's own writes never pollute the ground truth. | `202 {"accepted": true, "event": "labels_changed", "key": "SHOP-7"}` |
| anything else | nothing | `200 {"accepted": false, "reason": "..."}` |

Triage is idempotent per delivery only: a re-delivered `issue_created` triages again and records a second decision (the decision log is append-only; calibration reads the latest one per ticket).

### `POST /webhooks/github`

Verified with `CodeHost::verify_webhook`, parsed with `CodeHost::parse_webhook`.

| `X-GitHub-Event` | Response |
| --- | --- |
| `ping` | `200 {"accepted": true, "event": "ping"}` |
| `pull_request` (`opened`, `synchronize`, `reopened`, `edited`, `closed`) | `202 {"accepted": true, "event": "pull_request", "number": 12}`. Phase 2 only logs it; PRs go straight to the guard from CI (section 12). Phase 3 uses it for the agent merge flow. |
| anything else | `200 {"accepted": false, "reason": "..."}` |

### `POST /webhooks/gitlab`

Same as GitHub with `X-Gitlab-Event: Merge Request Hook` (`object_attributes.action`: `open`→`opened`, `update`→`synchronize`, `reopen`→`reopened`, `close`→`closed`, `merge`→`merged`).

## API

### `POST /api/impact`

Used by `guard test --affected` when `[server].url` is set (the server holds the only Jev key).

Request:

```json
{
  "diff": {
    "files": [{"path": "examples/demo-shop/src/cart/cart.mjs", "status": "modified", "additions": 4, "deletions": 1}],
    "symbols": ["examples/demo-shop/src/cart/cart.mjs#addToCart"],
    "hunks": [{"path": "examples/demo-shop/src/cart/cart.mjs", "header": "@@ -10,6 +10,9 @@ export function addToCart", "lines": ["+  if (qty < 1) throw new Error('qty');"]}],
    "truncated": false
  },
  "journeys": [
    {"id": "checkout.customer-card", "impact_question": "This diff can directly or indirectly change how ..."}
  ]
}
```

- `journeys`: 0 to 1000 entries, unique ids. The CLI sends every **active** journey of its local catalog (the server never second-guesses the client's catalog).

Response `200`:

```json
{"model": "jev-1.13.0", "threshold": 0.25, "impact": {"checkout.customer-card": 0.81}}
```

- `impact` has exactly one entry per requested id. `threshold` is `thresholds.impact_threshold`; the client selects `impact > threshold`.
- Any engine error: `503 {"error": "engine_unavailable", ...}`. The CLI then runs every active journey (fail closed) and prints `impact: engine unavailable, running every journey`.

### `POST /api/gate-result`

Sent by `guard gate finalize` when `[server].url` is set. Body: `guard_core::ports::GateReport` (version `2`, see finalize-phase2.md). The server:

1. Runs the Jira reporting use case (`guard_tracker::report::report_gate`, reporting.md), reading screenshots for attachments from the `EvidenceStore` by `EvidenceFile.key`.
2. Stores the report and the outcome (`GateHistory::record_gate_result`, idempotent on `(repo, run_id)`).
3. Answers `200`:

```json
{"id": 17, "outcome": {"bugs": [{"fingerprint": "1a2b3c4d", "key": "SHOP-210", "url": "https://acme.atlassian.net/browse/SHOP-210", "action": "created"}], "ticket_comment": "SHOP-123", "pr_comment": false, "status_published": false, "warnings": []}}
```

The server never publishes the check run / commit status or the PR comment: `finalize` does that itself with the CI token (finalize-phase2.md). Reporting failures go to `outcome.warnings` and never fail the request; only an invalid body (`400`) or an unavailable store (`503`) do.

### `GET /api/pr-context?number=<n>`

Used by `guard gate finalize` (flow-change status of the ticket) and `guard repro --pr <n>` (latest evidence).

Response `200`:

```json
{
  "pr": { "provider": "github", "repo": "acme/shop", "number": 12, "title": "SHOP-123 fix cart", "url": "...", "author": "dev", "ticket_key": "SHOP-123", "branch": "SHOP-123-fix-cart", "base_branch": "main", "base_sha": "<40 hex>", "head_sha": "<40 hex>", "changed_files": ["..."] },
  "ticket": {"key": "SHOP-123", "url": "https://acme.atlassian.net/browse/SHOP-123", "flow_change": false},
  "latest_gate": null
}
```

- `pr`: `CodeHost::pr_context` with `[tracker].ticket_pattern`.
- `ticket`: `null` when there is no ticket key or no tracker; `flow_change` = the issue has the `flow-change` label.
- `latest_gate`: `GateHistory::latest_for_pr(repo, n)` (a `StoredGateResult`) or `null`.
- `400` without `number`; `404` unknown PR; `501` without a code host.

### `GET /healthz`

Always `200` while the process runs:

```json
{"status": "ok", "version": "0.1.0", "checks": {"database": "ok", "engine": "configured", "tracker": "configured", "code_host": "configured", "evidence": "disabled"}}
```

`database`: `ok` | `unavailable` | `disabled`; the others: `configured` | `disabled`. No secrets, no network calls except one `SELECT 1`.

## Dashboard (read-only HTML)

Server-rendered with askama, refreshed with htmx (no SPA, no frontend build). `text/html; charset=utf-8`. With `HX-Request: true` a route returns only its `<main>` fragment. No route writes anything.

| Route | View | Source |
| --- | --- | --- |
| `GET /` | Overview: last 10 gates, links to every view | `GateHistory::recent_gate_results(10)` |
| `GET /catalog` | Journeys by area: id, title, tags, smoke, quarantined, last result | catalog + latest gate results |
| `GET /coverage` | `[areas]` with 0 or 1 active journeys | `target.toml` × catalog |
| `GET /flaky` | Tests flaky in the last 50 gate results: flaky rate, quarantined flag | `recent_gate_results(50)` |
| `GET /durations` | Gate wall time, slowest spec files, most expensive tests | `recent_gate_results(50)` (`duration_ms`) |
| `GET /guards` | Last 50 PRs/MRs: verdict, regressions, modified journeys, links to Jira and evidence | `recent_gate_results(50)` |
| `GET /decisions` | Triage vs. final human label, agent-lane precision | `DecisionLog::list_for_calibration` |
| `GET /agents` | Placeholder `No agent runs yet` until Phase 3 | — |

Without `DATABASE_URL`, data views render an empty state (`Decision log not configured`) with `200`, never an error page.
