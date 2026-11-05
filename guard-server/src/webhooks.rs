//! `POST /webhooks/{jira,github,gitlab}`: verify, answer fast, process in
//! the background.

use crate::AppState;
use crate::body::{SMALL_LIMIT, read_limited, webhook_request};
use crate::deliveries::with_retries;
use crate::error::ApiError;
use crate::triage::{record_labels, triage_ticket};
use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use guard_core::ports::{
    CodeHostEvent, CodeHostProvider, TrackerEvent, TrackerWebhookError, WebhookError,
    WebhookRequest,
};
use serde_json::json;
use std::sync::Arc;

async fn raw(request: Request) -> Result<WebhookRequest, ApiError> {
    let (parts, body) = request.into_parts();
    let bytes = read_limited(body, SMALL_LIMIT).await?;
    Ok(webhook_request(&parts, bytes))
}

fn not_accepted(reason: String) -> Response {
    (
        StatusCode::OK,
        Json(json!({"accepted": false, "reason": reason})),
    )
        .into_response()
}

pub async fn jira(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Result<Response, ApiError> {
    let delivery = raw(request).await?;
    let tracker = state
        .tracker
        .clone()
        .ok_or_else(|| ApiError::not_configured("the issue tracker"))?;
    let event = tracker.parse_webhook(&delivery).map_err(|e| match e {
        TrackerWebhookError::Unauthorized => {
            ApiError::unauthorized("missing or invalid webhook secret")
        }
        TrackerWebhookError::Malformed(m) => {
            ApiError::bad_request(format!("malformed webhook: {m}"))
        }
    })?;
    let (event_name, key, dedupe_key, fingerprint) = match &event {
        TrackerEvent::IssueCreated { key } => (
            "issue_created",
            key.clone(),
            format!("issue_created:{key}"),
            String::new(),
        ),
        TrackerEvent::LabelsChanged { key, labels } => (
            "labels_changed",
            key.clone(),
            format!("labels_changed:{key}"),
            labels.join(" "),
        ),
        TrackerEvent::Ignored { reason } => return Ok(not_accepted(reason.clone())),
    };
    if state.decisions.is_none() {
        return Err(ApiError::not_configured("the decision log"));
    }
    if !state.deliveries.first_seen(&dedupe_key, &fingerprint) {
        return Ok(not_accepted(format!(
            "duplicate delivery of {event_name} for {key}"
        )));
    }
    let task_state = state.clone();
    state.tasks.spawn(async move {
        let policy = task_state.webhook_retry;
        let result = with_retries(policy, || async {
            match &event {
                TrackerEvent::IssueCreated { key } => {
                    triage_ticket(&task_state, key).await.map(|_| ())
                }
                TrackerEvent::LabelsChanged { key, labels } => {
                    record_labels(&task_state, key, labels).await
                }
                TrackerEvent::Ignored { .. } => Ok(()),
            }
        })
        .await;
        if let Err(err) = result {
            task_state.deliveries.forget(&dedupe_key);
            tracing::error!(error = %err, "jira webhook processing failed");
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"accepted": true, "event": event_name, "key": key})),
    )
        .into_response())
}

pub async fn github(state: State<Arc<AppState>>, request: Request) -> Result<Response, ApiError> {
    code_host(state, request, CodeHostProvider::Github).await
}

pub async fn gitlab(state: State<Arc<AppState>>, request: Request) -> Result<Response, ApiError> {
    code_host(state, request, CodeHostProvider::Gitlab).await
}

async fn code_host(
    State(state): State<Arc<AppState>>,
    request: Request,
    route: CodeHostProvider,
) -> Result<Response, ApiError> {
    let delivery = raw(request).await?;
    let host = state
        .code_host
        .clone()
        .filter(|h| h.provider() == route)
        .ok_or_else(|| ApiError::not_configured(&format!("a {route:?} code host")))?;
    host.verify_webhook(&delivery).map_err(|e| match e {
        WebhookError::MissingSignature(h) => ApiError::unauthorized(format!("missing {h} header")),
        WebhookError::InvalidSignature => ApiError::unauthorized("invalid webhook signature"),
        WebhookError::Malformed(m) => ApiError::bad_request(format!("malformed webhook: {m}")),
    })?;
    let event = host.parse_webhook(&delivery).map_err(|e| match e {
        WebhookError::Malformed(m) => ApiError::bad_request(format!("malformed webhook: {m}")),
        other => ApiError::bad_request(other.to_string()),
    })?;
    Ok(match event {
        CodeHostEvent::Ping => (
            StatusCode::OK,
            Json(json!({"accepted": true, "event": "ping"})),
        )
            .into_response(),
        CodeHostEvent::PullRequest {
            action,
            number,
            head_sha,
            ..
        } => {
            // Phase 2 only logs it: PRs reach the guard from CI (section 12).
            tracing::info!(?route, ?action, number, head_sha, "pull request event");
            (
                StatusCode::ACCEPTED,
                Json(json!({"accepted": true, "event": "pull_request", "number": number})),
            )
                .into_response()
        }
        CodeHostEvent::Ignored { reason } => not_accepted(reason),
    })
}
