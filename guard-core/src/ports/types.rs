//! Domain types exchanged through the ports.
//!
//! These are plain data: every adapter and the server agree on them, and the
//! HTTP API (`docs/contracts/server-api.md`) and the decision log
//! (`docs/contracts/decision-log.md`) serialize them as JSON with the serde
//! attributes declared here. Renaming a field or a variant is a contract change.

use crate::fingerprint::Fingerprint;
use crate::thresholds::{Mode, Thresholds};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------- triage

/// A key of `[areas]` in `target.toml`. Areas are data, so the core stays target-agnostic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Area(pub String);

impl Area {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The kind of change a ticket asks for (triage Choice `kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Bugfix,
    MinorFeature,
    Refactor,
    Architecture,
}

impl Kind {
    /// Canonical option order of the `kind` Choice (before any permutation).
    pub const ALL: [Kind; 4] = [
        Kind::Bugfix,
        Kind::MinorFeature,
        Kind::Refactor,
        Kind::Architecture,
    ];

    /// The Choice option name, the serde name and the Jira label suffix (`type:<name>`).
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bugfix => "bugfix",
            Kind::MinorFeature => "minor_feature",
            Kind::Refactor => "refactor",
            Kind::Architecture => "architecture",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == name)
    }
}

/// A classification with the engine's confidence (Choice and Score only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Classified<T> {
    pub value: T,
    pub confidence: f64,
}

/// What Jev sees about the reach of a change. The real fan-out number stays in Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlastRadius {
    Isolated,
    Moderate,
    Wide,
}

/// The `state` object of the triage request. Serialized verbatim into the
/// Jev request body, so it carries no ticket key, no numbers Jev would have to
/// compare and no markup (stripped by `triage::build_state`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriageState {
    pub summary: String,
    pub description: String,
    pub acceptance_criteria: Vec<String>,
    pub mentioned_modules: Vec<String>,
    pub blast_radius: BlastRadius,
}

/// One option of the area Choice: an `[areas.<key>]` table of `target.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaOption {
    pub key: Area,
    /// `[areas.<key>].description`, used verbatim as the Choice criterion.
    pub description: String,
}

/// Input of `DecisionEngine::triage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriageRequest {
    /// For logs and errors only; never sent to the engine.
    pub ticket_key: String,
    pub state: TriageState,
    /// Area options in canonical order (sorted by key, as in `target.toml`).
    pub areas: Vec<AreaOption>,
    /// `Some(seed)` permutes the Choice option order (calibration, see
    /// `docs/contracts/jev-questions.md`); `None` keeps the canonical order.
    pub permutation_seed: Option<u64>,
}

/// The seven triage answers, already validated and converted to domain types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriageAnswers {
    /// The model version that actually answered (must equal the pinned one).
    pub model: String,
    pub area: Classified<Area>,
    pub kind: Classified<Kind>,
    /// Probability-weighted mean of 0-based levels: `0.0..=4.0` for 5 levels.
    pub risk: Classified<f64>,
    /// Noul probabilities (no confidence field exists for Noul).
    pub clear_e2e_criteria: f64,
    pub touches_money_or_security: f64,
    pub agent_suitable: f64,
    pub changes_existing_flow: f64,
    /// The raw `answers` object of the engine response, kept for calibration
    /// (Choice probabilities, Score legend). Opaque to the domain.
    pub raw: serde_json::Value,
}

/// Why a ticket goes to the human lane (section 2 of the architecture doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanReason {
    HardRulePath,
    SchemaMigration,
    FanOutTooWide,
    SensitiveArea,
    ArchitecturalKind,
    LowConfidence,
    RiskTooHigh,
    MoneyOrSecurity,
    NotAgentSuitable,
    UnclearCriteria,
    ChangesExistingFlow,
    EngineUnavailable,
    ShadowMode,
}

/// The Gatekeeper's routing decision.
///
/// JSON: `{"lane":"agent"}` or `{"lane":"human","reasons":["hard_rule_path"]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lane", content = "reasons", rename_all = "snake_case")]
pub enum Lane {
    Agent,
    Human(Vec<HumanReason>),
}

impl Lane {
    pub fn is_agent(&self) -> bool {
        matches!(self, Lane::Agent)
    }
}

// ---------------------------------------------------------------- impact

/// Facts computed in Rust (tree-sitter + `target.toml`). Jev never sees these numbers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImpactFacts {
    /// Largest fan-out over the analyzed files (0 when no file is known).
    pub max_fan_out: u32,
    /// Fan-out per analyzed file: number of distinct repository files that
    /// import it directly. Sorted by path.
    pub fan_out: BTreeMap<String, u32>,
    /// A path matched `[hard_rules].human_only_paths`.
    pub human_only_path_hit: bool,
    /// The paths that matched `human_only_paths`, sorted.
    pub human_only_paths: Vec<String>,
    /// A path is a schema migration (see `docs/contracts/ports.md`).
    pub schema_migration: bool,
}

impl ImpactFacts {
    /// `0..=3` isolated, `4..=15` moderate, `16..` wide.
    pub fn blast_radius(&self) -> BlastRadius {
        match self.max_fan_out {
            0..=3 => BlastRadius::Isolated,
            4..=15 => BlastRadius::Moderate,
            _ => BlastRadius::Wide,
        }
    }
}

/// Status of a file in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
}

/// One trimmed hunk: its `@@` header and at most the first lines of the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub path: String,
    pub header: String,
    pub lines: Vec<String>,
}

/// Deterministic summary of a diff: what the impact questions see instead of
/// the raw `git diff` (token limits, section 2 row 6). Built by
/// `guard_impact::summarize_diff`; size limits in `docs/contracts/jev-questions.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files: Vec<DiffFile>,
    /// Changed top-level symbols (functions, classes, exports), `path#symbol`.
    pub symbols: Vec<String>,
    pub hunks: Vec<DiffHunk>,
    /// `true` when anything was cut to fit the size limits.
    pub truncated: bool,
}

/// One impact Noul question: a catalog journey and its `impact_question`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyQuestion {
    pub id: String,
    pub impact_question: String,
}

// ---------------------------------------------------------------- code host

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeHostProvider {
    Github,
    Gitlab,
}

/// A pull request (GitHub) or merge request (GitLab) of the configured repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PrRef {
    /// PR number (GitHub) or MR IID (GitLab).
    pub number: u64,
}

/// Everything the gate and the server need to know about a PR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrContext {
    pub provider: CodeHostProvider,
    /// `owner/name` (GitHub) or the full project path `group/sub/name` (GitLab).
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    /// First match of `[tracker].ticket_pattern` in the branch name, else in the title.
    pub ticket_key: Option<String>,
    pub branch: String,
    pub base_branch: String,
    pub base_sha: String,
    pub head_sha: String,
    /// Repo-relative paths changed between `base_sha` and `head_sha`, sorted.
    pub changed_files: Vec<String>,
}

/// State of the required `guard` check (GitHub check run / GitLab commit status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pending,
    Running,
    Success,
    Failure,
    /// Infrastructure error: the verdict could not be computed.
    Error,
}

/// What `CodeHost::publish_status` publishes on a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateStatus {
    pub head_sha: String,
    /// Check run / commit status name. Always `guard` (reporting.md).
    pub name: String,
    pub state: CheckState,
    /// One line, at most 140 characters (GitLab `description` limit).
    pub title: String,
    /// Markdown body (GitHub check run `output.summary`; ignored by GitLab).
    pub summary: String,
    pub details_url: Option<String>,
}

/// A raw webhook delivery, as the server received it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebhookRequest {
    /// Header names lowercased; repeated headers joined with `, `.
    pub headers: BTreeMap<String, String>,
    /// Raw query string without `?`, if any.
    pub query: Option<String>,
    /// The exact raw body bytes (signatures are computed over these).
    pub body: Vec<u8>,
}

impl WebhookRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrAction {
    Opened,
    Synchronize,
    Reopened,
    Edited,
    Closed,
    Merged,
}

/// A webhook event the server acts on. Anything else parses to `Ignored`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CodeHostEvent {
    /// GitHub `ping` (sent when the webhook is created).
    Ping,
    PullRequest {
        action: PrAction,
        number: u64,
        title: String,
        branch: String,
        head_sha: String,
        base_sha: String,
    },
    Ignored {
        reason: String,
    },
}

// ---------------------------------------------------------------- tracker

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusCategory {
    ToDo,
    InProgress,
    Done,
}

/// A Jira issue as the domain needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub key: String,
    pub url: String,
    pub summary: String,
    /// The ADF description flattened to plain text (paragraphs separated by
    /// blank lines, list items prefixed with `- `). Markup stripping for the
    /// triage state happens later, in `triage::build_state`.
    pub description: String,
    pub issue_type: String,
    pub status: String,
    pub status_category: StatusCategory,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRef {
    pub key: String,
    pub url: String,
    pub status_category: StatusCategory,
}

/// A Bug to create (`POST /rest/api/3/issue`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewBug {
    pub project: String,
    pub summary: String,
    /// Atlassian Document Format (`{"type":"doc","version":1,"content":[...]}`).
    pub description_adf: serde_json::Value,
    pub labels: Vec<String>,
    /// Jira priority name (e.g. `Highest`); `None` = project default.
    pub priority: Option<String>,
    /// Change ticket to link with a `Relates` link, if any.
    pub linked_issue: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentRef {
    pub id: String,
    pub url: Option<String>,
}

/// A file to upload (Jira attachment or evidence object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub file_name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub id: String,
    pub file_name: String,
}

// ---------------------------------------------------------------- decision log

/// One triage decision, recorded always (also in Shadow Mode and when the
/// engine failed): it is the calibration dataset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriageDecision {
    pub ticket_key: String,
    /// RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
    pub decided_at: String,
    pub mode: Mode,
    pub state: TriageState,
    /// `None` when the engine failed (then `engine_error` is set).
    pub answers: Option<TriageAnswers>,
    pub engine_error: Option<String>,
    pub facts: ImpactFacts,
    pub thresholds: Thresholds,
    pub permutation_seed: Option<u64>,
    /// What the Gatekeeper decided.
    pub decided: Lane,
    /// What was applied (`Human([ShadowMode])` in Shadow Mode).
    pub effective: Lane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneKind {
    Agent,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    JiraWebhook,
    Manual,
}

/// The final label a human left in Jira: the ground truth for calibration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanLabel {
    pub ticket_key: String,
    /// RFC 3339 UTC.
    pub labeled_at: String,
    /// From the `area:<key>` label, if exactly one is present.
    pub area: Option<Area>,
    /// From the `type:<kind>` label, if exactly one is present.
    pub kind: Option<Kind>,
    /// From the `lane:agent` / `lane:human` label, if present.
    pub lane: Option<LaneKind>,
    /// `Some(true)` when the `flow-change` label is present.
    pub flow_change: Option<bool>,
    /// Every label on the issue at that moment, sorted.
    pub labels: Vec<String>,
    pub source: LabelSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CalibrationQuery {
    /// RFC 3339 lower bound on `decided_at` (inclusive).
    pub since: Option<String>,
    pub mode: Option<Mode>,
    /// Only decisions that have a human label.
    pub labeled_only: bool,
    /// Most rows to return, newest first. `0` means the store default (1000).
    pub limit: u32,
}

/// The latest decision of a ticket next to the latest human label of that ticket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationRow {
    pub decision_id: DecisionId,
    pub decision: TriageDecision,
    pub human: Option<HumanLabel>,
}

// ---------------------------------------------------------------- gate results

/// Outcome of re-running a test that failed 3/3 against the base commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaselineOutcome {
    /// Passes on the base commit (or does not exist there): this change broke it.
    Regression,
    /// Fails 3/3 on the base commit too: already broken on main.
    PreExisting,
    /// Passes on a retry on the base commit: unstable, not attributable.
    Flaky,
    /// The base run could not produce a result (infrastructure, cap reached).
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateVerdict {
    Green,
    Red,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestStatus {
    Passed,
    Flaky,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GateCounts {
    pub passed: usize,
    pub flaky: usize,
    pub failed: usize,
    pub skipped: usize,
    pub regressions: usize,
    pub pre_existing: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Screenshot,
    Trace,
    Video,
}

/// One evidence file. `key` is set once uploaded; `url` is a presigned GET URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceFile {
    pub kind: EvidenceKind,
    pub name: String,
    pub content_type: String,
    pub key: Option<String>,
    pub url: Option<String>,
    /// `https://trace.playwright.dev/?trace=<url-encoded url>` for traces.
    pub viewer_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptSummary {
    /// 0-based retry index.
    pub retry: u32,
    /// Playwright result status (`passed`, `failed`, `timedOut`, ...).
    pub status: String,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// Everything captured for one confirmed failure (section 11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBundle {
    pub test_id: String,
    pub fingerprint: Fingerprint,
    pub title: String,
    pub journey: Option<String>,
    /// Exact `file:line` of the failing step, from the JSON report's error location.
    pub location: Option<String>,
    /// First line of the error message (not normalized).
    pub error: String,
    pub step: Option<String>,
    /// The `seed` annotation of the test, if any.
    pub seed: Option<String>,
    pub commit: String,
    pub attempts: Vec<AttemptSummary>,
    pub baseline: Option<BaselineOutcome>,
    pub files: Vec<EvidenceFile>,
}

/// One test of a gate run, as reported to the server and the tracker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateTest {
    /// `<project>::<file>::<title>` (see `docs/contracts/reporting.md`).
    pub test_id: String,
    pub title: String,
    pub file: String,
    pub line: u32,
    pub project: String,
    pub tags: Vec<String>,
    pub journey: Option<String>,
    pub status: TestStatus,
    pub blocking: bool,
    pub attempts: usize,
    pub duration_ms: u64,
    pub error: Option<String>,
    /// Set only for tests that failed 3/3 and went through the baseline run.
    pub baseline: Option<BaselineOutcome>,
    pub fingerprint: Option<Fingerprint>,
    pub evidence: Option<EvidenceBundle>,
}

/// A modified journey spec, with the red flags of `journey_changes::review`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyChangeReport {
    pub journey: String,
    pub file: String,
    pub flags: Vec<String>,
}

/// The finalize result: `POST /api/gate-result` body and the `report` column
/// of `gate_results`. Version 2 of the verdict (Phase 1's `.guard/verdict.json`
/// is version 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    pub version: u32,
    pub run_id: String,
    /// RFC 3339 UTC.
    pub generated_at: String,
    pub provider: Option<CodeHostProvider>,
    pub repo: Option<String>,
    pub pr: Option<PrContext>,
    pub ticket_key: Option<String>,
    pub ticket_is_flow_change: bool,
    pub head_sha: String,
    pub base_sha: Option<String>,
    pub verdict: GateVerdict,
    pub counts: GateCounts,
    pub tests: Vec<GateTest>,
    pub journey_changes: Vec<JourneyChangeReport>,
    /// Flags of `journey_changes::review` (empty = clean).
    pub change_flags: Vec<String>,
    /// Presigned URL of the merged HTML report, if uploaded.
    pub report_url: Option<String>,
    /// CI pipeline/job URL (`GITHUB_SERVER_URL/.../actions/runs/<id>` or `CI_PIPELINE_URL`).
    pub ci_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BugAction {
    Created,
    Commented,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedBug {
    pub fingerprint: Fingerprint,
    pub key: String,
    pub url: String,
    pub action: BugAction,
}

/// What the reporting use case did (response of `POST /api/gate-result`,
/// and what `finalize` prints when it reports directly).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ReportOutcome {
    pub bugs: Vec<ReportedBug>,
    /// Key of the change ticket that received the gate summary comment.
    pub ticket_comment: Option<String>,
    pub pr_comment: bool,
    pub status_published: bool,
    /// Non-fatal reporting failures (reporting never changes the verdict).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GateResultId(pub i64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredGateResult {
    pub id: GateResultId,
    /// RFC 3339 UTC.
    pub received_at: String,
    pub report: GateReport,
    pub outcome: ReportOutcome,
}

// ---------------------------------------------------------------- evidence store

/// A stored object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectRef {
    pub key: String,
    pub size: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_names_round_trip() {
        for kind in Kind::ALL {
            assert_eq!(Kind::from_name(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(Kind::from_name("feature"), None);
    }

    #[test]
    fn blast_radius_buckets() {
        let facts = |n| ImpactFacts {
            max_fan_out: n,
            ..ImpactFacts::default()
        };
        assert_eq!(facts(0).blast_radius(), BlastRadius::Isolated);
        assert_eq!(facts(3).blast_radius(), BlastRadius::Isolated);
        assert_eq!(facts(4).blast_radius(), BlastRadius::Moderate);
        assert_eq!(facts(15).blast_radius(), BlastRadius::Moderate);
        assert_eq!(facts(16).blast_radius(), BlastRadius::Wide);
    }

    #[test]
    fn lane_json_shape() {
        assert_eq!(
            serde_json::to_value(Lane::Agent).unwrap(),
            serde_json::json!({"lane": "agent"})
        );
        let human = Lane::Human(vec![HumanReason::HardRulePath, HumanReason::ShadowMode]);
        let json = serde_json::to_value(&human).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"lane": "human", "reasons": ["hard_rule_path", "shadow_mode"]})
        );
        assert_eq!(serde_json::from_value::<Lane>(json).unwrap(), human);
    }

    #[test]
    fn area_is_a_plain_string_in_json() {
        assert_eq!(
            serde_json::to_string(&Area::new("checkout")).unwrap(),
            "\"checkout\""
        );
    }

    #[test]
    fn webhook_header_lookup_is_case_insensitive() {
        let mut req = WebhookRequest::default();
        req.headers
            .insert("x-hub-signature-256".into(), "sha256=ab".into());
        assert_eq!(req.header("X-Hub-Signature-256"), Some("sha256=ab"));
        assert_eq!(req.header("x-gitlab-token"), None);
    }

    #[test]
    fn code_host_event_json_shape() {
        let json = serde_json::to_value(CodeHostEvent::Ping).unwrap();
        assert_eq!(json, serde_json::json!({"event": "ping"}));
    }
}
