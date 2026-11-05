//! The triage use case (flow in `guard_core::triage` module docs):
//!
//! ```text
//! get_issue -> resolve_mentions -> fan_out -> triage -> decide -> record_decision
//!           -> add_labels (active) / suggest_labels (shadow)
//! ```
//!
//! Fail closed everywhere: an impact-analysis failure or an engine failure
//! routes to the human lane (`EngineUnavailable`, plus any hard-rule reason
//! that still applies), and a decision that could not be recorded in the
//! decision log is never applied as the agent lane.
//!
//! Phase 4: an audited lane-A ticket in active mode is handed to the
//! [`AgentLane`] (the QA + Coder queue). Shadow Mode, the human lane and an
//! unrecorded decision never reach it. A failed hand-off is logged and leaves
//! the ticket to humans (fail closed); it does not fail the webhook.

use crate::AppState;
use guard_core::gatekeeper::{GateRules, decide, decide_without_engine};
use guard_core::ports::{
    AreaOption, DecisionId, ImpactFacts, Issue, LabelSource, Lane, StoreError, TrackerError,
    TriageAnswers, TriageDecision, TriageRequest,
};

/// Where audited lane-A tickets go in active mode (guard-cli wires the
/// `guard-agents` queue here). Enqueueing must be idempotent per ticket.
#[async_trait::async_trait]
pub trait AgentLane: Send + Sync {
    /// `labels` are the labels triage just wrote (`area:<key>`, `lane:agent`, ...).
    async fn enqueue(
        &self,
        issue: &Issue,
        decision: DecisionId,
        labels: &[String],
    ) -> Result<(), String>;
}
use guard_core::triage::{
    build_state, effective_lane, human_label_from_labels, jira_labels, mentioned_modules,
};

#[derive(Debug, Clone, PartialEq)]
pub struct TriageOutcome {
    /// `None` when the decision log refused the decision.
    pub decision_id: Option<DecisionId>,
    pub decided: Lane,
    /// The lane the labels reflect.
    pub effective: Lane,
    /// Labels written (`add_labels` in active mode, `suggest_labels` in shadow).
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TriageError {
    #[error("{0} is not configured")]
    NotConfigured(&'static str),
    #[error(transparent)]
    Tracker(#[from] TrackerError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The area options of the triage Choice, sorted by key (BTreeMap order).
fn area_options(state: &AppState) -> Vec<AreaOption> {
    state
        .config
        .areas
        .iter()
        .map(|(key, area)| AreaOption {
            key: guard_core::ports::Area::new(key.clone()),
            description: area.description.clone(),
        })
        .collect()
}

/// Impact facts for the ticket's mentioned modules, off the async runtime.
async fn facts_for(state: &AppState, modules: Vec<String>) -> Result<ImpactFacts, String> {
    let impact = state.impact.clone();
    tokio::task::spawn_blocking(move || {
        let paths = impact.resolve_mentions(&modules);
        impact.fan_out(&paths).map_err(|e| e.to_string())
    })
    .await
    .unwrap_or_else(|e| Err(format!("impact task failed: {e}")))
}

/// Triages one Jira issue end to end.
pub async fn triage_ticket(state: &AppState, key: &str) -> Result<TriageOutcome, TriageError> {
    let tracker = state
        .tracker
        .clone()
        .ok_or(TriageError::NotConfigured("issue tracker"))?;
    let log = state
        .decisions
        .clone()
        .ok_or(TriageError::NotConfigured("decision log"))?;
    let issue = tracker.get_issue(key).await?;
    let modules = mentioned_modules(&issue);
    let rules = GateRules::from_config(&state.config);
    let mode = state.thresholds.mode;

    let (facts, impact_error) = match facts_for(state, modules.clone()).await {
        Ok(facts) => (facts, None),
        Err(err) => (
            ImpactFacts::default(),
            Some(format!("impact analysis failed: {err}")),
        ),
    };
    let triage_state = build_state(&issue, modules, facts.blast_radius());

    let (answers, engine_error): (Option<TriageAnswers>, Option<String>) = match impact_error {
        // Without facts the hard rules cannot be checked: never ask the engine.
        Some(err) => (None, Some(err)),
        None => {
            let request = TriageRequest {
                ticket_key: key.to_string(),
                state: triage_state.clone(),
                areas: area_options(state),
                permutation_seed: None,
            };
            match state.engine.triage(&request).await {
                Ok(answers) => (Some(answers), None),
                Err(err) => (None, Some(err.to_string())),
            }
        }
    };
    let decided = match &answers {
        Some(answers) => decide(answers, &facts, &state.thresholds, &rules),
        None => decide_without_engine(&facts, &rules),
    };
    let effective = effective_lane(&decided, mode);

    let decision = TriageDecision {
        ticket_key: key.to_string(),
        decided_at: state.clock.now_rfc3339(),
        mode,
        state: triage_state,
        answers: answers.clone(),
        engine_error,
        facts,
        thresholds: state.thresholds.clone(),
        permutation_seed: None,
        decided: decided.clone(),
        effective: effective.clone(),
    };
    let decision_id = match log.record_decision(&decision).await {
        Ok(id) => Some(id),
        Err(err) => {
            tracing::error!(ticket = key, error = %err, "decision not recorded; applying the human lane");
            None
        }
    };

    // An unaudited decision is never applied as the agent lane.
    let unaudited_agent = decision_id.is_none() && decided.is_agent();
    let label_lane = if unaudited_agent {
        Lane::Human(Vec::new())
    } else {
        decided.clone()
    };
    let effective = if unaudited_agent && !mode.is_shadow() {
        Lane::Human(Vec::new())
    } else {
        effective
    };
    let labels = jira_labels(answers.as_ref(), &label_lane);
    if mode.is_shadow() {
        tracker.suggest_labels(key, &labels).await?;
    } else {
        tracker.add_labels(key, &labels).await?;
    }
    tracing::info!(ticket = key, ?decided, ?effective, "ticket triaged");
    if let (Some(lane), Some(id), true, false) = (
        &state.agent_lane,
        decision_id,
        effective.is_agent(),
        mode.is_shadow(),
    ) {
        match lane.enqueue(&issue, id, &labels).await {
            Ok(()) => tracing::info!(ticket = key, "lane-A ticket queued for QA + Coder"),
            Err(err) => tracing::error!(
                ticket = key,
                error = %err,
                "agent lane enqueue failed; the ticket stays with humans"
            ),
        }
    }
    Ok(TriageOutcome {
        decision_id,
        decided,
        effective,
        labels,
    })
}

/// `labels_changed`: records the human label (the calibration ground truth).
pub async fn record_labels(
    state: &AppState,
    key: &str,
    labels: &[String],
) -> Result<(), TriageError> {
    let log = state
        .decisions
        .clone()
        .ok_or(TriageError::NotConfigured("decision log"))?;
    let label = human_label_from_labels(
        key,
        labels,
        &state.clock.now_rfc3339(),
        LabelSource::JiraWebhook,
    );
    log.record_human_label(&label).await?;
    crate::memory::after_labels(state, key).await;
    Ok(())
}
