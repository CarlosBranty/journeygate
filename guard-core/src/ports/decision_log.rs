//! `DecisionLog` (triage decisions + human labels) and `GateHistory` (finalize
//! results for the dashboard). Postgres in production; schema in
//! `docs/contracts/decision-log.md`.

use super::types::{
    CalibrationQuery, CalibrationRow, DecisionId, GateReport, GateResultId, HumanLabel,
    ReportOutcome, StoredGateResult, TriageDecision,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("store not configured: {0}")]
    Unconfigured(String),
    /// Connection refused, pool timeout, TLS failure.
    #[error("store unavailable: {0}")]
    Unavailable(String),
    /// Constraint violation, bad row, JSON (de)serialization failure.
    #[error("store rejected the operation: {0}")]
    Invalid(String),
}

#[async_trait::async_trait]
pub trait DecisionLog: Send + Sync {
    /// Appends one decision (never updates an earlier one).
    async fn record_decision(&self, decision: &TriageDecision) -> Result<DecisionId, StoreError>;

    /// Appends the human label observed for a ticket.
    async fn record_human_label(&self, label: &HumanLabel) -> Result<(), StoreError>;

    /// For each ticket, its latest decision next to its latest human label,
    /// newest decision first, filtered by `query`.
    async fn list_for_calibration(
        &self,
        query: &CalibrationQuery,
    ) -> Result<Vec<CalibrationRow>, StoreError>;
}

#[async_trait::async_trait]
pub trait GateHistory: Send + Sync {
    /// Stores one finalize result with what the reporting did. Idempotent on
    /// `(repo, run_id)`: storing the same run again replaces it and keeps the id.
    async fn record_gate_result(
        &self,
        report: &GateReport,
        outcome: &ReportOutcome,
    ) -> Result<GateResultId, StoreError>;

    /// The newest results first, at most `limit`.
    async fn recent_gate_results(&self, limit: u32) -> Result<Vec<StoredGateResult>, StoreError>;

    /// The newest result for a PR of a repo, if any.
    async fn latest_for_pr(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Option<StoredGateResult>, StoreError>;
}
