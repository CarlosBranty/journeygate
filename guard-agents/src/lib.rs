//! Agent configuration, queue, scheduling, the per-ticket pipeline and skill
//! rendering (architecture sections 4 and 16).
//!
//! - [`config`]: `agents.toml` parsing and validation
//!   (`docs/contracts/agents-toml.md`, `schemas/agents.schema.json`).
//! - [`pg_queue`]: the Postgres `TaskQueue` (`FOR UPDATE SKIP LOCKED`,
//!   (repo, ticket, role) dedupe, leases as visibility timeouts, retries) and
//!   the `spend_records` daily ledger.
//! - [`scheduler`]: the three slots of section 16 (host slots, provider
//!   slots, daily USD cap) and the queue [`scheduler::Worker`].
//! - [`pipeline`]: one ticket through QA → red-first → seal → Coder attempts
//!   → PR branch `agent/<KEY>`, only through ports (`AgentRuntime`,
//!   `SandboxProvider`, `ModelGateway`, `EvidenceStore`) and the orchestrator
//!   seams in [`pipeline::seams`]; optional best-of-N.
//! - [`reaper`]: revokes gateway keys of runs that are no longer active and
//!   removes sandbox networks no container uses.
//! - [`sync`]: `guard agents sync` — renders `agents/skills/*/SKILL.md` and
//!   `agents/AGENTS.md` once per runtime, plus `guard mcp` registrations.
//!
//! Memory: the pipeline holds only a `MemoryFactsReader`; agent memory
//! proposals are returned to the orchestrator, never written here.

pub mod ab;
pub mod config;
pub mod lane;
pub mod pg_queue;
pub mod pipeline;
pub mod reaper;
pub mod runs;
pub mod scanner;
pub mod scheduler;
pub mod sync;
pub mod worker;
