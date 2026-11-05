# Contract: `guard_map` (repository map)

A ranked, hard token-budgeted map of the files and definitions most relevant to the current ticket, for agents that would otherwise spend tokens exploring. Crate: `guard-map` (types and the `GraphSource` trait are final in `guard-map/src/lib.rs`). MCP exposure: a `guard_map` tool in `guard-mcp` (mcp-tools.md conventions; `readOnlyHint: true`, `destructiveHint: false`, `openWorldHint: false`).

## Backend-agnostic

Ranking runs over any `GraphSource`:

```rust
pub trait GraphSource: Send + Sync {
    fn backend(&self) -> &'static str;          // "guard-impact", "codegraph", ...
    fn commit(&self) -> &str;
    fn files(&self) -> Result<Vec<String>, MapError>;          // sorted, repo-relative
    fn edges(&self) -> Result<Vec<Edge>, MapError>;            // from imports/references to
    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError>;
}
```

Candidate backends: the `guard-impact` import graph (built today, TS/JS), a CodeGraph index (the read-only `/codegraph` mount, sandbox.md), a tree-sitter tag graph. The default is chosen **by benchmark** (the `+map` arm of the tokens.md A/B, per backend); until then the default is `guard-impact`. Adding a backend never changes the tool I/O. A `FakeGraphSource` for tests lives in `guard-map` tests.

## Algorithm

1. Nodes = `files()`, edges = `edges()` (parallel edges summed). Dangling nodes redistribute uniformly.
2. **Personalized PageRank**, damping `0.85`, at most 50 iterations or L1 delta < `1e-6`. Personalization: each `focus_files` entry weight 1.0; each file defining a `mentioned` identifier weight 0.5; uniform when both are empty. Unknown focus files are ignored (reported nowhere).
3. Sort by rank descending, ties by path ascending (deterministic).
4. Emit entries greedily: for each file, its top definitions (`symbols`, in source order, at most 8, rendered `name:line`). After adding an entry, serialize the whole response and measure `estimate_tokens` (= `ceil(bytes / 4)`); if over `token_budget`, drop that entry's symbols one by one, then the entry itself, and stop. `omitted` = ranked files not emitted.

The budget is **hard**: `estimated_tokens <= token_budget` always (asserted in tests over random graphs). `estimate_tokens` is deliberately tokenizer-free so results are reproducible across models.

## Tool I/O

Input (`MapRequest`):

```json
{"type": "object", "additionalProperties": false, "properties": {
  "focus_files": {"type": "array", "items": {"type": "string"}, "description": "Files the ticket or diff names"},
  "mentioned": {"type": "array", "items": {"type": "string"}, "description": "Identifiers from the ticket"},
  "token_budget": {"type": "integer", "minimum": 128, "maximum": 4096, "default": 1024}
}}
```

Output (`MapResponse`):

```json
{"type": "object", "required": ["backend", "commit", "token_budget", "estimated_tokens", "entries", "omitted"], "properties": {
  "backend": {"type": "string"},
  "commit": {"type": "string"},
  "token_budget": {"type": "integer"},
  "estimated_tokens": {"type": "integer"},
  "entries": {"type": "array", "items": {"type": "object", "required": ["path", "rank", "symbols"], "properties": {
    "path": {"type": "string"}, "rank": {"type": "number"},
    "symbols": {"type": "array", "items": {"type": "string"}}
  }}},
  "omitted": {"type": "integer"}
}}
```

Errors: `token_budget` out of range → JSON-RPC `-32602`; `MapError::Unavailable` (index missing or built at another commit than `HEAD`) → `isError: true`, `error: "environment_error"`.

## Never

`guard_map` never returns file contents, never reads outside the checkout, and never reveals `tests/journeys/**` or `tests/smoke/**` to a Coder: in a Coder sandbox the graph is built without the hidden tests dir (sandbox.md).
