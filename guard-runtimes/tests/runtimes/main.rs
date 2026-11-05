//! Adapter tests: golden invocations (exact argv, env and config files),
//! output parsing against fixtures, and the shared run loop against
//! `FakeSandboxProvider`. No runtime binary and no provider is ever called.
//!
//! Fixtures under `tests/fixtures/<runtime>/` are synthesized from each
//! runtime's documented output schema (sources cited in the adapter modules);
//! none was recorded from a live provider (no credentials on this machine).

mod common;

mod claude_code;
mod codex;
mod copilot;
mod deploy;
mod endpoint;
mod fake;
mod mini_swe_agent;
mod openhands;
mod registry;
mod run_loop;
