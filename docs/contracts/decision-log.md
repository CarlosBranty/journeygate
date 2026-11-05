# Contract: decision log and gate history (Postgres)

The decision log is the calibration dataset of section 6: every triage decision (also in Shadow Mode and when the engine failed) next to the final label a human left in Jira. The same database stores the gate results the dashboard reads. Crate: `guard-store`; ports: `guard_core::ports::{DecisionLog, GateHistory}`.

## Connection and migrations

- `DATABASE_URL` (`postgres://user:pass@host:5432/guard`), sqlx `PgPool` (max 10 connections, acquire timeout 5 s).
- Migrations: `guard-store/migrations/<YYYYMMDDHHMMSS>_<name>.sql`, applied with `sqlx::migrate!("./migrations")` at `guard serve` startup. Migrations are forward-only and never edited after merge: a change is a new file.
- Queries use the runtime API (`sqlx::query` / `query_as` with binds), **not** the compile-time `query!` macros, so building never needs a database.
- Tests that need a live database read `DATABASE_URL`; when it is unset they print `skipped: DATABASE_URL not set` and return early (they pass). Each such test uses its own schema (`CREATE SCHEMA guard_test_<random>`, `SET search_path`) and drops it at the end.

## Schema (`20261101000001_decision_log.sql`)

### `triage_decisions` (append-only)

| Column | Type | From `TriageDecision` |
| --- | --- | --- |
| `id` | `bigint` identity PK | → `DecisionId` |
| `ticket_key` | `text` | `ticket_key` |
| `decided_at` | `timestamptz` | `decided_at` (RFC 3339) |
| `recorded_at` | `timestamptz` default `now()` | — |
| `mode` | `text` `shadow`/`active` | `mode` |
| `engine_model` | `text` null | `answers.model` |
| `engine_error` | `text` null | `engine_error` |
| `permutation_seed` | `bigint` null | `permutation_seed` (the `u64` reinterpreted as `i64`, bit-for-bit) |
| `decided_lane`, `decided_reasons` | `text`, `text[]` | `decided` (`agent`/`human`, reasons in snake_case) |
| `effective_lane`, `effective_reasons` | `text`, `text[]` | `effective` |
| `area`, `area_confidence` | `text`, `float8` | `answers.area` |
| `kind`, `kind_confidence` | `text`, `float8` | `answers.kind` |
| `risk_score`, `risk_confidence` | `float8` | `answers.risk` |
| `clear_e2e_criteria`, `touches_money_or_security`, `agent_suitable`, `changes_existing_flow` | `float8` | the Noul answers |
| `decision` | `jsonb` | the whole `TriageDecision` serialized with serde (source of truth; the columns are denormalized copies) |

Checks: answers columns are all NULL exactly when the engine failed; `engine_model` or `engine_error` is set.

### `human_labels` (append-only)

| Column | Type | From `HumanLabel` |
| --- | --- | --- |
| `id` | `bigint` identity PK | — |
| `ticket_key` | `text` | `ticket_key` |
| `labeled_at` | `timestamptz` | `labeled_at` |
| `area`, `kind`, `lane`, `flow_change` | `text`, `text`, `text`, `boolean` (all null) | same fields |
| `labels` | `text[]` | `labels` (sorted) |
| `source` | `text` `jira_webhook`/`manual` | `source` |

### `calibration` (view)

The latest decision per ticket (`DISTINCT ON (ticket_key) ORDER BY decided_at DESC, id DESC`) left-joined with the latest human label of that ticket. `DecisionLog::list_for_calibration` reads it:

```sql
SELECT * FROM calibration
WHERE ($1::timestamptz IS NULL OR decided_at >= $1)
  AND ($2::text IS NULL OR mode = $2)
  AND (NOT $3 OR human_label_id IS NOT NULL)
ORDER BY decided_at DESC, decision_id DESC
LIMIT $4;              -- CalibrationQuery.limit, 0 => 1000
```

`CalibrationRow.decision` is deserialized from `decision`; `human` is rebuilt from the `human_*` columns (`None` when `human_label_id` is NULL).

### `gate_results`

| Column | Type | From |
| --- | --- | --- |
| `id` | `bigint` identity PK | → `GateResultId` |
| `received_at` | `timestamptz` default `now()` | → `StoredGateResult.received_at` |
| `repo` | `text` (`''` when `GateReport.repo` is `None`) | `report.repo` |
| `run_id` | `text` | `report.run_id` |
| `provider`, `pr_number`, `ticket_key`, `head_sha`, `base_sha`, `verdict` | | `report.*` (`pr_number` = `report.pr.number`) |
| `regressions` | `integer` | `report.counts.regressions` |
| `report` | `jsonb` | the whole `GateReport` |
| `outcome` | `jsonb` | the `ReportOutcome` |

`UNIQUE (repo, run_id)`: `record_gate_result` is an upsert (`INSERT ... ON CONFLICT (repo, run_id) DO UPDATE SET report = EXCLUDED.report, outcome = EXCLUDED.outcome, received_at = now(), ...  RETURNING id`), so a re-run finalize keeps the id.

- `recent_gate_results(limit)`: `ORDER BY received_at DESC, id DESC LIMIT $1`.
- `latest_for_pr(repo, n)`: `WHERE repo = $1 AND pr_number = $2 ORDER BY received_at DESC, id DESC LIMIT 1`.

## Error mapping

| sqlx error | `StoreError` |
| --- | --- |
| `PoolTimedOut`, `Io`, `Tls`, `PoolClosed` | `Unavailable` |
| `Database` (constraint violation), `ColumnDecode`, serde failure | `Invalid` |
| `DATABASE_URL` unset / unparsable | `Unconfigured` |

## Retention

Rows are never deleted by guard. The table is small (one row per ticket triage and per gate run); pruning, if ever needed, is an operator task.
