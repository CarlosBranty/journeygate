//! The port the tools call into. `guard-cli` backs it with its own command
//! internals (in-process or `current_exe()` subprocesses, never a user shell).
//!
//! The trait is the safety boundary of `guard mcp`: it only offers reads and
//! test runs. There is no method to edit or delete journeys or the catalog,
//! update snapshots, change `target.toml`/`thresholds.toml`, quarantine or skip
//! tests, or override the guard verdict, so no tool can do those things.

use crate::error::ToolError;
use crate::types::{
    AffectedOutput, AffectedRequest, FailureOutput, FailureRequest, ReproOutput, ReproRequest,
    TestOutput, TestRequest,
};
use async_trait::async_trait;
use guard_core::catalog::Catalog;

#[async_trait]
pub trait GuardActions: Send + Sync + 'static {
    /// Same selection as `guard affected --json` against `request.base`.
    async fn affected(&self, request: AffectedRequest) -> Result<AffectedOutput, ToolError>;

    /// Runs the selected tests headless. Exit `1` (tests failed) is an `Ok`
    /// result; configuration/environment failures are `Err`. The server cancels
    /// the returned future on timeout, so implementations must kill child
    /// processes on drop (`tokio::process::Command::kill_on_drop(true)`).
    async fn test(&self, request: TestRequest) -> Result<TestOutput, ToolError>;

    /// One failure from the last local run, or from the latest remote gate of
    /// `request.pr`. Unknown test id = [`ToolError::not_found`]; `pr` without
    /// `[server]` = [`ToolError::server_not_configured`].
    async fn failure(&self, request: FailureRequest) -> Result<FailureOutput, ToolError>;

    /// `guard repro <test-id> --pr <n>`. Same cancellation rule as [`Self::test`].
    async fn repro(&self, request: ReproRequest) -> Result<ReproOutput, ToolError>;

    /// The parsed `catalog/journeys.json`; the server filters and sorts it.
    async fn catalog(&self) -> Result<Catalog, ToolError>;
}
