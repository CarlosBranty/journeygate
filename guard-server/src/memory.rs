//! The orchestrator's fact writers in `guard serve` (memory.md, "Writers").
//!
//! [`MemoryWriters`] holds the only `MemoryFactsWriter` of the process and
//! runs `guard_store::memory_writers` where the observations happen:
//!
//! - `POST /api/gate-result`, after the result is stored: blocking failures
//!   (`failure/`), fingerprints seen in >= 2 runs (`fingerprint/`) and the
//!   flaky history of the repo (`flaky/`, `review_after = now + 14 d`);
//! - Jira `labels_changed`, after the human label is recorded: a label that
//!   overrides the ticket's triage decision (`decision/<ticket>`).
//!
//! Writes run in the background after the response (`AppState::tasks`), go
//! through `memory_writers::observe` (a replayed delivery confirms instead of
//! versioning) and never fail the request: errors are logged.

use crate::AppState;
use guard_core::ports::{
    CalibrationQuery, DecisionLog, GateHistory, GateReport, MemoryFactsWriter, StoreError,
};
use guard_store::memory_writers::{
    self, MIN_FINGERPRINT_RUNS, fact_from_label_override, facts_from_fingerprints,
    facts_from_flaky, facts_from_gate_result,
};
use guard_store::{DEFAULT_FLAKY_WINDOW, FlakyHistory, FlakyQuery};
use std::sync::Arc;

/// Flaky rate from which a test gets a `flaky/` fact (the 2 % quarantine
/// rule of architecture section 4).
pub const DEFAULT_MIN_FLAKY_RATE: f64 = 0.02;
/// Stored gate results scanned for recurring fingerprints.
pub const DEFAULT_FINGERPRINT_SCAN: u32 = 200;
/// Calibration rows scanned to find the decision of a relabelled ticket.
pub const LABEL_SCAN: u32 = 1000;

/// The default branch head: `commit_sha` of `flaky/` and `decision/` facts.
#[async_trait::async_trait]
pub trait BranchHead: Send + Sync {
    async fn head(&self) -> Result<String, String>;
}

/// What one writer pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryReport {
    pub recorded: usize,
    pub skipped: Vec<String>,
}

impl MemoryReport {
    fn add(&mut self, out: memory_writers::WriteOutcome) {
        self.recorded += out.recorded.len();
        self.skipped.extend(out.skipped);
    }
}

pub struct MemoryWriters {
    facts: Arc<dyn MemoryFactsWriter>,
    /// `owner/name` of `decision/` facts (gate facts use `GateReport.repo`).
    repo: String,
    flaky: Option<Arc<dyn FlakyHistory>>,
    head: Option<Arc<dyn BranchHead>>,
    evidence_base: Option<String>,
    min_flaky_rate: f64,
    flaky_window: u32,
    fingerprint_scan: u32,
}

impl MemoryWriters {
    pub fn new(facts: Arc<dyn MemoryFactsWriter>, repo: impl Into<String>) -> Self {
        Self {
            facts,
            repo: repo.into(),
            flaky: None,
            head: None,
            evidence_base: None,
            min_flaky_rate: DEFAULT_MIN_FLAKY_RATE,
            flaky_window: DEFAULT_FLAKY_WINDOW,
            fingerprint_scan: DEFAULT_FINGERPRINT_SCAN,
        }
    }

    /// Enables `flaky/` facts (`PgStore` / `MemoryStore`).
    pub fn with_flaky(mut self, flaky: Arc<dyn FlakyHistory>) -> Self {
        self.flaky = Some(flaky);
        self
    }

    /// Where the default branch head comes from. Without it, `flaky/` facts
    /// use the run's base commit and `decision/` facts are skipped.
    pub fn with_head(mut self, head: Arc<dyn BranchHead>) -> Self {
        self.head = Some(head);
        self
    }

    /// `<base>/<key>` becomes a failure fact's `evidence_uri` (`s3://<bucket>`).
    pub fn with_evidence_base(mut self, base: impl Into<String>) -> Self {
        self.evidence_base = Some(base.into());
        self
    }

    pub fn with_min_flaky_rate(mut self, rate: f64) -> Self {
        self.min_flaky_rate = rate;
        self
    }

    pub fn with_flaky_window(mut self, window: u32) -> Self {
        self.flaky_window = window;
        self
    }

    pub fn facts(&self) -> &Arc<dyn MemoryFactsWriter> {
        &self.facts
    }

    async fn head(&self) -> Option<String> {
        let head = self.head.as_ref()?;
        match head.head().await {
            Ok(sha) => Some(sha),
            Err(e) => {
                tracing::warn!(error = %e, "memory: default branch head unavailable");
                None
            }
        }
    }

    /// The writers of one stored gate result. `now` is RFC 3339.
    pub async fn after_gate_result(
        &self,
        gates: &dyn GateHistory,
        report: &GateReport,
        now: &str,
    ) -> Result<MemoryReport, StoreError> {
        let mut out = MemoryReport::default();
        out.add(
            facts_from_gate_result(self.facts.as_ref(), report, self.evidence_base.as_deref())
                .await?,
        );
        let Some(repo) = report.repo.as_deref().filter(|r| !r.is_empty()) else {
            return Ok(out);
        };
        let recent = gates.recent_gate_results(self.fingerprint_scan).await?;
        out.add(
            facts_from_fingerprints(self.facts.as_ref(), repo, &recent, MIN_FINGERPRINT_RUNS)
                .await?,
        );
        if let Some(flaky) = &self.flaky {
            let stats = flaky
                .flaky_stats(&FlakyQuery {
                    repo: Some(repo.to_string()),
                    window: self.flaky_window,
                })
                .await?;
            if !stats.is_empty() {
                let commit = match self.head().await {
                    Some(sha) => sha,
                    None => report
                        .base_sha
                        .clone()
                        .unwrap_or_else(|| report.head_sha.clone()),
                };
                out.add(
                    facts_from_flaky(
                        self.facts.as_ref(),
                        repo,
                        &commit,
                        &stats,
                        now,
                        self.min_flaky_rate,
                    )
                    .await?,
                );
            }
        }
        Ok(out)
    }

    /// The writer of a recorded human label of `ticket`.
    pub async fn after_label_change(
        &self,
        log: &dyn DecisionLog,
        ticket: &str,
    ) -> Result<MemoryReport, StoreError> {
        let mut out = MemoryReport::default();
        let rows = log
            .list_for_calibration(&CalibrationQuery {
                labeled_only: true,
                limit: LABEL_SCAN,
                ..CalibrationQuery::default()
            })
            .await?;
        let Some(row) = rows.iter().find(|r| r.decision.ticket_key == ticket) else {
            out.skipped.push(format!("{ticket}: no triage decision"));
            return Ok(out);
        };
        let Some(head) = self.head().await else {
            out.skipped
                .push(format!("{ticket}: default branch head unavailable"));
            return Ok(out);
        };
        if let Some(fact) = fact_from_label_override(row, &self.repo, &head) {
            match memory_writers::observe(self.facts.as_ref(), &fact).await {
                Ok(_) => out.recorded += 1,
                Err(StoreError::Invalid(why)) => {
                    out.skipped.push(format!("{}: {why}", fact.topic_key));
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }
}

fn log_outcome(what: &str, key: &str, result: Result<MemoryReport, StoreError>) {
    match result {
        Ok(r) => {
            if r.recorded > 0 || !r.skipped.is_empty() {
                tracing::info!(what, key, recorded = r.recorded, skipped = ?r.skipped, "memory facts");
            }
        }
        Err(e) => tracing::error!(what, key, error = %e, "memory facts not written"),
    }
}

/// Runs the gate-result writers in the background (no-op without writers).
pub(crate) fn spawn_gate_result(state: &AppState, report: GateReport) {
    let (Some(memory), Some(gates)) = (state.memory.clone(), state.gates.clone()) else {
        return;
    };
    let now = state.clock.now_rfc3339();
    state.tasks.spawn(async move {
        let result = memory
            .after_gate_result(gates.as_ref(), &report, &now)
            .await;
        log_outcome("gate result", &report.run_id, result);
    });
}

/// Runs the label-override writer (in the webhook's background task).
pub(crate) async fn after_labels(state: &AppState, ticket: &str) {
    let (Some(memory), Some(log)) = (state.memory.clone(), state.decisions.clone()) else {
        return;
    };
    let result = memory.after_label_change(log.as_ref(), ticket).await;
    log_outcome("label change", ticket, result);
}
