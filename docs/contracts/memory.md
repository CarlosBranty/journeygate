# Contract: orchestrator memory (`memory_facts`)

Durable, evidence-backed facts about a repo, its areas and its journeys, written **only by the orchestrator** and read by agents through a byte-capped brief and the read-only `guard_history` MCP tool. Ports: `guard_core::ports::{MemoryFactsWriter, MemoryFactsReader}` (`ports/memory_facts.rs`). Postgres adapter: `guard-store`. Fake: `InMemoryMemoryFacts`. Migration: `guard-store/migrations/20261101000003_agent_lane.sql`.

## Who writes

Enforced at three layers, not by convention:

1. **Types.** The write side is `MemoryFactsWriter`, held only by the orchestrator (`guard serve` workers). `guard mcp`, the facts-brief builder and the harness receive `Arc<dyn MemoryFactsReader>` (`ports::read_only(writer)` or a reader adapter); the wrapper exposes no write method and cannot be downcast back.
2. **Database roles.** `guard_memory_reader` has `SELECT` on `memory_facts` only. The reader adapter connects with the URL in `GUARD_MEMORY_READ_DATABASE_URL`, whose login is a member of `guard_memory_reader` and nothing else. `guard_orchestrator` has `SELECT, INSERT, UPDATE` (never `DELETE`) on `memory_facts`, `memory_fact_candidates`, `task_queue`, `agent_runs`, `spend_records`. Tested live in `guard-store/tests/agent_lane_schema.rs`.
3. **Network.** Sandboxes cannot reach Postgres at all (sandbox.md). Agents read memory only via the brief in `task.json` and, when `guard_history` is in `task.tools`, via `guard mcp` running in the orchestrator's trust zone with the reader URL.

Agents **propose** facts in `result.json` `memory_proposals`; the orchestrator stores them with `enqueue_candidate` in `memory_fact_candidates`; a human approves (`approve_candidate` → fact with `source = approved_candidate`) or rejects. Agents never read candidates.

### Database roles

The migration creates `guard_orchestrator` and `guard_memory_reader` as `NOLOGIN` group roles when the migrating user has `CREATEROLE` (it tolerates concurrent creation), and grants on the tables of the schema it runs in. Without `CREATEROLE` it emits a `NOTICE` and the DBA runs, once per cluster:

```sql
CREATE ROLE guard_orchestrator NOLOGIN;
CREATE ROLE guard_memory_reader NOLOGIN;
CREATE ROLE guard_mcp LOGIN PASSWORD '<from the secret store>' IN ROLE guard_memory_reader;
```

and re-runs the migration's grant block. Grants follow the table owner; `PUBLIC` has nothing.

## Schema

| Column | Type | Rule |
| --- | --- | --- |
| `id` | bigint identity | `FactId` |
| `repo` | text | `owner/name` |
| `scope` | `repo` \| `area` \| `journey` | `scope_key` NULL iff `repo` (`[areas]` key or journey id otherwise) |
| `topic_key` | text | `^[a-z0-9][a-z0-9._/-]*$`, <= 120, e.g. `flaky/checkout-pay`, `selector/product-card` |
| `body` | text | 1..=1024 bytes (`MAX_FACT_BODY_BYTES`), one claim in plain English |
| `commit_sha` | text | commit the fact was observed at |
| `run_id`, `evidence_uri` | text | `evidence_uri` is a pointer (`s3://<bucket>/<key>`), never inline bytes |
| `source` | enum | `gate_result`, `failure_fingerprint`, `flaky_history`, `fix_commit`, `approved_decision`, `approved_candidate` |
| `valid_from`, `invalid_at` | timestamptz | **valid time**: when the fact holds for the code; `invalid_at` set iff `status = invalidated` |
| `recorded_at`, `updated_at` | timestamptz | **transaction time**: when the store learned / last changed lifecycle |
| `review_after` | timestamptz | staleness deadline |
| `status` | `active` \| `needs_review` \| `invalidated` | |
| `review_reason` | text | why it is `needs_review` / `invalidated` |
| `supersedes` | bigint → `memory_facts` | the version this one replaced |
| `referenced_paths` | text[] | repo-relative files whose change makes it stale (GIN indexed) |

Invariants: one non-invalidated fact per `(repo, scope, scope_key, topic_key)` (partial unique index); rows are **append-only** — a trigger rejects `DELETE`, any change to the claim columns, and reviving an invalidated row. A correction is a new row: `record_fact` invalidates the live version (`invalid_at = new.valid_from`) and sets `supersedes`. "What did we believe at time T about the code at time V" = filter on `recorded_at <= T` and `valid_from <= V < invalid_at`.

`memory_fact_candidates`: `id`, `repo`, `fact jsonb` (a `NewFact`), `proposed_by` (`<runtime>:<role>`), `run_id`, `created_at`, `decision` (`pending`/`approved`/`rejected`), `decided_by`, `decided_at`, `reason`, `fact_id` (set iff approved).

## Writers (orchestrator)

| Writer | Trigger | Fact |
| --- | --- | --- |
| Gate results | `GateHistory::record_gate_result` with a blocking failure | `journey:<id>` `failure/<fingerprint label>`: what failed and where (file:line), `evidence_uri` = trace |
| Failure fingerprints | a fingerprint seen in >= 2 runs | `journey:<id>` `fingerprint/<label>` with the normalized error |
| Flaky history | `FlakyHistory` flags a test flaky over the window | `journey:<id>` `flaky/<test id>`, `review_after = now + 14 d` |
| Fix commits | a merged PR that turned a recorded failure green | `area:<key>` `fix/<fingerprint label>`: the commit and touched files (`referenced_paths`) |
| Approved decisions | a human label overriding a triage decision | `repo` `decision/<ticket>` |
| Candidates | human approval | as proposed, `source = approved_candidate` |

Every writer passes `commit_sha`, `run_id` and `evidence_uri` when it has them. `record_fact` is idempotent: re-recording the live fact of a topic with the same `body` and `commit_sha` returns its id and changes nothing, so writers may replay.

## Staleness (job, orchestrator)

`guard serve` runs it as the `memory staleness` job (at startup, then every `GUARD_STALENESS_INTERVAL`, default 15 minutes; server-api.md) against the `--repo` checkout's default branch (the default base (cli.md "Base resolution" steps 4-6), else the current branch, else HEAD), with the fan-out graph rebuilt from the checkout on every run. A fact becomes `needs_review` (`mark_needs_review(id, reason)`) when:

1. a commit since `commit_sha` touches any of its `referenced_paths`, or a file whose fan-out reaches one of them (`guard-impact` `ImpactAnalyzer::fan_out`); reason `"<path> changed in <short sha>"`;
2. `review_after < now`; reason `"review_after passed"`;
3. a `failure/` or `fingerprint/` fact's fingerprint is absent from the last `N = 20` gate runs of its journey; reason `"fingerprint absent for 20 runs"`.

A human `confirm`s it back to `active` or `invalidate`s it. A writer that sees the same claim again at a newer commit records it as a new `active` version at that commit (superseding the flagged one, so staleness measures from the new commit); seeing it again at the fact's own `commit_sha` (re-reading the recent gate results that still hold the old runs) changes nothing, so a writer never clears `needs_review` from old evidence. `needs_review` facts are still served, flagged; `invalidated` never are.

## Facts brief

Injected into `task.json` `facts_brief` at sandbox start (`FactsBrief::render(facts, FACTS_BRIEF_MAX_BYTES)`, implemented in `guard-core`):

- Input: `list_facts` for the ticket's repo, its areas and its affected journeys (statuses `active` + `needs_review`).
- Order: `(scope, scope_key, topic_key, id)` — deterministic, so the brief is a stable cached prefix (tokens.md).
- Format: the header `FACTS_BRIEF_HEADER`, then one line per fact: `- [<id>] <scope>[:<key>] <topic_key>: <body with whitespace collapsed>` + ` (needs_review)` when flagged.
- Cap: **4096 UTF-8 bytes**, hard. Rendering stops at the first line that would exceed it and sets `truncated = true`. `fact_ids` lists what was included (stored in `agent_runs.facts_brief_ids`, to measure citations).

## `guard_history` MCP tool

Added to `guard mcp` (mcp-tools.md conventions: `readOnlyHint: true`, `destructiveHint: false`, `openWorldHint: false`). Backed by a `MemoryFactsReader` only. Response well under 10k tokens: at most 50 facts per page, bodies are <= 1024 bytes, evidence is a pointer.

Input:

```json
{"type": "object", "additionalProperties": false, "properties": {
  "scope": {"enum": ["repo", "area", "journey"]},
  "scope_key": {"type": "string"},
  "topic_prefix": {"type": "string"},
  "topic_key": {"type": "string", "description": "Exact topic: return every version (topic_history)"},
  "include_invalidated": {"type": "boolean", "default": false},
  "as_of": {"type": "string", "description": "RFC 3339 valid-time point: the versions valid then (valid_from <= as_of < invalid_at), superseded ones included"},
  "cursor": {"type": "string"},
  "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 20}
}}
```

Output:

```json
{"type": "object", "required": ["repo", "facts", "next_cursor"], "properties": {
  "repo": {"type": "string"},
  "facts": {"type": "array", "maxItems": 50, "items": {"type": "object",
    "required": ["id", "scope", "topic_key", "body", "status", "needs_review", "commit_sha", "valid_from"],
    "properties": {
      "id": {"type": "integer"}, "scope": {"type": "string"}, "scope_key": {"type": ["string", "null"]},
      "topic_key": {"type": "string"}, "body": {"type": "string"},
      "status": {"enum": ["active", "needs_review", "invalidated"]}, "needs_review": {"type": "boolean"},
      "review_reason": {"type": ["string", "null"]},
      "commit_sha": {"type": "string"}, "run_id": {"type": ["string", "null"]},
      "evidence_uri": {"type": ["string", "null"]}, "evidence_url": {"type": ["string", "null"], "description": "presigned GET, 1 h"},
      "valid_from": {"type": "string"}, "invalid_at": {"type": ["string", "null"]},
      "supersedes": {"type": ["integer", "null"]}
    }}},
  "next_cursor": {"type": ["string", "null"]}
}}
```

The repo is the one `guard mcp` runs in (`target.toml` / git remote). `topic_key` set → `topic_history` (all statuses, oldest first, no paging); otherwise `list_facts`. Errors: `server_not_configured` when no reader URL is set.

## Engram export

`guard memory export --engram <file.json>` (integrator wires the CLI; logic in `guard-store`) writes the facts of a repo in the backup format `engram import <file.json>` reads. **Verified** on 2026-11-01 against `github.com/Gentleman-Programming/engram`, `internal/store/store.go` on `main`: `ExportData {version, exported_at, sessions, observations, prompts, relations?, prompt_tombstones?}`, `currentExportVersion = "0.2.0"` (export *format* version; Engram releases themselves are v3.x), `Session {id, project, ownership_mode?, directory, started_at, ended_at?, summary?}`, `Observation {id, sync_id, session_id, type, title, content, tool_name?, project?, scope, topic_key?, revision_count, duplicate_count, last_seen_at?, review_after?, superseded_by_sync_id?, created_at, updated_at, deleted_at?}`. Import dedupes on `sync_id` and skips rows whose `updated_at` is not newer.

Schema of what we emit: `schemas/engram-export.schema.json` (example `schemas/examples/engram-export.example.json`).

| Engram | From |
| --- | --- |
| `version` | `"0.2.0"` |
| one `session` | `id = "guard-<repo>"`, `project = repo`, `directory = ""`, `started_at` = oldest `recorded_at` |
| `observation.id` / `sync_id` | fact `id` / `"guard-fact-<id>"` (stable, so re-imports update in place) |
| `type` | `gate_result`, `flaky_history` → `discovery`; `failure_fingerprint`, `fix_commit` → `bugfix`; `approved_decision` → `decision`; `approved_candidate` → `pattern` |
| `title` / `content` | `topic_key` / `body` + lines `scope:`, `commit:`, `run:`, `evidence:`, `status:` |
| `project`, `scope` | `repo`, `"project"` |
| `topic_key` | `guard/<scope>[/<scope_key>]/<topic_key>` |
| `revision_count`, `duplicate_count` | `1`, `1` |
| `review_after` | `review_after` |
| `superseded_by_sync_id` | `"guard-fact-<id of the fact that supersedes it>"` |
| `created_at` / `updated_at` | `recorded_at` / `updated_at` |
| `deleted_at` | `invalid_at` for invalidated facts |
| `prompts` | `[]` (no relations, no tombstones) |

**Provisional:** the `type` mapping (Engram accepts free-form types; ours is a choice), whether Engram's importer requires the session to pre-exist (we always emit it), and the timestamp format it compares (`normalizeComparableTimestamp`; we emit RFC 3339 UTC). An import test against a real `engram` binary is a builder task; it has not been run.

## Agent-lane facts (Phase 4, integrated)

The orchestrator (`guard_agents::lane::Orchestrator`, run by `guard serve` workers and `guard agents run`) records, through the only `MemoryFactsWriter`:

| Topic | Scope | When | `referenced_paths` |
| --- | --- | --- | --- |
| `ticket/<key>/sealed-spec` | repo | QA sealed a spec that failed red on the base commit | the spec path |
| `ticket/<key>/agent-pr` | repo | the agent branch passed the gate with the sealed spec (awaiting approval) | the winning diff's paths |

Both use `source = gate_result` (orchestrator-run guard checks) and go through `memory_writers::observe`, so replays confirm instead of versioning. Agent `memory_proposals` only reach `memory_fact_candidates` (`enqueue_proposals`). The pipeline itself holds a reader: `PgMemoryFactsReader` when `GUARD_MEMORY_READ_DATABASE_URL` is set, else `read_only(writer)`. The brief is injected into `task.json` `facts_brief` at sandbox start, built once per stage (byte-identical across attempts); A/B arms without memory get none.

Verified live (guard-cli `agent_lane_e2e`): both facts are written, the Coder's `task.json` carries the sealed-spec fact in its brief, `guard_history` returns both through a `guard_memory_reader` login, and that login gets `permission denied` on `DELETE FROM memory_facts`.
