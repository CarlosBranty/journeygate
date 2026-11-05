//! In-memory fakes of every port, for adapter, server and use-case tests.
//!
//! Enabled with the `fakes` feature (`guard-core = { path = "../guard-core",
//! features = ["fakes"] }` in `[dev-dependencies]`). They hold state behind a
//! `std::sync::Mutex` (never held across an `.await`) and expose inspection
//! helpers (`created_bugs`, `statuses`, ...). They model the observable
//! contract of each port, not the HTTP details of any provider.

use super::code_host::{CodeHost, CodeHostError, WebhookError};
use super::decision_engine::{DecisionEngine, EngineError};
use super::decision_log::{DecisionLog, GateHistory, StoreError};
use super::evidence_store::{EvidenceError, EvidenceStore, validate_expiry, validate_key};
use super::impact_analyzer::{ImpactAnalyzer, ImpactError};
use super::issue_tracker::{IssueTracker, TrackerError, TrackerEvent, TrackerWebhookError};
use super::types::*;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(test)]
mod phase4_tests;
#[cfg(test)]
mod tests;

mod phase4;
pub use phase4::{
    FakeAgentRuntime, FakeModelGateway, FakeSandboxProvider, InMemoryMemoryFacts, InMemoryTaskQueue,
};

/// The pinned model id the fake engine reports.
pub const FAKE_MODEL: &str = "jev-1.13.0";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------- engine

/// Scripted `DecisionEngine`: triage answers are consumed in order; impact
/// returns the configured probability per journey id (default `0.0`).
#[derive(Default)]
pub struct FakeDecisionEngine {
    triage: Mutex<VecDeque<Result<TriageAnswers, EngineError>>>,
    triage_requests: Mutex<Vec<TriageRequest>>,
    impact: BTreeMap<String, f64>,
    impact_error: Option<EngineError>,
    impact_calls: Mutex<usize>,
}

impl FakeDecisionEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues the result of the next `triage` call.
    pub fn with_triage(self, result: Result<TriageAnswers, EngineError>) -> Self {
        lock(&self.triage).push_back(result);
        self
    }

    pub fn with_impact(mut self, journey: &str, probability: f64) -> Self {
        self.impact.insert(journey.to_string(), probability);
        self
    }

    /// Every non-empty `impact` call fails with `error`.
    pub fn failing_impact(mut self, error: EngineError) -> Self {
        self.impact_error = Some(error);
        self
    }

    pub fn triage_requests(&self) -> Vec<TriageRequest> {
        lock(&self.triage_requests).clone()
    }

    /// Number of `impact` calls that reached the engine (non-empty input).
    pub fn impact_calls(&self) -> usize {
        *lock(&self.impact_calls)
    }
}

#[async_trait::async_trait]
impl DecisionEngine for FakeDecisionEngine {
    fn model(&self) -> &str {
        FAKE_MODEL
    }

    async fn triage(&self, request: &TriageRequest) -> Result<TriageAnswers, EngineError> {
        lock(&self.triage_requests).push(request.clone());
        lock(&self.triage).pop_front().unwrap_or_else(|| {
            Err(EngineError::Unconfigured(
                "FakeDecisionEngine: no scripted triage answer".into(),
            ))
        })
    }

    async fn impact(
        &self,
        _diff: &DiffSummary,
        journeys: &[JourneyQuestion],
    ) -> Result<BTreeMap<String, f64>, EngineError> {
        if journeys.is_empty() {
            return Ok(BTreeMap::new());
        }
        *lock(&self.impact_calls) += 1;
        if let Some(err) = &self.impact_error {
            return Err(err.clone());
        }
        Ok(journeys
            .iter()
            .map(|j| (j.id.clone(), self.impact.get(&j.id).copied().unwrap_or(0.0)))
            .collect())
    }
}

// ---------------------------------------------------------------- tracker

#[derive(Default)]
struct TrackerState {
    issues: BTreeMap<String, Issue>,
    /// Creation order, for "newest first".
    order: Vec<String>,
    comments: BTreeMap<String, Vec<serde_json::Value>>,
    attachments: BTreeMap<String, Vec<Attachment>>,
    suggested: BTreeMap<String, Vec<String>>,
    created: Vec<NewBug>,
    next_number: u64,
}

/// In-memory Jira. Created Bugs get keys `<PROJECT>-1001`, `-1002`, ...
pub struct InMemoryTracker {
    project: String,
    state: Mutex<TrackerState>,
}

impl InMemoryTracker {
    pub fn new(project: &str) -> Self {
        Self {
            project: project.to_string(),
            state: Mutex::new(TrackerState {
                next_number: 1001,
                ..TrackerState::default()
            }),
        }
    }

    /// Seeds an existing issue (e.g. the change ticket).
    pub fn insert_issue(&self, issue: Issue) {
        let mut s = lock(&self.state);
        s.order.push(issue.key.clone());
        s.issues.insert(issue.key.clone(), issue);
    }

    pub fn created_bugs(&self) -> Vec<NewBug> {
        lock(&self.state).created.clone()
    }

    pub fn comments(&self, key: &str) -> Vec<serde_json::Value> {
        lock(&self.state)
            .comments
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn attachments(&self, key: &str) -> Vec<Attachment> {
        lock(&self.state)
            .attachments
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn suggested_labels(&self, key: &str) -> Vec<String> {
        lock(&self.state)
            .suggested
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    fn issue_ref(issue: &Issue) -> IssueRef {
        IssueRef {
            key: issue.key.clone(),
            url: issue.url.clone(),
            status_category: issue.status_category,
        }
    }

    fn require(s: &TrackerState, key: &str) -> Result<(), TrackerError> {
        if s.issues.contains_key(key) {
            Ok(())
        } else {
            Err(TrackerError::NotFound(key.to_string()))
        }
    }
}

#[async_trait::async_trait]
impl IssueTracker for InMemoryTracker {
    async fn find_by_label(&self, label: &str) -> Result<Vec<IssueRef>, TrackerError> {
        let s = lock(&self.state);
        Ok(s.order
            .iter()
            .rev()
            .filter_map(|k| s.issues.get(k))
            .filter(|i| i.labels.iter().any(|l| l == label))
            .take(10)
            .map(Self::issue_ref)
            .collect())
    }

    async fn create_bug(&self, bug: &NewBug) -> Result<IssueRef, TrackerError> {
        let mut s = lock(&self.state);
        let key = format!("{}-{}", self.project, s.next_number);
        s.next_number += 1;
        let issue = Issue {
            key: key.clone(),
            url: format!("memory://jira/browse/{key}"),
            summary: bug.summary.clone(),
            description: String::new(),
            issue_type: "Bug".into(),
            status: "To Do".into(),
            status_category: StatusCategory::ToDo,
            labels: bug.labels.clone(),
        };
        let out = Self::issue_ref(&issue);
        s.created.push(bug.clone());
        s.order.push(key.clone());
        s.issues.insert(key, issue);
        Ok(out)
    }

    async fn comment_adf(
        &self,
        key: &str,
        body: &serde_json::Value,
    ) -> Result<CommentRef, TrackerError> {
        let mut s = lock(&self.state);
        Self::require(&s, key)?;
        let list = s.comments.entry(key.to_string()).or_default();
        list.push(body.clone());
        Ok(CommentRef {
            id: list.len().to_string(),
            url: None,
        })
    }

    async fn attach_file(
        &self,
        key: &str,
        file: &Attachment,
    ) -> Result<AttachmentRef, TrackerError> {
        let mut s = lock(&self.state);
        Self::require(&s, key)?;
        let list = s.attachments.entry(key.to_string()).or_default();
        list.push(file.clone());
        Ok(AttachmentRef {
            id: list.len().to_string(),
            file_name: file.file_name.clone(),
        })
    }

    async fn add_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        let mut s = lock(&self.state);
        let issue = s
            .issues
            .get_mut(key)
            .ok_or_else(|| TrackerError::NotFound(key.to_string()))?;
        for label in labels {
            if !issue.labels.contains(label) {
                issue.labels.push(label.clone());
            }
        }
        Ok(())
    }

    async fn suggest_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        let mut s = lock(&self.state);
        Self::require(&s, key)?;
        s.suggested.insert(key.to_string(), labels.to_vec());
        Ok(())
    }

    async fn get_issue(&self, key: &str) -> Result<Issue, TrackerError> {
        lock(&self.state)
            .issues
            .get(key)
            .cloned()
            .ok_or_else(|| TrackerError::NotFound(key.to_string()))
    }

    /// No authentication; parses the Jira webhook JSON shape
    /// (`webhookEvent`, `issue.key`, `issue.fields.labels`, `changelog.items[].field`).
    fn parse_webhook(&self, request: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        let v: serde_json::Value = serde_json::from_slice(&request.body)
            .map_err(|e| TrackerWebhookError::Malformed(e.to_string()))?;
        let event = v["webhookEvent"].as_str().unwrap_or_default();
        let key = v["issue"]["key"].as_str().map(str::to_string);
        match (event, key) {
            ("jira:issue_created", Some(key)) => Ok(TrackerEvent::IssueCreated { key }),
            ("jira:issue_updated", Some(key)) => {
                let labels_changed = v["changelog"]["items"]
                    .as_array()
                    .is_some_and(|items| items.iter().any(|i| i["field"] == "labels"));
                if !labels_changed {
                    return Ok(TrackerEvent::Ignored {
                        reason: "no label change".into(),
                    });
                }
                let labels: BTreeSet<String> = v["issue"]["fields"]["labels"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| l.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(TrackerEvent::LabelsChanged {
                    key,
                    labels: labels.into_iter().collect(),
                })
            }
            _ => Ok(TrackerEvent::Ignored {
                reason: format!("event {event:?} not handled"),
            }),
        }
    }
}

// ---------------------------------------------------------------- code host

/// Header the fake code host reads its shared token from.
pub const FAKE_WEBHOOK_HEADER: &str = "x-guard-fake-token";

/// Marker of the guard summary comment (reporting.md).
pub const PR_COMMENT_MARKER: &str = "<!-- guard:gate-summary -->";

/// In-memory code host (reports itself as GitHub). Webhooks authenticate with
/// [`FAKE_WEBHOOK_HEADER`] equal to the secret; bodies are `CodeHostEvent` JSON.
pub struct FakeCodeHost {
    secret: String,
    prs: BTreeMap<u64, PrContext>,
    statuses: Mutex<Vec<GateStatus>>,
    comments: Mutex<BTreeMap<u64, Vec<String>>>,
}

impl FakeCodeHost {
    pub fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
            prs: BTreeMap::new(),
            statuses: Mutex::new(Vec::new()),
            comments: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn with_pr(mut self, pr: PrContext) -> Self {
        self.prs.insert(pr.number, pr);
        self
    }

    pub fn statuses(&self) -> Vec<GateStatus> {
        lock(&self.statuses).clone()
    }

    pub fn comments(&self, number: u64) -> Vec<String> {
        lock(&self.comments)
            .get(&number)
            .cloned()
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl CodeHost for FakeCodeHost {
    fn provider(&self) -> CodeHostProvider {
        CodeHostProvider::Github
    }

    async fn pr_context(
        &self,
        pr: PrRef,
        _ticket_pattern: Option<&Regex>,
    ) -> Result<PrContext, CodeHostError> {
        self.prs
            .get(&pr.number)
            .cloned()
            .ok_or(CodeHostError::NotFound(pr.number))
    }

    async fn publish_status(&self, status: &GateStatus) -> Result<(), CodeHostError> {
        lock(&self.statuses).push(status.clone());
        Ok(())
    }

    async fn comment_pr(
        &self,
        pr: PrRef,
        body_markdown: &str,
    ) -> Result<CommentRef, CodeHostError> {
        let mut all = lock(&self.comments);
        let list = all.entry(pr.number).or_default();
        let existing = body_markdown
            .contains(PR_COMMENT_MARKER)
            .then(|| list.iter().position(|c| c.contains(PR_COMMENT_MARKER)))
            .flatten();
        let index = match existing {
            Some(i) => {
                list[i] = body_markdown.to_string();
                i
            }
            None => {
                list.push(body_markdown.to_string());
                list.len() - 1
            }
        };
        Ok(CommentRef {
            id: (index + 1).to_string(),
            url: None,
        })
    }

    fn verify_webhook(&self, request: &WebhookRequest) -> Result<(), WebhookError> {
        match request.header(FAKE_WEBHOOK_HEADER) {
            None => Err(WebhookError::MissingSignature(FAKE_WEBHOOK_HEADER)),
            Some(token) if token == self.secret => Ok(()),
            Some(_) => Err(WebhookError::InvalidSignature),
        }
    }

    fn parse_webhook(&self, request: &WebhookRequest) -> Result<CodeHostEvent, WebhookError> {
        serde_json::from_slice(&request.body).map_err(|e| WebhookError::Malformed(e.to_string()))
    }
}

// ---------------------------------------------------------------- stores

#[derive(Default)]
struct StoreState {
    decisions: Vec<(DecisionId, TriageDecision)>,
    labels: Vec<HumanLabel>,
    gates: Vec<StoredGateResult>,
    next_gate: i64,
}

/// In-memory `DecisionLog` + `GateHistory`. `received_at` is a fixed
/// `1970-01-01T00:00:00Z`; ordering uses insertion order.
#[derive(Default)]
pub struct InMemoryStore {
    state: Mutex<StoreState>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl DecisionLog for InMemoryStore {
    async fn record_decision(&self, decision: &TriageDecision) -> Result<DecisionId, StoreError> {
        let mut s = lock(&self.state);
        let id = DecisionId(s.decisions.len() as i64 + 1);
        s.decisions.push((id, decision.clone()));
        Ok(id)
    }

    async fn record_human_label(&self, label: &HumanLabel) -> Result<(), StoreError> {
        lock(&self.state).labels.push(label.clone());
        Ok(())
    }

    async fn list_for_calibration(
        &self,
        query: &CalibrationQuery,
    ) -> Result<Vec<CalibrationRow>, StoreError> {
        let s = lock(&self.state);
        let limit = if query.limit == 0 {
            1000
        } else {
            query.limit as usize
        };
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for (id, d) in s.decisions.iter().rev() {
            if !seen.insert(d.ticket_key.clone()) {
                continue;
            }
            if query
                .since
                .as_ref()
                .is_some_and(|since| d.decided_at < *since)
            {
                continue;
            }
            if query.mode.is_some_and(|m| m != d.mode) {
                continue;
            }
            let human = s
                .labels
                .iter()
                .rev()
                .find(|l| l.ticket_key == d.ticket_key)
                .cloned();
            if query.labeled_only && human.is_none() {
                continue;
            }
            rows.push(CalibrationRow {
                decision_id: *id,
                decision: d.clone(),
                human,
            });
        }
        rows.sort_by(|a, b| b.decision.decided_at.cmp(&a.decision.decided_at));
        rows.truncate(limit);
        Ok(rows)
    }
}

#[async_trait::async_trait]
impl GateHistory for InMemoryStore {
    async fn record_gate_result(
        &self,
        report: &GateReport,
        outcome: &ReportOutcome,
    ) -> Result<GateResultId, StoreError> {
        let mut s = lock(&self.state);
        let same_run =
            |g: &StoredGateResult| g.report.repo == report.repo && g.report.run_id == report.run_id;
        if let Some(pos) = s.gates.iter().position(same_run) {
            let mut stored = s.gates.remove(pos);
            stored.report = report.clone();
            stored.outcome = outcome.clone();
            let id = stored.id;
            s.gates.push(stored);
            return Ok(id);
        }
        s.next_gate += 1;
        let id = GateResultId(s.next_gate);
        s.gates.push(StoredGateResult {
            id,
            received_at: "1970-01-01T00:00:00Z".into(),
            report: report.clone(),
            outcome: outcome.clone(),
        });
        Ok(id)
    }

    async fn recent_gate_results(&self, limit: u32) -> Result<Vec<StoredGateResult>, StoreError> {
        let s = lock(&self.state);
        Ok(s.gates.iter().rev().take(limit as usize).cloned().collect())
    }

    async fn latest_for_pr(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Option<StoredGateResult>, StoreError> {
        let s = lock(&self.state);
        Ok(s.gates
            .iter()
            .rev()
            .find(|g| {
                g.report.repo.as_deref() == Some(repo)
                    && g.report.pr.as_ref().is_some_and(|p| p.number == number)
            })
            .cloned())
    }
}

/// In-memory `EvidenceStore`. Presigned URLs look like
/// `memory://evidence/<key>?expires=<seconds>`.
#[derive(Default)]
pub struct InMemoryEvidenceStore {
    objects: Mutex<BTreeMap<String, (Vec<u8>, String)>>,
}

impl InMemoryEvidenceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes and content type of a stored object.
    pub fn object(&self, key: &str) -> Option<(Vec<u8>, String)> {
        lock(&self.objects).get(key).cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        lock(&self.objects).keys().cloned().collect()
    }
}

#[async_trait::async_trait]
impl EvidenceStore for InMemoryEvidenceStore {
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<ObjectRef, EvidenceError> {
        validate_key(key)?;
        let size = bytes.len() as u64;
        lock(&self.objects).insert(key.to_string(), (bytes, content_type.to_string()));
        Ok(ObjectRef {
            key: key.to_string(),
            size,
        })
    }

    async fn get_object(&self, key: &str) -> Result<Vec<u8>, EvidenceError> {
        validate_key(key)?;
        lock(&self.objects)
            .get(key)
            .map(|(bytes, _)| bytes.clone())
            .ok_or_else(|| EvidenceError::NotFound(key.to_string()))
    }

    fn presign_get(&self, key: &str, expires_in: Duration) -> Result<String, EvidenceError> {
        validate_key(key)?;
        validate_expiry(expires_in)?;
        Ok(format!(
            "memory://evidence/{key}?expires={}",
            expires_in.as_secs()
        ))
    }
}

// ---------------------------------------------------------------- impact

/// Scripted `ImpactAnalyzer`. Schema migrations are paths containing
/// `/migrations/` or starting with `migrations/`.
#[derive(Default)]
pub struct FakeImpactAnalyzer {
    fan_out: BTreeMap<String, u32>,
    human_only: BTreeSet<String>,
    areas: Vec<(String, Area)>,
    mentions: BTreeMap<String, String>,
}

impl FakeImpactAnalyzer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_fan_out(mut self, path: &str, fan_out: u32) -> Self {
        self.fan_out.insert(path.to_string(), fan_out);
        self
    }

    /// `path` counts as matching `human_only_paths`.
    pub fn with_human_only(mut self, path: &str) -> Self {
        self.human_only.insert(path.to_string());
        self
    }

    /// Paths starting with `prefix` belong to `area`.
    pub fn with_area(mut self, prefix: &str, area: &str) -> Self {
        self.areas.push((prefix.to_string(), Area::new(area)));
        self
    }

    pub fn with_mention(mut self, mention: &str, path: &str) -> Self {
        self.mentions.insert(mention.to_string(), path.to_string());
        self
    }
}

impl ImpactAnalyzer for FakeImpactAnalyzer {
    fn fan_out(&self, changed_files: &[String]) -> Result<ImpactFacts, ImpactError> {
        let fan_out: BTreeMap<String, u32> = changed_files
            .iter()
            .map(|f| (f.clone(), self.fan_out.get(f).copied().unwrap_or(0)))
            .collect();
        let human_only_paths: Vec<String> = changed_files
            .iter()
            .filter(|f| self.human_only.contains(*f))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(ImpactFacts {
            max_fan_out: fan_out.values().copied().max().unwrap_or(0),
            fan_out,
            human_only_path_hit: !human_only_paths.is_empty(),
            human_only_paths,
            schema_migration: changed_files
                .iter()
                .any(|f| f.contains("/migrations/") || f.starts_with("migrations/")),
        })
    }

    fn areas_for(&self, changed_paths: &[String]) -> Vec<Area> {
        changed_paths
            .iter()
            .flat_map(|p| {
                self.areas
                    .iter()
                    .filter(move |(prefix, _)| p.starts_with(prefix.as_str()))
                    .map(|(_, a)| a.clone())
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn resolve_mentions(&self, mentions: &[String]) -> Vec<String> {
        mentions
            .iter()
            .filter_map(|m| self.mentions.get(m).cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
