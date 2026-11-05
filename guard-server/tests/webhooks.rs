//! `/webhooks/*`: authentication, event mapping, 202-and-process-async triage.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use guard_core::ports::fakes::{
    FAKE_WEBHOOK_HEADER, FakeCodeHost, FakeDecisionEngine, FakeImpactAnalyzer,
};
use guard_core::ports::*;
use guard_core::thresholds::Mode;
use serde_json::json;
use std::sync::Arc;

fn created(key: &str) -> serde_json::Value {
    json!({"webhookEvent": "jira:issue_created", "issue": {"key": key}})
}

// ------------------------------------------------------------------ jira

#[tokio::test]
async fn jira_without_secret_is_401() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post("/webhooks/jira")
        .body(Body::from(created("SHOP-7").to_string()))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.json()["error"], "unauthorized");
}

#[tokio::test]
async fn jira_secret_in_query_is_accepted() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post(format!("/webhooks/jira?secret={JIRA_SECRET}"))
        .body(Body::from(
            json!({"webhookEvent": "comment_created"}).to_string(),
        ))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["accepted"], false);
    assert!(res.json()["reason"].is_string());
}

#[tokio::test]
async fn jira_malformed_body_is_400() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post("/webhooks/jira")
        .header("authorization", format!("Bearer {JIRA_SECRET}"))
        .body(Body::from("{not json"))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.json()["error"], "bad_request");
}

#[tokio::test]
async fn jira_without_tracker_is_501() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(
        guard_server::router(fakes.bare_state()),
        jira_hook(created("SHOP-7")),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(res.json()["error"], "not_configured");
}

#[tokio::test]
async fn jira_body_over_one_mebibyte_is_413() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let big = vec![b' '; 1024 * 1024 + 1];
    let request = Request::post("/webhooks/jira")
        .header("authorization", format!("Bearer {JIRA_SECRET}"))
        .body(Body::from(big))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(res.json()["error"], "payload_too_large");
}

#[tokio::test]
async fn issue_created_answers_202_and_triages_in_the_background_shadow_mode() {
    let fakes =
        Fakes::new(FakeDecisionEngine::new().with_triage(Ok(confident_answers("checkout"))));
    fakes
        .tracker
        .insert_issue(issue("SHOP-7", "Fix cart total", "Cart rounds wrong.", &[]));
    let state = fakes.state();
    let tasks = state.tasks.clone();

    let res = send(guard_server::router(state), jira_hook(created("SHOP-7"))).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(
        res.json(),
        json!({"accepted": true, "event": "issue_created", "key": "SHOP-7"})
    );

    tasks.wait_idle().await;
    let rows = fakes
        .store
        .list_for_calibration(&CalibrationQuery::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let decision = &rows[0].decision;
    assert_eq!(decision.mode, Mode::Shadow);
    assert_eq!(decision.decided, Lane::Agent);
    assert_eq!(
        decision.effective,
        Lane::Human(vec![HumanReason::ShadowMode])
    );
    assert_eq!(decision.decided_at, NOW);
    // Shadow Mode never touches the real labels.
    assert_eq!(
        fakes.tracker.suggested_labels("SHOP-7"),
        vec!["area:checkout", "lane:agent", "type:bugfix"]
    );
    let issue = fakes.tracker.get_issue("SHOP-7").await.unwrap();
    assert!(issue.labels.is_empty());
}

#[tokio::test]
async fn labels_changed_records_the_human_label() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    fakes.tracker.insert_issue(issue("SHOP-7", "x", "", &[]));
    let state = fakes.state();
    let tasks = state.tasks.clone();
    let body = json!({
        "webhookEvent": "jira:issue_updated",
        "issue": {"key": "SHOP-7", "fields": {"labels": ["lane:human", "area:billing", "type:refactor"]}},
        "changelog": {"items": [{"field": "labels"}]}
    });
    let res = send(guard_server::router(state.clone()), jira_hook(body)).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(
        res.json(),
        json!({"accepted": true, "event": "labels_changed", "key": "SHOP-7"})
    );
    tasks.wait_idle().await;

    // A decision is needed for the calibration view to show the label.
    let decisions = fakes.store.clone();
    guard_server::triage::triage_ticket(&state, "SHOP-7")
        .await
        .unwrap();
    let rows = decisions
        .list_for_calibration(&CalibrationQuery::default())
        .await
        .unwrap();
    let human = rows[0].human.as_ref().expect("human label recorded");
    assert_eq!(human.lane, Some(LaneKind::Human));
    assert_eq!(human.area, Some(Area::new("billing")));
    assert_eq!(human.kind, Some(Kind::Refactor));
    assert_eq!(human.source, LabelSource::JiraWebhook);
    assert_eq!(human.labeled_at, NOW);
}

#[tokio::test]
async fn issue_updated_without_label_change_is_ignored() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let body = json!({
        "webhookEvent": "jira:issue_updated",
        "issue": {"key": "SHOP-7", "fields": {"labels": []}},
        "changelog": {"items": [{"field": "summary"}]}
    });
    let res = send(guard_server::router(fakes.state()), jira_hook(body)).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["accepted"], false);
}

#[tokio::test]
async fn jira_actionable_event_without_decision_log_is_501() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut state = fakes.state();
    state.decisions = None;
    let res = send(guard_server::router(state), jira_hook(created("SHOP-7"))).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(res.json()["error"], "not_configured");
}

// ------------------------------------------------------------------ github

#[tokio::test]
async fn github_without_signature_is_401() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post("/webhooks/github")
        .header("x-github-event", "ping")
        .body(Body::from(json!({"event": "ping"}).to_string()))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.json()["error"], "unauthorized");
}

#[tokio::test]
async fn github_with_wrong_signature_is_401() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post("/webhooks/github")
        .header(FAKE_WEBHOOK_HEADER, "nope")
        .body(Body::from(json!({"event": "ping"}).to_string()))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn github_ping_is_200() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let req = code_host_hook(
        "/webhooks/github",
        ("x-github-event", "ping"),
        json!({"event": "ping"}),
    );
    let res = send(guard_server::router(fakes.state()), req).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json(), json!({"accepted": true, "event": "ping"}));
}

#[tokio::test]
async fn github_pull_request_is_202_with_number() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let body = json!({
        "event": "pull_request", "action": "opened", "number": 12, "title": "SHOP-1 x",
        "branch": "SHOP-1-x", "head_sha": "b", "base_sha": "a"
    });
    let req = code_host_hook("/webhooks/github", ("x-github-event", "pull_request"), body);
    let res = send(guard_server::router(fakes.state()), req).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(
        res.json(),
        json!({"accepted": true, "event": "pull_request", "number": 12})
    );
}

#[tokio::test]
async fn github_other_event_is_not_accepted() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let req = code_host_hook(
        "/webhooks/github",
        ("x-github-event", "push"),
        json!({"event": "ignored", "reason": "push not handled"}),
    );
    let res = send(guard_server::router(fakes.state()), req).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.json(),
        json!({"accepted": false, "reason": "push not handled"})
    );
}

#[tokio::test]
async fn github_malformed_payload_is_400() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post("/webhooks/github")
        .header(FAKE_WEBHOOK_HEADER, HOOK_SECRET)
        .body(Body::from("[1,2"))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn github_without_code_host_is_501() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let req = code_host_hook(
        "/webhooks/github",
        ("x-github-event", "ping"),
        json!({"event": "ping"}),
    );
    let res = send(guard_server::router(fakes.bare_state()), req).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(res.json()["error"], "not_configured");
}

#[tokio::test]
async fn github_route_with_a_gitlab_host_is_501() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut state = fakes.state();
    state.code_host = Some(Arc::new(GitlabHost(FakeCodeHost::new(HOOK_SECRET))));
    let req = code_host_hook(
        "/webhooks/github",
        ("x-github-event", "ping"),
        json!({"event": "ping"}),
    );
    let res = send(guard_server::router(state), req).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
}

// ------------------------------------------------------------------ gitlab

#[tokio::test]
async fn gitlab_merge_request_is_202() {
    let fakes = Fakes::with(
        FakeDecisionEngine::new(),
        FakeImpactAnalyzer::new(),
        FakeCodeHost::new(HOOK_SECRET),
    );
    let mut state = fakes.state();
    state.code_host = Some(Arc::new(GitlabHost(FakeCodeHost::new(HOOK_SECRET))));
    let body = json!({
        "event": "pull_request", "action": "merged", "number": 4, "title": "t",
        "branch": "b", "head_sha": "h", "base_sha": "s"
    });
    let req = code_host_hook(
        "/webhooks/gitlab",
        ("x-gitlab-event", "Merge Request Hook"),
        body,
    );
    let res = send(guard_server::router(state), req).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    assert_eq!(
        res.json(),
        json!({"accepted": true, "event": "pull_request", "number": 4})
    );
}

#[tokio::test]
async fn gitlab_with_wrong_token_is_401() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut state = fakes.state();
    state.code_host = Some(Arc::new(GitlabHost(FakeCodeHost::new(HOOK_SECRET))));
    let request = Request::post("/webhooks/gitlab")
        .header(FAKE_WEBHOOK_HEADER, "wrong")
        .body(Body::from(json!({"event": "ping"}).to_string()))
        .unwrap();
    let res = send(guard_server::router(state), request).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn gitlab_route_with_a_github_host_is_501() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let req = code_host_hook(
        "/webhooks/gitlab",
        ("x-gitlab-event", "Merge Request Hook"),
        json!({"event": "ping"}),
    );
    let res = send(guard_server::router(fakes.state()), req).await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn unknown_route_is_json_404() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(guard_server::router(fakes.state()), get("/webhooks/nope")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.json()["error"], "not_found");
}

// ------------------------------------------------------------------ redelivery

#[tokio::test]
async fn a_redelivered_issue_created_is_triaged_once() {
    let fakes =
        Fakes::new(FakeDecisionEngine::new().with_triage(Ok(confident_answers("checkout"))));
    fakes
        .tracker
        .insert_issue(issue("SHOP-7", "Fix cart", "x", &[]));
    let state = fakes.state();
    let tasks = state.tasks.clone();
    let router = guard_server::router(state);
    let first = send(router.clone(), jira_hook(created("SHOP-7"))).await;
    assert_eq!(first.status, StatusCode::ACCEPTED);
    tasks.wait_idle().await;
    let again = send(router, jira_hook(created("SHOP-7"))).await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.json()["accepted"], false);
    assert!(
        again.json()["reason"]
            .as_str()
            .unwrap()
            .contains("duplicate")
    );
    tasks.wait_idle().await;
    assert_eq!(fakes.engine.triage_requests().len(), 1);
    let rows = fakes
        .store
        .list_for_calibration(&CalibrationQuery::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
}

#[tokio::test]
async fn a_label_change_reverted_and_redone_is_recorded_each_time() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    fakes.tracker.insert_issue(issue("SHOP-7", "x", "", &[]));
    let state = fakes.state();
    let tasks = state.tasks.clone();
    let router = guard_server::router(state);
    let labels = |l: &[&str]| {
        json!({
            "webhookEvent": "jira:issue_updated",
            "issue": {"key": "SHOP-7", "fields": {"labels": l}},
            "changelog": {"items": [{"field": "labels"}]}
        })
    };
    for body in [
        labels(&["lane:human"]),
        labels(&["lane:agent"]),
        labels(&["lane:human"]),
    ] {
        let res = send(router.clone(), jira_hook(body)).await;
        assert_eq!(res.status, StatusCode::ACCEPTED, "{}", res.text());
        tasks.wait_idle().await;
    }
    // An exact consecutive redelivery is dropped.
    let res = send(router, jira_hook(labels(&["lane:human"]))).await;
    assert_eq!(res.json()["accepted"], false);
}

/// Fails `get_issue` with a transport error the first `n` times.
struct FlakyTracker {
    inner: SecretTracker,
    failures: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl IssueTracker for FlakyTracker {
    async fn find_by_label(&self, label: &str) -> Result<Vec<IssueRef>, TrackerError> {
        self.inner.find_by_label(label).await
    }
    async fn create_bug(&self, bug: &NewBug) -> Result<IssueRef, TrackerError> {
        self.inner.create_bug(bug).await
    }
    async fn comment_adf(
        &self,
        key: &str,
        body: &serde_json::Value,
    ) -> Result<CommentRef, TrackerError> {
        self.inner.comment_adf(key, body).await
    }
    async fn attach_file(
        &self,
        key: &str,
        file: &Attachment,
    ) -> Result<AttachmentRef, TrackerError> {
        self.inner.attach_file(key, file).await
    }
    async fn add_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        self.inner.add_labels(key, labels).await
    }
    async fn suggest_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        self.inner.suggest_labels(key, labels).await
    }
    async fn get_issue(&self, key: &str) -> Result<Issue, TrackerError> {
        let left = self.failures.load(std::sync::atomic::Ordering::SeqCst);
        if left > 0 {
            self.failures
                .store(left - 1, std::sync::atomic::Ordering::SeqCst);
            return Err(TrackerError::Transport("connection reset".into()));
        }
        self.inner.get_issue(key).await
    }
    fn parse_webhook(&self, request: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        self.inner.parse_webhook(request)
    }
}

#[tokio::test]
async fn a_transient_tracker_failure_is_retried_until_triage_completes() {
    let fakes =
        Fakes::new(FakeDecisionEngine::new().with_triage(Ok(confident_answers("checkout"))));
    fakes
        .tracker
        .insert_issue(issue("SHOP-7", "Fix cart", "x", &[]));
    let flaky = FlakyTracker {
        inner: SecretTracker {
            inner: fakes.tracker.clone(),
        },
        failures: std::sync::atomic::AtomicUsize::new(1),
    };
    let state = fakes
        .state()
        .with_tracker(Arc::new(flaky))
        .with_webhook_retry(3, std::time::Duration::from_millis(5));
    let tasks = state.tasks.clone();
    let res = send(guard_server::router(state), jira_hook(created("SHOP-7"))).await;
    assert_eq!(res.status, StatusCode::ACCEPTED);
    tasks.wait_idle().await;
    let rows = fakes
        .store
        .list_for_calibration(&CalibrationQuery::default())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "triage completed after the retry");
}
