# Contract: `guard mcp` tools

`guard mcp` is a Model Context Protocol server over stdio so that the developer's assistant (Claude Code, Codex, Copilot, Gemini CLI, OpenCode) can run and read the tests (section 17). Crate: `guard-mcp`; `guard-cli` adds the `mcp` subcommand by calling `guard_mcp::run_stdio(actions: Arc<dyn GuardActions>)`, with actions that run `guard` as `current_exe()` subprocesses (stdout captured, never a user shell).

**It exposes nothing the guard forbids.** There is no tool to update snapshots, edit or delete journeys or the catalog, change `target.toml`/`thresholds.toml`, mark tests as skipped/quarantined, or bypass/override the guard verdict. Adding such a tool is a contract violation; the tests assert the exact tool list.

## Protocol

- JSON-RPC 2.0, one message per line (newline-delimited) on stdin/stdout; logs go to stderr only.
- `initialize` → `{"protocolVersion": "2025-06-18", "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "guard", "version": "<crate version>"}}`. A client asking for an older version gets `2025-06-18` back (the client decides).
- `notifications/initialized` (no response), `ping` → `{}`, `tools/list`, `tools/call`. Anything else → error `-32601`.
- `tools/call` result: `{"content": [{"type": "text", "text": "<the output JSON, pretty-printed>"}], "structuredContent": <output object>, "isError": false}`. A tool-level failure (bad config, app not ready, Playwright crash) is `isError: true` with `structuredContent = {"error": "<code>", "detail": "<text>", "exit_code": <guard exit code>}`. Invalid arguments (schema mismatch) are JSON-RPC error `-32602`.
- Every tool declares `inputSchema` and `outputSchema` (JSON Schema draft 2020-12) exactly as below, and `annotations`: `readOnlyHint` (true for all but `guard_test`/`guard_repro`), `destructiveHint: false`, `openWorldHint` (true only for `guard_repro`, which talks to `guard serve`).
- Tools work in the repo the server was started in (`current_dir`), and run the guard commands in-process or as `current_exe()` subprocesses; they never shell out through a user shell. Long runs (`guard_test`, `guard_repro`) have a 30-minute timeout → `isError` with `error: "timeout"`.

Error codes: `config_error` (exit 2), `environment_error` (exit 3), `not_found`, `server_not_configured`, `server_error`, `timeout`.

## Tools

### `guard_affected`

Journeys affected by the current diff and why (path or Jev). Same selection as `guard affected --json` (cli.md) plus the Phase 2 impact reasons (ports.md, "Phase 2 changes").

Input:

```json
{"type": "object", "additionalProperties": false, "properties": {
  "base": {"type": "string", "description": "Base ref for the diff; omitted = detected (cli.md, Base resolution)"}
}}
```

When `base` is omitted the tool runs guard without `--base`, so the base is detected (cli.md, Base resolution).

Output: the `guard affected --json` object:

```json
{"type": "object", "required": ["base", "base_source", "merge_base", "branch", "ticket", "changed_files", "tags", "journeys", "impact"], "properties": {
  "base": {"type": "string"},
  "base_source": {"enum": ["flag", "env", "ci", "remote_head", "remote_branch", "local_branch"]},
  "merge_base": {"type": "string"},
  "branch": {"type": ["string", "null"]},
  "ticket": {"type": ["string", "null"]},
  "changed_files": {"type": "array", "items": {"type": "string"}},
  "tags": {"type": "array", "items": {"type": "string"}},
  "journeys": {"type": "array", "items": {"type": "object", "required": ["id", "tag", "reasons"], "properties": {
    "id": {"type": "string"}, "tag": {"type": "string"},
    "reasons": {"type": "array", "items": {"type": "object", "required": ["kind"], "properties": {
      "kind": {"enum": ["path", "smoke", "impact", "all", "requested", "engine_unavailable"]},
      "file": {"type": "string"}, "glob": {"type": "string"}, "probability": {"type": "number"}
    }}}
  }}},
  "impact": {"type": "object", "required": ["source", "status"], "properties": {
    "source": {"enum": ["server", "local", "none"]},
    "status": {"enum": ["ok", "engine_unavailable", "not_configured"]},
    "threshold": {"type": "number"}
  }}
}}
```

### `guard_test`

Runs tests headless and returns a structured result: what passed, what failed, exact line and screenshot path.

Input:

```json
{"type": "object", "additionalProperties": false, "properties": {
  "selection": {"enum": ["affected", "all", "journeys"], "default": "affected"},
  "journeys": {"type": "array", "items": {"type": "string"}, "description": "Journey ids; required when selection = journeys"},
  "base": {"type": "string", "description": "Base for selection = affected; omitted = detected"},
  "docker": {"type": "boolean", "default": false}
}}
```

No `headed`/`ui`/`debug` (an assistant cannot use a window) and no `update_snapshots`.

Output:

```json
{"type": "object", "required": ["exit_code", "counts", "duration_ms", "tags", "failures"], "properties": {
  "exit_code": {"type": "integer"},
  "counts": {"type": "object", "required": ["passed", "flaky", "failed", "skipped"], "properties": {
    "passed": {"type": "integer"}, "flaky": {"type": "integer"}, "failed": {"type": "integer"}, "skipped": {"type": "integer"}}},
  "duration_ms": {"type": "integer"},
  "tags": {"type": "array", "items": {"type": "string"}},
  "failures": {"type": "array", "items": {"$ref": "#/$defs/failure_summary"}}
},
"$defs": {"failure_summary": {"type": "object", "required": ["test_id", "title", "file", "line", "status", "error"], "properties": {
  "test_id": {"type": "string", "description": "<project>::<file>::<title>"},
  "title": {"type": "string"}, "file": {"type": "string"}, "line": {"type": "integer"},
  "journey": {"type": ["string", "null"]},
  "status": {"enum": ["failed", "flaky"]},
  "error": {"type": "string"},
  "location": {"type": ["string", "null"], "description": "file:line of the failing step"},
  "screenshot": {"type": ["string", "null"], "description": "Repo-relative path under e2e-playwright/test-results/"}
}}}}
```

Exit `1` (tests failed) is a normal result (`isError: false`); exit `2`/`3` are `isError: true`.

### `guard_failure`

Detail of one failure from the last local run (`results/report.json`) or, with `pr`, from the latest remote gate (`GET /api/pr-context?number=<pr>`).

Input:

```json
{"type": "object", "additionalProperties": false, "required": ["test_id"], "properties": {
  "test_id": {"type": "string"},
  "pr": {"type": "integer", "minimum": 1}
}}
```

Output:

```json
{"type": "object", "required": ["test_id", "source", "title", "file", "line", "error", "steps", "attempts"], "properties": {
  "test_id": {"type": "string"},
  "source": {"enum": ["local", "remote"]},
  "title": {"type": "string"}, "file": {"type": "string"}, "line": {"type": "integer"},
  "journey": {"type": ["string", "null"]},
  "location": {"type": ["string", "null"]},
  "error": {"type": "string", "description": "Full error message, ANSI stripped"},
  "steps": {"type": "array", "items": {"type": "string"}, "description": "Titles of the steps up to the failing one"},
  "attempts": {"type": "array", "items": {"type": "object", "properties": {
    "retry": {"type": "integer"}, "status": {"type": "string"}, "duration_ms": {"type": "integer"}}}},
  "baseline": {"enum": ["regression", "pre_existing", "flaky", "unavailable", null]},
  "fingerprint": {"type": ["string", "null"]},
  "screenshot": {"type": ["string", "null"], "description": "Local path, or presigned URL for remote"},
  "trace_url": {"type": ["string", "null"], "description": "Local trace.zip path, or the trace.playwright.dev viewer URL"},
  "jira": {"type": ["string", "null"], "description": "Bug key for remote failures"}
}}
```

Unknown `test_id` → `isError` `not_found`. `pr` without `[server]` → `server_not_configured`.

### `guard_repro`

Reproduces a remote guard failure locally: same as `guard repro <test-id> --pr <n>` (below).

Input:

```json
{"type": "object", "additionalProperties": false, "required": ["test_id", "pr"], "properties": {
  "test_id": {"type": "string"}, "pr": {"type": "integer", "minimum": 1}
}}
```

Output:

```json
{"type": "object", "required": ["reproduced", "exit_code", "evidence_dir", "remote", "local"], "properties": {
  "reproduced": {"type": "boolean", "description": "The local run failed the same way (same fingerprint)"},
  "exit_code": {"type": "integer"},
  "evidence_dir": {"type": "string", "description": ".guard/repro/<fingerprint>/ with the downloaded screenshot and trace"},
  "remote": {"$ref": "#/$defs/failure_ref"},
  "local": {"oneOf": [{"$ref": "#/$defs/failure_ref"}, {"type": "null"}]}
},
"$defs": {"failure_ref": {"type": "object", "required": ["status", "fingerprint"], "properties": {
  "status": {"enum": ["passed", "flaky", "failed"]}, "fingerprint": {"type": ["string", "null"]}, "error": {"type": ["string", "null"]}}}}}
```

### `guard_catalog`

Searches journeys by area, tag or text. Read-only view of `catalog/journeys.json`.

Input:

```json
{"type": "object", "additionalProperties": false, "properties": {
  "area": {"type": "string"},
  "tag": {"type": "string", "description": "e.g. @smoke or @journey:checkout.customer-card"},
  "text": {"type": "string", "description": "Case-insensitive match on id, title and impact_question"},
  "include_quarantined": {"type": "boolean", "default": true}
}}
```

Filters combine with AND; no filter = every journey.

Output:

```json
{"type": "object", "required": ["journeys"], "properties": {"journeys": {"type": "array", "items": {"type": "object",
  "required": ["id", "area", "title", "tags", "paths", "smoke", "quarantined"], "properties": {
  "id": {"type": "string"}, "area": {"type": "string"}, "title": {"type": "string"},
  "tags": {"type": "array", "items": {"type": "string"}, "description": "@journey:<id>, @area:<area>, plus @smoke when smoke"},
  "paths": {"type": "array", "items": {"type": "string"}},
  "smoke": {"type": "boolean"}, "quarantined": {"type": "boolean"}
}}}}}
```

Sorted by `id`.

## `guard repro <test-id> --pr <n>` (CLI, Phase 2)

1. Requires `[server]`: `GET /api/pr-context?number=<n>` → `latest_gate` (none = exit `2`, `no gate result for PR <n>`); find `tests[]` with `test_id` (absent = exit `2`).
2. Download the bundle's screenshot and trace (presigned URLs) into `.guard/repro/<fingerprint>/`.
3. Check out nothing: it runs against the current working tree (the dev checks out the PR head first; a mismatch between `HEAD` and the gate's `head_sha` prints `warning: HEAD <sha> differs from the gate's head <sha>`).
4. `guard test --docker --journey <journey>` (local browsers, with a warning, when `docker info` fails or the Playwright version is unknown) (or, for a test without a journey, the spec file via `--shard`-free direct run of `<file>`), with `RUN_ID=repro-<fingerprint>`. The seed is derived from the Playwright test id, so the same test reruns with the same randomness.
5. Print the local status and compare fingerprints. Exit `0` if reproduced (failed with the same fingerprint), `1` if the local run passed or failed differently, `2`/`3` as usual.

## Phase 4 additions

Two read-only tools join the exact tool list, which is now `guard_affected`, `guard_test`, `guard_failure`, `guard_repro`, `guard_catalog`, `guard_history`, `guard_map` (in `tools/list` order). Their schemas are pinned to memory.md and guard-map.md by the tests next to their definitions; the five Phase 2 tools stay pinned to this document. Wiring (`guard mcp`, guard-cli): `guard_history` gets a `HistoryContext` from `PgMemoryFactsReader::from_env()` when `GUARD_MEMORY_READ_DATABASE_URL` is set; `guard_map` serves `GuardImpactSource` at `HEAD` (the default backend), wrapped in `HidePaths::for_coder` when `GUARD_MCP_ROLE=coder`. Without a graph it answers `environment_error` (exit 3). Invalid arguments of either tool are JSON-RPC `-32602`.

- `guard_history`: orchestrator memory facts, paginated, read through a `MemoryFactsReader` only (memory.md, "guard_history MCP tool"). Requires `GUARD_MEMORY_READ_DATABASE_URL`; without it the tool returns `isError` with `error: "server_not_configured"`.
- `guard_map`: token-budgeted repository map (guard-map.md).

Both: `readOnlyHint: true`, `destructiveHint: false`, `openWorldHint: false`. There is still no tool that records, approves or invalidates a fact.
