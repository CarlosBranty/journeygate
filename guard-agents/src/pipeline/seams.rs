//! Orchestrator seams of the per-ticket pipeline that no shared port covers
//! yet. Each runs on the orchestrator side (host checkout, guard, code host),
//! never inside a sandbox:
//!
//! - [`LayoutProvider`]: prepares the host side of one sandbox (worktree on
//!   branch `agent/<KEY>`, `/task` dir, CodeGraph index, rendered skills, and
//!   for the Coder the host copy of the sealed spec).
//! - [`TicketChecks`]: deterministic test quality, red-first on a clean
//!   checkout of `base_commit`, the section-4 diff scanner, and the gate
//!   (apply the diff on a clean checkout, overlay the sealed spec, run the
//!   selection). The guard-sandbox harness is the intended implementation.
//! - [`PrPublisher`]: pushes `agent/<KEY>` and opens the PR. `CodeHost` has no
//!   push / create-PR method yet (reported to the integrator).

use crate::config::RuntimeKind;
use guard_core::ports::{ExtractedDiff, Role, SandboxLayout, SealedSpec};
use serde::{Deserialize, Serialize};

/// An infrastructure failure of a seam (retried by the worker).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SeamError(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

/// Everything the pipeline knows about one lane-A ticket. Serialized into
/// the queue payload, so a (ticket, role) task is self-contained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TicketInput {
    /// `owner/name`.
    pub repo: String,
    pub ticket: String,
    pub run_id: String,
    pub base_commit: String,
    pub summary: String,
    pub acceptance_criteria: Vec<String>,
    /// Globs the Coder may write (the ticket's `target.toml` areas).
    pub app_paths: Vec<String>,
    /// `[playwright].tests_dir`, usually `tests`.
    pub tests_dir: String,
    /// Tracker labels (best-of-N is opt-in by label).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Repo-relative files the impact analysis predicts (queue conflicts).
    #[serde(default)]
    pub predicted_files: Vec<String>,
    /// QA only: the preview environment Playwright MCP drives.
    #[serde(default)]
    pub preview: Option<Endpoint>,
    /// Coder only: the read-only package mirror.
    #[serde(default)]
    pub package_mirror: Option<Endpoint>,
    /// A/B trial (tokens.md "4-arm A/B protocol"); `None` outside experiments.
    #[serde(default)]
    pub experiment: Option<ExperimentArm>,
    /// `[areas]` keys of the ticket (facts brief scope, memory.md).
    #[serde(default)]
    pub areas: Vec<String>,
    /// Affected journey ids (facts brief scope, memory.md).
    #[serde(default)]
    pub journeys: Vec<String>,
    /// The queue claim this run belongs to (`task_queue.attempts`), set by
    /// the worker; not part of the payload. A claim after the first gets
    /// fresh key aliases, so a key left behind by a killed worker never
    /// blocks the retry.
    #[serde(skip)]
    pub claim: u32,
}

/// One (experiment, arm, task, repetition) of the token A/B.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentArm {
    /// `<experiment>:<arm>:<task>:<rep>`: the key alias of every attempt of
    /// the trial, so spend is attributable from `spend_records` alone.
    pub alias: String,
    /// `baseline`, `memory`, `map` or `both` (`agent_runs.arm`).
    pub arm: String,
    /// Facts brief and `guard_history` on.
    pub memory: bool,
    /// `guard_map` on.
    pub map: bool,
}

pub struct LayoutRequest<'a> {
    pub input: &'a TicketInput,
    pub role: Role,
    /// `[agents.<name>]`.
    pub agent: &'a str,
    pub runtime: RuntimeKind,
    pub attempt: u32,
    pub image: &'a str,
    /// Coder only: the verified sealed spec and its bytes, to materialize the
    /// host copy that is bind-mounted read-only.
    pub sealed: Option<(&'a SealedSpec, &'a [u8])>,
}

#[async_trait::async_trait]
pub trait LayoutProvider: Send + Sync {
    async fn layout(&self, request: &LayoutRequest<'_>) -> Result<SandboxLayout, SeamError>;

    /// Removes the host side of `layout` once its sandbox is destroyed (the
    /// per-attempt clone, task and skills dirs). Best effort.
    async fn release(&self, layout: &SandboxLayout) -> Result<(), SeamError>;
}

/// Ids of the failed deterministic checks (qa-coder-protocol.md table); empty = clean.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct QualityReport {
    pub failed: Vec<String>,
}

/// Result of running only the new spec on a clean checkout of `base_commit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum RedFirst {
    /// Failed with at least one assertion failure: the spec proves something new.
    Failed,
    /// Passed on the base commit: it proves nothing new.
    Passed,
    /// Did not fail on an assertion (syntax/compile error, app start timeout).
    Broken(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateResult {
    pub passed: bool,
    pub summary: String,
    /// Summary and trace excerpt of the failing tests only (never journey sources).
    pub failure_context: Option<String>,
}

#[async_trait::async_trait]
pub trait TicketChecks: Send + Sync {
    async fn test_quality(
        &self,
        input: &TicketInput,
        spec_path: &str,
        bytes: &[u8],
    ) -> Result<QualityReport, SeamError>;

    async fn red_first(
        &self,
        input: &TicketInput,
        spec_path: &str,
        bytes: &[u8],
    ) -> Result<RedFirst, SeamError>;

    /// Section-4 scanner over the Coder diff (test-environment detection,
    /// seed / `data-testid` in business logic, removed auth checks). Any
    /// finding sends the ticket to the human lane.
    async fn scan_diff(
        &self,
        input: &TicketInput,
        diff: &ExtractedDiff,
    ) -> Result<Vec<String>, SeamError>;

    /// Clean checkout of `base_commit` + the diff + the sealed spec overlaid.
    async fn gate(
        &self,
        input: &TicketInput,
        diff: &ExtractedDiff,
        sealed: &SealedSpec,
        sealed_bytes: &[u8],
    ) -> Result<GateResult, SeamError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct PrRequest {
    pub repo: String,
    /// The ticket key; `branch` is `agent/<ticket>`.
    pub ticket: String,
    /// `agent/<KEY>`.
    pub branch: String,
    pub base_commit: String,
    /// The Coder diff (applied on `base_commit`).
    pub diff: String,
    /// Overlaid at `sealed_spec.path` on the branch.
    pub sealed_spec: SealedSpec,
    pub sealed_bytes: Vec<u8>,
    pub title: String,
    /// The full PR body (used by publishers that render none of their own).
    pub body: String,
    /// The winning Coder's change summary (else the ticket summary).
    pub summary: String,
    /// Coder attempts spent on the ticket (every candidate).
    pub attempts: u32,
    /// Model spend of those Coder attempts in USD.
    pub cost_usd: f64,
    /// The winning Coder, e.g. `coder (claude-code, sonnet)`.
    pub coder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPr {
    pub url: String,
    pub number: Option<u64>,
}

#[async_trait::async_trait]
pub trait PrPublisher: Send + Sync {
    /// Pushes the branch as the orchestrator and opens the PR (human approval required).
    async fn publish(&self, request: &PrRequest) -> Result<PublishedPr, SeamError>;
}
