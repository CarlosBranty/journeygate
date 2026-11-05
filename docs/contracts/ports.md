# Contract: ports and crate ownership (Phases 2-4)

Index of the shared interfaces. The traits and types live in `guard-core/src/ports/` and are the only thing crates share; adapters never depend on each other (except `guard-server` and `guard-cli`, which wire them).

## Crates

| Crate | Implements / provides | Depends on | Contract docs |
| --- | --- | --- | --- |
| `guard-core` | Ports, domain types, fakes (`features = ["fakes"]`), pure domain (`triage`, `gatekeeper`, `journey_changes`, `fingerprint`, `thresholds`, `baseline`) | — | this file, thresholds-toml.md, reporting.md |
| `guard-jev` | `DecisionEngine` (Jev) | core | jev-questions.md |
| `guard-impact` | `ImpactAnalyzer`, `summarize_diff`, `journey_change` | core | this file |
| `guard-tracker` | `IssueTracker` (Jira), `report::report_gate` + ADF builders | core | reporting.md |
| `guard-codehost` | `CodeHost` (GitHub, GitLab), `report::{render_pr_comment, gate_status}`; `PullRequestPublisher` (`publish::{github, gitlab}`, `publish::from_target_env`) | core | reporting.md, pr-publisher.md |
| `guard-store` | `DecisionLog` + `GateHistory` (Postgres), `EvidenceStore` (S3/MinIO); Phase 4 `TaskQueue`, `MemoryFactsWriter`/`MemoryFactsReader`, spend ingestion, Engram export | core | decision-log.md, finalize-phase2.md, memory.md |
| `guard-server` | `guard serve` (axum router + dashboard) | core + all adapters | server-api.md |
| `guard-mcp` | `guard mcp` (stdio MCP); Phase 4 `guard_history` (reader only), `guard_map` | core (+ `guard-map`) | mcp-tools.md, memory.md, guard-map.md |
| `guard-cli` | binary; Phase 2 `finalize`, `--affected` with impact, `repro`, `serve`, `mcp` subcommands; Phase 4 `agents` (integrator) | core + adapters + server + mcp | cli.md, finalize-phase2.md, agents-toml.md |
| `guard-sandbox` (Phase 4) | `SandboxProvider` (rootless Docker), QA/Coder harness | core | sandbox.md, qa-coder-protocol.md |
| `guard-runtimes` (Phase 4) | `AgentRuntime` adapters, `ModelGateway` (LiteLLM) | core | agent-task.md, tokens.md |
| `guard-agents` (Phase 4) | `agents.toml`, scheduler, `guard agents sync` renderer | core | agents-toml.md |
| `guard-map` (Phase 4) | `GraphSource` trait, personalized PageRank, `MapRequest`/`MapResponse` | none (backends adapt into it) | guard-map.md |

## Ports (`guard_core::ports`)

| Trait | Methods | Async | Error |
| --- | --- | --- | --- |
| `DecisionEngine` | `model`, `triage(&TriageRequest) -> TriageAnswers`, `impact(&DiffSummary, &[JourneyQuestion]) -> BTreeMap<id, f64>` | yes | `EngineError` |
| `IssueTracker` | `find_by_label`, `create_bug`, `comment_adf`, `attach_file`, `add_labels`, `suggest_labels`, `get_issue`, `parse_webhook` (sync) | yes | `TrackerError`, `TrackerWebhookError` |
| `CodeHost` | `provider`, `pr_context(PrRef, Option<&Regex>) -> PrContext`, `publish_status(&GateStatus)`, `comment_pr(PrRef, &str)` (upsert by marker), `verify_webhook` (sync), `parse_webhook` (sync) | yes | `CodeHostError`, `WebhookError` |
| `code_host::PullRequestPublisher` | `provider`, `push_branch(&BranchPush) -> PushedBranch` (force-push to `agent/<KEY>` only), `open_pull_request(&AgentPullRequest) -> OpenedPullRequest` (idempotent per `agent/<KEY>`, draft by default, label `agent`, never merges); see [`pr-publisher.md`](pr-publisher.md) | yes | `CodeHostError` |
| `DecisionLog` | `record_decision`, `record_human_label`, `list_for_calibration` | yes | `StoreError` |
| `GateHistory` | `record_gate_result` (idempotent on `(repo, run_id)`), `recent_gate_results`, `latest_for_pr` | yes | `StoreError` |
| `EvidenceStore` | `put_object`, `get_object`, `presign_get(key, Duration) -> String` (sync) | yes | `EvidenceError` |
| `ImpactAnalyzer` | `fan_out(&[String]) -> ImpactFacts`, `areas_for`, `resolve_mentions` | **no** (CPU over a local checkout; wrap in `spawn_blocking`) | `ImpactError` |
| `AgentRuntime` (Phase 4) | `name() -> &'static str`, `run(&Sandbox, &AgentTask) -> AgentOutcome` | yes | `RuntimeError` |
| `SandboxProvider` (Phase 4) | `create(&SandboxSpec) -> Sandbox`, `put_file`, `read_file`, `exec(&ExecRequest) -> ExecOutput`, `extract_diff(base) -> ExtractedDiff`, `destroy` (idempotent) | yes | `SandboxError` |
| `ModelGateway` (Phase 4) | `mint_key(&KeyRequest) -> VirtualKey`, `revoke(key_id)`, `spend_logs(&SpendQuery) -> SpendPage` | yes | `GatewayError` |
| `TaskQueue` (Phase 4) | `enqueue(&NewTask) -> TaskEnqueued`, `claim(worker, lease) -> Option<ClaimedTask>`, `conflicts(&[String])`, `heartbeat`, `complete`, `fail(.., retry_after)` | yes | `StoreError` |
| `MemoryFactsReader` (Phase 4) | `list_facts(&FactsQuery) -> FactsPage`, `get_fact`, `topic_history` | yes | `StoreError` |
| `MemoryFactsWriter: MemoryFactsReader` (Phase 4, orchestrator only) | `record_fact` (supersedes, idempotent), `mark_needs_review`, `confirm`, `invalidate`, `enqueue_candidate`, `list_candidates`, `approve_candidate`, `reject_candidate` | yes | `StoreError` |

All traits are `Send + Sync` and object-safe (`Arc<dyn Trait>`), async via `async-trait`. Domain types serialize with the serde attributes in `ports/types.rs`; those JSON shapes are what the HTTP API and the `jsonb` columns carry.

### Fakes

`guard_core::ports::fakes` (feature `fakes`): `FakeDecisionEngine`, `InMemoryTracker`, `FakeCodeHost` (webhook token header `x-guard-fake-token`, bodies are `CodeHostEvent` JSON), `InMemoryStore` (`DecisionLog` + `GateHistory`), `InMemoryEvidenceStore` (`memory://evidence/<key>?expires=<s>` URLs), `FakeImpactAnalyzer`. Phase 4: `FakeAgentRuntime` (scripted outcomes, validates tasks, `completed(summary, cost)`), `FakeSandboxProvider` (validates specs; `with_exec`, `with_diff`, `exec_log`, `is_destroyed`), `FakeModelGateway` (`sk-fake-<n>` tokens, `push_spend`, `is_active`, `minted`), `InMemoryTaskQueue` (shared queue semantics, `expire_leases`), `InMemoryMemoryFacts` (writer + `reader()` read-only view). Use them for use-case and server tests; adapters test their HTTP behavior against wiremock.

## Domain stubs to fill (core-domain builder)

Signatures, doc comments and contracts are final; bodies are `todo!()`:

| Module | Functions |
| --- | --- |
| `thresholds` | `parse_thresholds`, `validate` |
| `gatekeeper` | `GateRules::from_config`, `decide`, `decide_without_engine` |
| `triage` | `strip_markup`, `extract_acceptance_criteria`, `mentioned_modules`, `build_state`, `effective_lane`, `jira_labels`, `human_label_from_labels` |
| `journey_changes` | `review` |
| `fingerprint` | `normalize_error`, `fingerprint`, `from_label` |

Already implemented with tests: `baseline::{classify, is_blocking}`, `ImpactFacts::blast_radius`, `Kind` names, `Thresholds::default`, `Fingerprint::label`, `fingerprint::test_id`, `evidence_store::{validate_key, validate_expiry}`, all fakes. Until the stubs are filled, any test that reaches them panics with `not yet implemented`; builders of other crates test against fakes and their own code, not through those functions.

## Impact analysis semantics (`guard-impact`)

- **Import graph**: for every tracked file of a language with a plugin (TypeScript, JavaScript, Python, Go, Rust, C#, PHP; tree-sitter queries in `languages/<lang>/imports.scm`), the imports it makes, resolved to repository files. Imports that resolve to no repository file (packages, the standard library) are dropped.
  - **TypeScript / JavaScript**: relative imports (`import ... from './x'`, `export ... from`, `require('./x')`, dynamic `import('./x')`, `declare module './x'` augmentations) resolved like Node/TS (`.ts`, `.tsx`, `.js`, `.mjs`, `.cjs`, `/index.*`); the `?` / `#` suffix is stripped from relative specifiers only. Non-relative specifiers go through the nearest `tsconfig.json` / `jsconfig.json` (a nearer config without `baseUrl` / `paths` still governs its files: a parent config's options never apply to them) (JSONC, `extends` as a string or an array, followed inside the repo): `compilerOptions.paths` (an exact pattern first, else the longest wildcard prefix), then `baseUrl`, relative to the directory TypeScript uses. Other bare specifiers (packages, `declare module 'name'`) are ignored.
  - **Python**: relative imports from the file's package; absolute imports searched in the roots importlib would see: the file's basedir (first ancestor directory without `__init__.py`), the repo root, `src/`, and the roots of every `pyproject.toml` (pytest `pythonpath`, setuptools `package-dir` and `packages.find.where`, poetry `packages[].from`, and the directory of a nested `pyproject.toml`). The packages around the importing file are not roots, so `from abc import ...` stays the standard library.
  - **Go**: import paths under a `module` of a repository `go.mod` (longest module first) resolve to that package directory's `.go` files (not `_test.go`).
  - **Rust**: every `Cargo.toml` with `[package]` gives its targets' root files (`[lib]` path and name, `[[bin]]`, `[[test]]`, `[[example]]`, `[[bench]]`, Cargo's `src/` auto-discovery and the `auto*` switches, `build.rs` / `package.build`) and its path dependencies (renames, `workspace = true`). Each target's module tree is built from `mod` declarations (`#[path = ...]` per the Rust Reference; a `#[path]` file owns its directory), then `use` / `crate::` / `super::` / `self::` paths, path-dependency crate names and `extern crate x (as y);` (an edge to `x`'s crate root; `y::...` paths anywhere in the target then resolve into `x`) resolve through it. Files no tree reaches fall back to their location.
  - **C#** (`.cs`): no file imports, so an edge means "references a type declared in that file". A symbol table maps every declared type (class, struct, interface, enum, record, delegate; nested types by their full dotted name) to the files declaring it, every part of a partial type included; a file of top-level statements declares the global `Program`. Each `.csproj` (with its `Directory.Build.props` / `.targets` chain) bounds the lookup: a reference sees the types of its project, of the projects it reaches through `ProjectReference` (transitively), of files linked with `<Compile Include>`, and of files under no `.csproj`; a name with no hit there is retried repository-wide. A type-name reference (base lists, fields, properties, parameters, return types, generic arguments, `new`, attributes with the `Attribute` suffix, `typeof` / `nameof`, static member access, casts, `is` / `as`, catch types) resolves in C#'s lookup order: nested types of the enclosing types, then each namespace from the innermost outwards, its own types before the using directives (plain, `static`, alias; a matching alias shadows the using namespaces of its level) declared in it, with `global using`s of the same project (the innermost directory holding a `.csproj`; files under no `.csproj` share one scope), the SDK's implicit usings (when `ImplicitUsings` is enabled) and `<Using>` items at the compilation-unit level. A using directive inside a namespace names its target relative to that namespace. The first level with a hit wins; an ambiguous name reaches every candidate. `using static T;` and `using X = T;` are themselves edges to `T`. Names that are not repository types (the BCL, packages) are dropped.
  - **PHP** (`.php`): an edge means "references a class, interface, trait or enum declared in that file, imports a function or constant declared there (`use function` / `use const`), or statically includes it". Class names in class positions (`new`, `extends`, `implements`, trait `use`, `::` calls, properties, constants and `::class`, `instanceof`, `catch`, parameter / return / property types, attributes) and class `use` imports (plain, aliased, grouped) are resolved by PHP's rules: `\A\B` is absolute, `namespace\A` is relative to the current namespace, otherwise the first segment goes through a `use` alias of the same namespace block, else the current namespace is prepended (no global fallback for classes). The fully qualified name maps to a file through the PSR-4 (longest prefix first, every directory of a prefix) then PSR-0 maps of `autoload` and `autoload-dev` in every `composer.json` (directories relative to each manifest), else to every file declaring it (classmap and non-PSR code; class names case-insensitive). `require` / `include` (and `_once`) with a string literal or `__DIR__` / `dirname(__FILE__)` / `dirname(__DIR__[, n])` concatenated with a literal resolve relative to the including file's directory. Composer vendor directories (`vendor/`, or `config.vendor-dir`, next to each `composer.json`) are third-party: their files are never targets and their manifests and declarations are ignored.
- **Fan-out of a file** = number of distinct repo files that import it directly. Files of languages without a plugin, unknown and deleted files have fan-out `0` (they still go through the path rules). `ImpactFacts.max_fan_out` = max over the analyzed files.
- **Hard rules**: a path matching `[hard_rules].human_only_paths` (globset, `literal_separator = true`) sets `human_only_path_hit` and is listed in `human_only_paths`.
- **Schema migration**: a path matching any of `**/migrations/**`, `**/migrate/**`, `**/alembic/versions/**`, `**/db/schema.rb`, `**/schema.prisma`, `**/*.sql`.
- **Areas**: `[areas.<key>].paths` globs; a file in two areas belongs to both (the doctor warns).
- `summarize_diff(unified_diff: &str) -> DiffSummary` (limits in jev-questions.md) and `journey_change(path, base_src: Option<&str>, head_src: &str) -> Option<JourneyChange>` (counts in `journey_changes.rs` docs) are free functions of the crate.
- The analyzer is built once per process from the repo root and `TargetConfig` (`ImpactIndex::build(root, &config) -> Result<ImpactIndex, ImpactError>`), walking the tree with the `ignore` crate (`.gitignore` respected, hidden files skipped).

## Phase 2 changes to Phase 1 contracts

These amend `cli.md`; `guard-cli` and `guard-core::routing` implement them.

1. **`guard test --affected` / `guard affected` with impact.** After the path rule, the CLI asks for impact: `[server].url` set → `POST /api/impact`; else `TYPESAFE_API_KEY` set → `guard-jev` locally; else none. Diff for the summary = `git diff --no-renames --submodule=diff --ignore-submodules=none <merge-base>` (working tree included; submodule changes appear as prefixed `diff --git a/<sub>/...` entries; untracked files are not part of the summary). Journeys with `impact > thresholds.impact_threshold` are added with reason `{"kind": "impact", "probability": p}`. A configured engine that fails (any server or engine error, a missing `GUARD_SERVER_TOKEN`, an answer missing an id or carrying a non-finite number) → **every active journey** with reason `{"kind": "engine_unavailable"}` (fail closed, architecture section 3) and `impact: engine unavailable, running every journey` on stdout. No engine configured (no `[server]`, no `TYPESAFE_API_KEY`) keeps the Phase 1 path rules only (`status: "not_configured"`, no `threshold`). `guard affected --json` gains `"impact": {"source": "server" | "local" | "none", "status": "ok" | "engine_unavailable" | "not_configured", "threshold": 0.25}`. `routing::Reason` gains `Impact { probability }` and `EngineUnavailable`; `routing::select_affected_with_impact(catalog, changed, impact: Option<&BTreeMap<String, f64>>, threshold, ticket)` implements it (`None` = engine failed; impact reasons follow path reasons and precede `smoke`; kinds serialize `snake_case`). The server's answer carries the threshold the client compares against. `guard test` and `guard affected` take `--thresholds <path>`.
2. **`guard gate finalize`**: finalize-phase2.md.
3. **New subcommands**: `guard serve` (server-api.md), `guard mcp` (mcp-tools.md), `guard repro` (mcp-tools.md), and `guard agents` (placeholder: prints that Phase 4 adds it, exit `0`).

## Phase 4 shared types (`guard_core::ports`)

Implemented and tested in `guard-core` (`ports::fakes::phase4_tests`), so every builder relies on the same invariants:

| Module | Types / pure logic |
| --- | --- |
| `agent` | `Role {Qa, Coder}`, `AgentTask` (+ `validate`), `TaskResult`, `TaskStatus`, `Usage` (+ `sum`), `RuntimeEvent`/`RuntimeEventKind`, `AgentOutcome`, `MemoryProposal`, `GatewayBinding`, `SealedSpec {path, sha256}` (+ `seal`, `verify`), `AttemptPolicy` (`MAX_ATTEMPTS = 3`, enforced on deserialize), `AllowedPaths` (+ `qa`, `coder`, `violations`), `CODER_PROTECTED_FILES`, `/task` path constants |
| `sandbox` | `SandboxSpec` (+ `for_role`, `validate`), `SandboxLayout`, `Mount`/`MountMode {ReadOnly, ReadWrite, Hidden}`, `EgressAllowlist`/`EgressRule`/`EgressPurpose`, `ResourceLimits`, `ExecRequest`, `ExecOutput`, `ExtractedDiff`, `Sandbox`, `SandboxId`, `REQUIRED_ENV` |
| `model_gateway` | `KeyRequest` (+ `validate`), `VirtualKey`, `SpendQuery` (+ `validate`), `SpendRecord`, `SpendPage` |
| `task_queue` | `NewTask`, `QueuedTask`, `TaskState`, `TaskId`, `TaskEnqueued`, `ClaimedTask` |
| `memory_facts` | `MemoryFact`, `NewFact` (+ `validate`), `FactScope`, `FactStatus`, `FactSource`, `FactCandidate`, `PendingCandidate`, `FactsQuery`, `FactsPage`, `FactsBrief` (+ `render`, `validate`; `FACTS_BRIEF_MAX_BYTES = 4096`), `read_only` |
| `secret` | `Secret` (no `Serialize`, `Debug` prints `***`) |

Phase 4 contract docs: agent-task.md, sandbox.md, qa-coder-protocol.md, agents-toml.md, memory.md, tokens.md, guard-map.md. Schemas: `schemas/{agent-task,agent-result,agent-event,agents,engram-export}.schema.json` (tests: `schemas/agent.test.mjs`). Migration: `guard-store/migrations/20261101000003_agent_lane.sql` (`memory_facts`, `memory_fact_candidates`, `task_queue`, `agent_runs`, `spend_records`; roles `guard_orchestrator` and `guard_memory_reader`).

`TaskQueue` is distinct from the generic `guard_store::JobQueue` (`jobs` table, Phase 2): it adds predicted-file conflict avoidance and (repo, ticket, role) dedupe; the scheduler uses `TaskQueue` only.

### Phase 4 ownership

| Builder | Owns | Must not edit |
| --- | --- | --- |
| sandbox + harness | `guard-sandbox/` | ports, contracts |
| runtimes + gateway | `guard-runtimes/` | ports, contracts |
| agents + scheduler | `guard-agents/` | ports, contracts |
| memory | `guard-store/` (TaskQueue + memory adapters, staleness, Engram export), `guard-mcp/` (`guard_history`) | the migration's table shapes (request a new migration) |
| map | `guard-map/`, `guard-mcp/` (`guard_map` tool only) | `GraphSource` signature |
| integrator | `guard-cli/` (`guard agents`, `guard memory export`, `agent_lane/`: `HostLayout`, `HostChecks`, `LocalBranchPublisher`, `QueueLane`), `guard_server::AgentLane`, `guard_agents::{lane, runs, scanner, ab}`, guard-mcp registration of `guard_history` / `guard_map`, root `Cargo.toml`, ports and contracts follow-ups | live E2E `guard-cli/tests/agent_lane_e2e.rs` |

Live suites for Phase 4: Postgres (`DATABASE_URL`), S3 (`GUARD_S3_TEST_ENDPOINT`), Docker (`GUARD_DOCKER_TESTS=1`). Agent runtimes are always tested with `FakeAgentRuntime` / recorded fixtures; no provider credential is read by any test.

## Testing policy (every crate)

- Strict TDD: failing test first.
- HTTP adapters are tested against `wiremock` (pinned `=0.6.5`), with the base URL injectable (`JEV_URL`, `JIRA_BASE_URL`, `GITHUB_API_URL`, `GITLAB_API_URL`, S3 endpoint from `GUARD_STORAGE_URL`). Response bodies are recorded fixtures under `<crate>/tests/fixtures/`.
- Live services only behind env vars that skip cleanly when unset: `DATABASE_URL` (Postgres), `GUARD_S3_TEST_ENDPOINT` (MinIO, an `s3+http://...` URL). A skipped test prints `skipped: <VAR> not set` and passes.
- Gates per crate: `cargo fmt --check`, `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo test -p <crate>`.

## Phase 4 integration seams

| Seam | Defined in | Implemented by |
| --- | --- | --- |
| `guard_server::AgentLane` (audited lane-A tickets, active mode only) | guard-server `triage.rs` | guard-cli `agent_lane::QueueLane` |
| `pipeline::seams::LayoutProvider` | guard-agents | guard-cli `agent_lane::host::HostLayout` |
| `pipeline::seams::TicketChecks` | guard-agents | guard-cli `agent_lane::checks::HostChecks` (guard-sandbox quality + report parser, `guard_agents::scanner`). Red-first and the gate run in a check sandbox (`guard_sandbox::check`, `DockerSandbox::create_check`: agent sandbox image, no mounts, internal network with no egress, tmpfs `/workspace`); `GUARD_UNSAFE_HOST_CHECKS=1` runs them on the host instead (development only, warned) |
| `pipeline::seams::PrPublisher` | guard-agents | guard-cli `agent_lane::host::CodeHostPublisher` (local branch, then `guard_codehost::publish::publish_agent_pr`) when `[code_host]` and its token resolve; else `LocalBranchPublisher` (`--fake`, no `[code_host]`, no token). Chosen by `agent_lane::host::pr_publisher` ([`pr-publisher.md`](pr-publisher.md), "Fake and wiring") |
| `runs::RunLog` (`agent_runs`) | guard-agents | `PgRunLog`, `MemoryRunLog` |
| `lane::Orchestrator` (runs, spend, candidates, orchestrator facts) | guard-agents | — |
| `TicketPipeline::with_secret_aliases` | guard-agents | `guard_runtimes::secret_env_aliases` |
| `guard_server::memory::MemoryWriters` (`AppState::with_memory`) | guard-server | `guard serve` with a Postgres store: `PgMemoryFacts`, `PgStore` as `FlakyHistory`, `staleness::GitRepo` on the default branch (the default base (cli.md "Base resolution" steps 4-6), else the current branch, else HEAD) as `BranchHead`, evidence base `s3://<bucket>` |
| `guard_server::jobs::PeriodicJob` (`AppState::with_job`) | guard-server | `guard serve`: `memory staleness` (Postgres store; `GUARD_STALENESS_INTERVAL`, default 15 min), and with the agent lane `sandbox reaper`, `key reaper` (`guard_agents::reaper::KeyReaper`, `PgRunActivity`, `min_age` = longest agent `timeout_secs` + `KEY_GRACE_SECS`, at least 15 min) and `network reaper` (`NetworkReaper`, `CliDockerNetworks`), every 5 min. Listed on the `serve: jobs:` startup line; failures and changed summaries go to stderr as `jobs: <name>: ...` |
