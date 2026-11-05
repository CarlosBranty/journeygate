//! Request and output types of the five tools (`docs/contracts/mcp-tools.md`).
//!
//! Requests are what the server hands to [`crate::GuardActions`] after it has
//! validated the tool arguments. Outputs serialize exactly to the declared
//! `outputSchema` of each tool: nullable fields are always present (as `null`),
//! optional non-nullable fields are omitted when absent.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- requests

/// `guard_affected` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffectedRequest {
    /// Base ref for the diff; `None` = detected (cli.md "Base resolution").
    pub base: Option<String>,
}

/// Which tests `guard_test` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestSelection {
    /// Journeys affected by the diff against `base` (`guard test --affected`).
    Affected,
    /// The whole suite.
    All,
    /// Exactly these journey ids (never empty).
    Journeys(Vec<String>),
}

/// `guard_test` arguments. There is deliberately no headed/ui/debug mode and no
/// snapshot update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestRequest {
    pub selection: TestSelection,
    /// Base for `Affected`; `None` = detected.
    pub base: Option<String>,
    pub docker: bool,
}

/// `guard_failure` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRequest {
    pub test_id: String,
    /// `Some` = read the latest remote gate of that PR; `None` = last local run.
    pub pr: Option<u64>,
}

/// `guard_repro` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReproRequest {
    pub test_id: String,
    pub pr: u64,
}

// ---------------------------------------------------------- guard_affected

/// Where the base of `guard affected` came from (cli.md "Base resolution").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseSource {
    Flag,
    Env,
    Ci,
    RemoteHead,
    RemoteBranch,
    LocalBranch,
}

/// The `guard affected --json` object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectedOutput {
    pub base: String,
    pub base_source: BaseSource,
    pub merge_base: String,
    pub branch: Option<String>,
    pub ticket: Option<String>,
    pub changed_files: Vec<String>,
    pub tags: Vec<String>,
    pub journeys: Vec<AffectedJourney>,
    pub impact: ImpactInfo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AffectedJourney {
    pub id: String,
    pub tag: String,
    pub reasons: Vec<SelectionReason>,
}

/// Why a journey was selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionReason {
    pub kind: ReasonKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probability: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonKind {
    Path,
    Smoke,
    Impact,
    All,
    Requested,
    EngineUnavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImpactInfo {
    pub source: ImpactSource,
    pub status: ImpactStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactSource {
    Server,
    Local,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactStatus {
    Ok,
    EngineUnavailable,
    NotConfigured,
}

// --------------------------------------------------------------- guard_test

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestOutput {
    /// Guard exit code: `0` passed, `1` tests failed (both normal results).
    pub exit_code: i32,
    pub counts: TestCounts,
    pub duration_ms: u64,
    pub tags: Vec<String>,
    pub failures: Vec<FailureSummary>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCounts {
    pub passed: u64,
    pub flaky: u64,
    pub failed: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureSummary {
    /// `<project>::<file>::<title>`.
    pub test_id: String,
    pub title: String,
    pub file: String,
    pub line: u64,
    pub journey: Option<String>,
    pub status: FailureStatus,
    pub error: String,
    /// `file:line` of the failing step.
    pub location: Option<String>,
    /// Repo-relative path under `e2e-playwright/test-results/`.
    pub screenshot: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStatus {
    Failed,
    Flaky,
}

// ------------------------------------------------------------ guard_failure

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureOutput {
    pub test_id: String,
    pub source: FailureSource,
    pub title: String,
    pub file: String,
    pub line: u64,
    pub journey: Option<String>,
    pub location: Option<String>,
    /// Full error message, ANSI stripped.
    pub error: String,
    /// Titles of the steps up to the failing one.
    pub steps: Vec<String>,
    pub attempts: Vec<Attempt>,
    pub baseline: Option<BaselineClass>,
    pub fingerprint: Option<String>,
    /// Local path, or presigned URL for remote.
    pub screenshot: Option<String>,
    /// Local `trace.zip` path, or the trace.playwright.dev viewer URL.
    pub trace_url: Option<String>,
    /// Bug key for remote failures.
    pub jira: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureSource {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub retry: u32,
    pub status: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineClass {
    Regression,
    PreExisting,
    Flaky,
    Unavailable,
}

// -------------------------------------------------------------- guard_repro

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReproOutput {
    /// The local run failed the same way (same fingerprint).
    pub reproduced: bool,
    /// `0` reproduced, `1` passed or failed differently (both normal results).
    pub exit_code: i32,
    /// `.guard/repro/<fingerprint>/` with the downloaded screenshot and trace.
    pub evidence_dir: String,
    pub remote: FailureRef,
    pub local: Option<FailureRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRef {
    pub status: RunStatus,
    pub fingerprint: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Passed,
    Flaky,
    Failed,
}

// ------------------------------------------------------------ guard_catalog

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogOutput {
    pub journeys: Vec<CatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub area: String,
    pub title: String,
    /// `@journey:<id>`, `@area:<area>`, plus `@smoke` when smoke.
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    pub smoke: bool,
    pub quarantined: bool,
}
