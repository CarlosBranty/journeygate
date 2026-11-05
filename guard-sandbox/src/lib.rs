//! Agent sandbox and the lane-A QA/Coder harness (architecture section 4).
//!
//! Contracts: `docs/contracts/sandbox.md` (mounts, egress, isolation flags)
//! and `docs/contracts/qa-coder-protocol.md` (red-first, sealing, diff
//! allowlist, finalize overlay, attempts, AST test-quality checks).
//!
//! - [`docker::DockerSandbox`]: `guard_core::ports::SandboxProvider` on the
//!   Docker CLI (`--read-only`, `--cap-drop=ALL`, `no-new-privileges`, an
//!   internal network whose only way out is one forwarder per egress rule,
//!   `runsc` when available). Live tests are `#[ignore]` and gated on
//!   `GUARD_DOCKER_TESTS=1`.
//! - [`harness::Harness`]: drives QA → diff check → quality → red-first →
//!   seal → Coder attempts → diff check → finalize, against the
//!   `AgentRuntime`, `SandboxProvider`, `ModelGateway`, `EvidenceStore` and
//!   `MemoryFactsReader` ports, so it is tested with the core fakes.
//!
//! - [`check`]: check sandboxes for the orchestrator's red-first and gate
//!   Playwright runs ([`check::run_playwright_check`] on
//!   [`check::CheckSandboxProvider`], implemented by `DockerSandbox`): no
//!   mounts, no egress, the candidate tree copied in.
//!
//! The shared types live in `guard_core::ports::{agent, sandbox}`.

pub mod check;
pub mod docker;
pub mod harness;

pub use docker::{DockerConfig, DockerSandbox, RuntimePreference};
