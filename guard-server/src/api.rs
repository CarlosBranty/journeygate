//! `/api/*` (bearer `GUARD_SERVER_TOKEN`) and `/healthz`.

use crate::AppState;
use crate::auth::{bearer, secret_eq};
use crate::body::{GATE_RESULT_LIMIT, SMALL_LIMIT, decode_json, read_limited};
use crate::error::ApiError;
use crate::report::{GateReporter, PortGateReporter, ReporterOptions};
use axum::Json;
use axum::extract::{Query, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use guard_core::ports::{
    CodeHostError, DiffSummary, GateHistory, GateReport, JourneyQuestion, PrContext, PrRef,
    ReportOutcome, StoredGateResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Most journeys one `/api/impact` request may ask about.
pub const MAX_IMPACT_JOURNEYS: usize = 1000;
/// `GateReport.version` this server accepts.
pub const GATE_REPORT_VERSION: u32 = 2;

pub async fn require_api_token(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    match bearer(request.headers()) {
        Some(token) if secret_eq(token, &state.api_token) => Ok(next.run(request).await),
        Some(_) => Err(ApiError::unauthorized("invalid bearer token")),
        None => Err(ApiError::unauthorized("missing bearer token")),
    }
}

// ------------------------------------------------------------------ impact

// Both directions: the server reads these and guard-cli sends/reads them.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImpactRequest {
    pub diff: DiffSummary,
    pub journeys: Vec<JourneyQuestion>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImpactResponse {
    pub model: String,
    pub threshold: f64,
    pub impact: BTreeMap<String, f64>,
}

pub async fn impact(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Result<Json<ImpactResponse>, ApiError> {
    let bytes = read_limited(request.into_body(), SMALL_LIMIT).await?;
    let body: ImpactRequest = decode_json(&bytes)?;
    if body.journeys.len() > MAX_IMPACT_JOURNEYS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_IMPACT_JOURNEYS} journeys per request, got {}",
            body.journeys.len()
        )));
    }
    let mut ids = BTreeSet::new();
    if let Some(dup) = body.journeys.iter().find(|j| !ids.insert(j.id.as_str())) {
        return Err(ApiError::bad_request(format!(
            "duplicate journey id {}",
            dup.id
        )));
    }
    let answers = state
        .engine
        .impact(&body.diff, &body.journeys)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "impact engine failed; client runs every journey");
            ApiError::engine_unavailable(e.to_string())
        })?;
    // Exactly one finite probability per requested id, or fail closed.
    let mut impact = BTreeMap::new();
    for journey in &body.journeys {
        match answers.get(&journey.id) {
            Some(p) if p.is_finite() => {
                impact.insert(journey.id.clone(), *p);
            }
            _ => {
                return Err(ApiError::engine_unavailable(format!(
                    "engine returned no usable answer for {}",
                    journey.id
                )));
            }
        }
    }
    Ok(Json(ImpactResponse {
        model: state.engine.model().to_string(),
        threshold: state.thresholds.impact_threshold,
        impact,
    }))
}

// ------------------------------------------------------------------ gate result

#[derive(Debug, Serialize, Deserialize)]
pub struct GateResultResponse {
    pub id: i64,
    pub outcome: ReportOutcome,
}

fn reporter(state: &AppState) -> Option<Arc<dyn GateReporter>> {
    if let Some(r) = &state.reporter {
        return Some(r.clone());
    }
    let tracker = state.tracker.clone()?;
    Some(Arc::new(PortGateReporter::new(
        tracker,
        state.evidence.clone(),
        ReporterOptions::from_config(&state.config),
    )))
}

pub async fn gate_result(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Result<Json<GateResultResponse>, ApiError> {
    let bytes = read_limited(request.into_body(), GATE_RESULT_LIMIT).await?;
    let report: GateReport = decode_json(&bytes)?;
    if report.version != GATE_REPORT_VERSION {
        return Err(ApiError::bad_request(format!(
            "unsupported gate report version {} (expected {GATE_REPORT_VERSION})",
            report.version
        )));
    }
    let gates = state
        .gates
        .clone()
        .ok_or_else(|| ApiError::not_configured("the gate history store"))?;
    let reporter = reporter(&state);
    let deadline = state.report_timeout;
    let memory_state = state.clone();
    let cell = state
        .gate_runs
        .cell(report.repo.as_deref().unwrap_or_default(), &report.run_id);
    // Spawned, so a client that disconnects (CLI timeout) never cancels the
    // reporting half-way or skips storing the result.
    let task = tokio::spawn(async move {
        let outcome = cell
            .get_or_init(|| async {
                if let Some(stored) = stored_outcome(gates.as_ref(), &report).await {
                    return stored;
                }
                let outcome = match reporter {
                    Some(r) => match tokio::time::timeout(deadline, r.report(&report)).await {
                        Ok(outcome) => outcome,
                        Err(_) => ReportOutcome {
                            warnings: vec![format!(
                                "Jira reporting timed out after {}s; the gate result is stored without it",
                                deadline.as_secs_f64()
                            )],
                            ..ReportOutcome::default()
                        },
                    },
                    None => ReportOutcome {
                        warnings: vec![
                            "issue tracker not configured: nothing reported to Jira".into(),
                        ],
                        ..ReportOutcome::default()
                    },
                };
                for warning in &outcome.warnings {
                    tracing::warn!(run_id = %report.run_id, warning, "gate reporting");
                }
                outcome
            })
            .await
            .clone();
        let id = gates.record_gate_result(&report, &outcome).await?;
        crate::memory::spawn_gate_result(&memory_state, report);
        Ok::<_, guard_core::ports::StoreError>(GateResultResponse { id: id.0, outcome })
    });
    let response = task
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|e| ApiError::from_store(&e))?;
    Ok(Json(response))
}

/// Most stored results searched for an earlier delivery of the same run.
const STORED_RUN_SCAN: u32 = 500;

/// The outcome already stored for `(repo, run_id)`, if this run was reported
/// before (another process, or before a restart).
async fn stored_outcome(gates: &dyn GateHistory, report: &GateReport) -> Option<ReportOutcome> {
    let recent = match gates.recent_gate_results(STORED_RUN_SCAN).await {
        Ok(recent) => recent,
        Err(e) => {
            tracing::warn!(error = %e, "gate history lookup failed; reporting the run");
            return None;
        }
    };
    recent
        .into_iter()
        .find(|r| r.report.run_id == report.run_id && r.report.repo == report.repo)
        .map(|r| r.outcome)
}

// ------------------------------------------------------------------ pr context

#[derive(Debug, Serialize, Deserialize)]
pub struct TicketInfo {
    pub key: String,
    pub url: String,
    pub flow_change: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PrContextResponse {
    pub pr: PrContext,
    pub ticket: Option<TicketInfo>,
    pub latest_gate: Option<StoredGateResult>,
}

pub async fn pr_context(
    State(state): State<Arc<AppState>>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<PrContextResponse>, ApiError> {
    let number: u64 = query
        .get("number")
        .ok_or_else(|| ApiError::bad_request("missing query parameter number"))?
        .parse()
        .map_err(|_| ApiError::bad_request("number must be a positive integer"))?;
    let host = state
        .code_host
        .clone()
        .ok_or_else(|| ApiError::not_configured("the code host"))?;
    // target.toml was validated at startup; an invalid pattern means no ticket.
    let pattern = state.config.ticket_regex().ok().flatten();
    let pr = host
        .pr_context(PrRef { number }, pattern.as_ref())
        .await
        .map_err(|e| match e {
            CodeHostError::NotFound(n) => {
                ApiError::not_found(format!("pull request {n} not found"))
            }
            other => {
                tracing::error!(error = %other, "code host pr_context failed");
                ApiError::internal()
            }
        })?;
    let ticket = match (&pr.ticket_key, &state.tracker) {
        (Some(key), Some(tracker)) => match tracker.get_issue(key).await {
            Ok(issue) => Some(TicketInfo {
                flow_change: issue.labels.iter().any(|l| l == "flow-change"),
                key: issue.key,
                url: issue.url,
            }),
            Err(e) => {
                tracing::warn!(ticket = key, error = %e, "ticket lookup failed");
                None
            }
        },
        _ => None,
    };
    let latest_gate = match &state.gates {
        Some(gates) => gates
            .latest_for_pr(&pr.repo, number)
            .await
            .map_err(|e| ApiError::from_store(&e))?,
        None => None,
    };
    Ok(Json(PrContextResponse {
        pr,
        ticket,
        latest_gate,
    }))
}

// ------------------------------------------------------------------ healthz

fn configured(on: bool) -> &'static str {
    if on { "configured" } else { "disabled" }
}

pub async fn healthz(State(state): State<Arc<AppState>>) -> Json<Value> {
    let database = if let Some(gates) = &state.gates {
        match gates.recent_gate_results(1).await {
            Ok(_) => "ok",
            Err(_) => "unavailable",
        }
    } else if let Some(log) = &state.decisions {
        let probe = guard_core::ports::CalibrationQuery {
            limit: 1,
            ..Default::default()
        };
        match log.list_for_calibration(&probe).await {
            Ok(_) => "ok",
            Err(_) => "unavailable",
        }
    } else {
        "disabled"
    };
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "checks": {
            "database": database,
            "engine": "configured",
            "tracker": configured(state.tracker.is_some()),
            "code_host": configured(state.code_host.is_some()),
            "evidence": configured(state.evidence.is_some()),
        }
    }))
}

pub async fn not_found() -> ApiError {
    ApiError::not_found("unknown route")
}
