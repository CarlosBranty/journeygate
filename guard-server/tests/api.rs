//! `/api/*` and `/healthz`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use guard_core::ports::fakes::{FakeCodeHost, FakeDecisionEngine, FakeImpactAnalyzer};
use guard_core::ports::*;
use serde_json::json;
use std::sync::Arc;

fn impact_body(ids: &[&str]) -> String {
    json!({
        "diff": {
            "files": [{"path": "web/src/checkout/cart.ts", "status": "modified", "additions": 4, "deletions": 1}],
            "symbols": ["web/src/checkout/cart.ts#addToCart"],
            "hunks": [],
            "truncated": false
        },
        "journeys": ids.iter().map(|id| json!({"id": id, "impact_question": "q"})).collect::<Vec<_>>()
    })
    .to_string()
}

// ------------------------------------------------------------------ auth

#[tokio::test]
async fn api_without_bearer_is_401() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    for uri in ["/api/impact", "/api/gate-result"] {
        let request = Request::post(uri).body(Body::from("{}")).unwrap();
        let res = send(guard_server::router(fakes.state()), request).await;
        assert_eq!(res.status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(res.json()["error"], "unauthorized");
    }
    let res = send(
        guard_server::router(fakes.state()),
        get("/api/pr-context?number=1"),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_with_wrong_bearer_is_401() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let request = Request::post("/api/impact")
        .header("authorization", "Bearer api-secreT")
        .body(Body::from(impact_body(&[])))
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    // Never echoes the expected token.
    assert!(!res.text().contains(API_TOKEN));
}

#[tokio::test]
async fn unknown_api_route_is_404_after_auth() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(guard_server::router(fakes.state()), api_get("/api/nope")).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.json()["error"], "not_found");
}

// ------------------------------------------------------------------ impact

#[tokio::test]
async fn impact_returns_one_probability_per_journey() {
    let engine = FakeDecisionEngine::new().with_impact("checkout.customer-card", 0.81);
    let fakes = Fakes::new(engine);
    let body = impact_body(&["checkout.customer-card", "billing.invoice-pdf"]);
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", body),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.json(),
        json!({
            "model": "jev-1.13.0",
            "threshold": 0.25,
            "impact": {"checkout.customer-card": 0.81, "billing.invoice-pdf": 0.0}
        })
    );
}

#[tokio::test]
async fn impact_engine_failure_is_503_engine_unavailable() {
    let engine = FakeDecisionEngine::new().failing_impact(EngineError::Timeout);
    let fakes = Fakes::new(engine);
    let body = impact_body(&["checkout.customer-card"]);
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", body),
    )
    .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.json()["error"], "engine_unavailable");
}

struct PartialEngine;

#[async_trait::async_trait]
impl DecisionEngine for PartialEngine {
    fn model(&self) -> &str {
        "jev-1.13.0"
    }
    async fn triage(&self, _: &TriageRequest) -> Result<TriageAnswers, EngineError> {
        Err(EngineError::Timeout)
    }
    async fn impact(
        &self,
        _: &DiffSummary,
        _: &[JourneyQuestion],
    ) -> Result<std::collections::BTreeMap<String, f64>, EngineError> {
        Ok([("other".to_string(), f64::NAN)].into_iter().collect())
    }
}

#[tokio::test]
async fn impact_with_missing_or_non_finite_answers_fails_closed() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut state = fakes.state();
    state.engine = Arc::new(PartialEngine);
    let body = impact_body(&["checkout.customer-card"]);
    let res = send(guard_server::router(state), api_post("/api/impact", body)).await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.json()["error"], "engine_unavailable");
}

#[tokio::test]
async fn impact_with_no_journeys_does_not_call_the_engine() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", impact_body(&[])),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["impact"], json!({}));
    assert_eq!(fakes.engine.impact_calls(), 0);
}

#[tokio::test]
async fn impact_rejects_duplicate_ids_and_bad_json() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let dup = impact_body(&["a.b", "a.b"]);
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", dup),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert_eq!(res.json()["error"], "bad_request");

    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", r#"{"diff": 1}"#),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn impact_rejects_more_than_1000_journeys() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let ids: Vec<String> = (0..1001).map(|i| format!("a.j{i}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", impact_body(&refs)),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn impact_body_over_one_mebibyte_is_413() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/impact", vec![b' '; 1024 * 1024 + 1]),
    )
    .await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(res.json()["error"], "payload_too_large");
}

// ------------------------------------------------------------------ pr-context

#[tokio::test]
async fn pr_context_returns_pr_ticket_and_latest_gate() {
    let fakes = Fakes::with(
        FakeDecisionEngine::new(),
        FakeImpactAnalyzer::new(),
        FakeCodeHost::new(HOOK_SECRET).with_pr(pr(12)),
    );
    fakes
        .tracker
        .insert_issue(issue("SHOP-123", "fix cart", "", &["flow-change"]));
    let report = gate_report("run-1", Some(pr(12)), vec![]);
    fakes
        .store
        .record_gate_result(&report, &ReportOutcome::default())
        .await
        .unwrap();

    let res = send(
        guard_server::router(fakes.state()),
        api_get("/api/pr-context?number=12"),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let body = res.json();
    assert_eq!(body["pr"]["number"], 12);
    assert_eq!(body["pr"]["ticket_key"], "SHOP-123");
    assert_eq!(
        body["ticket"],
        json!({"key": "SHOP-123", "url": "memory://jira/browse/SHOP-123", "flow_change": true})
    );
    assert_eq!(body["latest_gate"]["report"]["run_id"], "run-1");
}

#[tokio::test]
async fn pr_context_without_tracker_or_gates_has_nulls() {
    let fakes = Fakes::with(
        FakeDecisionEngine::new(),
        FakeImpactAnalyzer::new(),
        FakeCodeHost::new(HOOK_SECRET).with_pr(pr(12)),
    );
    let state = fakes.bare_state().with_code_host(fakes.code_host.clone());
    let res = send(
        guard_server::router(state),
        api_get("/api/pr-context?number=12"),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["ticket"], json!(null));
    assert_eq!(res.json()["latest_gate"], json!(null));
}

#[tokio::test]
async fn pr_context_errors() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(
        guard_server::router(fakes.state()),
        api_get("/api/pr-context"),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = send(
        guard_server::router(fakes.state()),
        api_get("/api/pr-context?number=x"),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = send(
        guard_server::router(fakes.state()),
        api_get("/api/pr-context?number=99"),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert_eq!(res.json()["error"], "not_found");
    let res = send(
        guard_server::router(fakes.bare_state()),
        api_get("/api/pr-context?number=1"),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
}

// ------------------------------------------------------------------ gate-result

pub fn gate_report(run_id: &str, pr: Option<PrContext>, tests: Vec<GateTest>) -> GateReport {
    GateReport {
        version: 2,
        run_id: run_id.into(),
        generated_at: NOW.into(),
        provider: Some(CodeHostProvider::Github),
        repo: Some("acme/shop".into()),
        ticket_key: pr.as_ref().and_then(|p| p.ticket_key.clone()),
        pr,
        ticket_is_flow_change: false,
        head_sha: "b".repeat(40),
        base_sha: Some("a".repeat(40)),
        verdict: if tests
            .iter()
            .any(|t| t.blocking && t.status == TestStatus::Failed)
        {
            GateVerdict::Red
        } else {
            GateVerdict::Green
        },
        counts: GateCounts::default(),
        tests,
        journey_changes: vec![],
        change_flags: vec![],
        report_url: Some("https://minio/report.html".into()),
        ci_url: Some("https://ci/run/1".into()),
    }
}

pub fn failed_test(fp: &str, baseline: BaselineOutcome, screenshot_key: Option<&str>) -> GateTest {
    let files = screenshot_key
        .map(|key| {
            vec![EvidenceFile {
                kind: EvidenceKind::Screenshot,
                name: "failure.png".into(),
                content_type: "image/png".into(),
                key: Some(key.into()),
                url: None,
                viewer_url: None,
            }]
        })
        .unwrap_or_default();
    GateTest {
        test_id: format!("chromium::tests/journeys/checkout/card.spec.ts::pays {fp}"),
        title: format!("pays {fp}"),
        file: "tests/journeys/checkout/card.spec.ts".into(),
        line: 14,
        project: "chromium".into(),
        tags: vec!["@journey:checkout.customer-card".into()],
        journey: Some("checkout.customer-card".into()),
        status: TestStatus::Failed,
        blocking: true,
        attempts: 3,
        duration_ms: 1200,
        error: Some("Timeout 5000ms exceeded".into()),
        baseline: Some(baseline),
        fingerprint: Some(Fingerprint(fp.into())),
        evidence: Some(EvidenceBundle {
            test_id: "id".into(),
            fingerprint: Fingerprint(fp.into()),
            title: "pays".into(),
            journey: Some("checkout.customer-card".into()),
            location: Some("tests/journeys/checkout/card.spec.ts:14".into()),
            error: "Timeout 5000ms exceeded".into(),
            step: None,
            seed: None,
            commit: "b".repeat(40),
            attempts: vec![],
            baseline: Some(baseline),
            files,
        }),
    }
}

#[tokio::test]
async fn gate_result_creates_a_bug_then_comments_on_redelivery_dedup_by_fingerprint() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    fakes
        .tracker
        .insert_issue(issue("SHOP-123", "fix cart", "", &[]));
    fakes
        .evidence
        .put_object(
            "runs/1/failure.png",
            vec![0x89, b'P', b'N', b'G'],
            "image/png",
        )
        .await
        .unwrap();
    let report = gate_report(
        "run-1",
        Some(pr(12)),
        vec![failed_test(
            "1a2b3c4d",
            BaselineOutcome::Regression,
            Some("runs/1/failure.png"),
        )],
    );
    let body = serde_json::to_string(&report).unwrap();

    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", body.clone()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let json = res.json();
    assert_eq!(json["id"], 1);
    let bug = &json["outcome"]["bugs"][0];
    assert_eq!(bug["fingerprint"], "1a2b3c4d");
    assert_eq!(bug["action"], "created");
    let key = bug["key"].as_str().unwrap().to_string();
    assert_eq!(json["outcome"]["ticket_comment"], "SHOP-123");
    // The server never publishes to the code host (finalize does).
    assert_eq!(json["outcome"]["pr_comment"], false);
    assert_eq!(json["outcome"]["status_published"], false);
    assert!(fakes.code_host.statuses().is_empty());
    assert!(fakes.code_host.comments(12).is_empty());

    let created = fakes.tracker.created_bugs();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].project, "SHOP");
    assert!(created[0].labels.contains(&"guard-fp-1a2b3c4d".to_string()));
    assert!(created[0].labels.contains(&"guard-regression".to_string()));
    assert_eq!(created[0].linked_issue.as_deref(), Some("SHOP-123"));
    assert_eq!(fakes.tracker.attachments(&key).len(), 1);
    assert_eq!(fakes.tracker.comments("SHOP-123").len(), 1);

    // A second run with the same fingerprint comments on the open Bug.
    let report2 = gate_report(
        "run-2",
        Some(pr(12)),
        vec![failed_test("1a2b3c4d", BaselineOutcome::Regression, None)],
    );
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", serde_json::to_string(&report2).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let json = res.json();
    assert_eq!(json["id"], 2);
    assert_eq!(json["outcome"]["bugs"][0]["action"], "commented");
    assert_eq!(json["outcome"]["bugs"][0]["key"], key.as_str());
    assert_eq!(fakes.tracker.created_bugs().len(), 1);
    assert_eq!(fakes.tracker.comments(&key).len(), 1);

    // Re-sending run-1 keeps its id (idempotent on (repo, run_id)) and does
    // not report to Jira again: same outcome, no new comment or attachment.
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", body),
    )
    .await;
    assert_eq!(res.json()["id"], 1);
    assert_eq!(res.json()["outcome"]["bugs"][0]["action"], "created");
    assert_eq!(fakes.tracker.created_bugs().len(), 1);
    assert_eq!(fakes.tracker.comments(&key).len(), 1);
    assert_eq!(fakes.tracker.comments("SHOP-123").len(), 2);
    assert_eq!(fakes.tracker.attachments(&key).len(), 1);
}

/// A reporter that never finishes (a hanging Jira).
struct Hanging;

#[async_trait::async_trait]
impl guard_server::GateReporter for Hanging {
    async fn report(&self, _: &GateReport) -> ReportOutcome {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_hanging_reporter_still_stores_the_gate_result() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let state = fakes
        .state()
        .with_reporter(Arc::new(Hanging))
        .with_report_timeout(std::time::Duration::from_millis(100));
    let report = gate_report(
        "run-1",
        Some(pr(12)),
        vec![failed_test("1a2b3c4d", BaselineOutcome::Regression, None)],
    );
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        send(
            guard_server::router(state),
            api_post("/api/gate-result", serde_json::to_string(&report).unwrap()),
        ),
    )
    .await
    .expect("answered before the reporter finished");
    assert_eq!(res.status, StatusCode::OK, "{}", res.text());
    let warnings = res.json()["outcome"]["warnings"].to_string();
    assert!(warnings.contains("timed out"), "{warnings}");
    let stored = fakes.store.recent_gate_results(10).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].report.run_id, "run-1");
}

#[tokio::test]
async fn concurrent_deliveries_of_one_run_report_once() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let state = fakes.state();
    let report = gate_report(
        "run-1",
        Some(pr(12)),
        vec![failed_test("1a2b3c4d", BaselineOutcome::Regression, None)],
    );
    let body = serde_json::to_string(&report).unwrap();
    let router = guard_server::router(state);
    let (a, b) = tokio::join!(
        send(router.clone(), api_post("/api/gate-result", body.clone())),
        send(router, api_post("/api/gate-result", body)),
    );
    assert_eq!(a.json()["id"], b.json()["id"]);
    assert_eq!(fakes.tracker.created_bugs().len(), 1);
}

#[tokio::test]
async fn pre_existing_failures_get_a_high_priority_unlinked_bug() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let report = gate_report(
        "run-1",
        None,
        vec![failed_test("deadbeef", BaselineOutcome::PreExisting, None)],
    );
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", serde_json::to_string(&report).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let created = fakes.tracker.created_bugs();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].priority.as_deref(), Some("Highest"));
    assert!(created[0].linked_issue.is_none());
    assert!(
        created[0]
            .labels
            .contains(&"guard-pre-existing".to_string())
    );
    assert_eq!(res.json()["outcome"]["ticket_comment"], json!(null));
}

#[tokio::test]
async fn unconfirmed_and_flaky_failures_are_not_reported_as_bugs() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut flaky = failed_test("00000001", BaselineOutcome::Flaky, None);
    flaky.status = TestStatus::Failed;
    let unavailable = failed_test("00000002", BaselineOutcome::Unavailable, None);
    let report = gate_report("run-1", None, vec![flaky, unavailable]);
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", serde_json::to_string(&report).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(fakes.tracker.created_bugs().is_empty());
    assert_eq!(res.json()["outcome"]["bugs"], json!([]));
}

#[tokio::test]
async fn at_most_ten_bugs_per_run() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let tests = (0..12)
        .map(|i| failed_test(&format!("{i:08x}"), BaselineOutcome::Regression, None))
        .collect();
    let report = gate_report("run-1", None, tests);
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", serde_json::to_string(&report).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(fakes.tracker.created_bugs().len(), 10);
    let warnings = res.json()["outcome"]["warnings"].to_string();
    assert!(warnings.contains("limit reached"), "{warnings}");
}

#[tokio::test]
async fn gate_result_without_tracker_still_stores_with_a_warning() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let state = fakes.bare_state().with_gates(fakes.store.clone());
    let report = gate_report(
        "run-1",
        None,
        vec![failed_test("1a2b3c4d", BaselineOutcome::Regression, None)],
    );
    let res = send(
        guard_server::router(state),
        api_post("/api/gate-result", serde_json::to_string(&report).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(
        !res.json()["outcome"]["warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fakes.store.recent_gate_results(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn gate_result_errors() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut v1 = gate_report("run-1", None, vec![]);
    v1.version = 1;
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", serde_json::to_string(&v1).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", "{}"),
    )
    .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);

    let ok = serde_json::to_string(&gate_report("run-1", None, vec![])).unwrap();
    let res = send(
        guard_server::router(fakes.bare_state()),
        api_post("/api/gate-result", ok),
    )
    .await;
    assert_eq!(res.status, StatusCode::NOT_IMPLEMENTED);
}

struct DownGates;

#[async_trait::async_trait]
impl GateHistory for DownGates {
    async fn record_gate_result(
        &self,
        _: &GateReport,
        _: &ReportOutcome,
    ) -> Result<GateResultId, StoreError> {
        Err(StoreError::Unavailable("pool timed out".into()))
    }
    async fn recent_gate_results(&self, _: u32) -> Result<Vec<StoredGateResult>, StoreError> {
        Err(StoreError::Unavailable("pool timed out".into()))
    }
    async fn latest_for_pr(&self, _: &str, _: u64) -> Result<Option<StoredGateResult>, StoreError> {
        Err(StoreError::Unavailable("pool timed out".into()))
    }
}

#[tokio::test]
async fn gate_result_with_store_down_is_503() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut state = fakes.state();
    state.gates = Some(Arc::new(DownGates));
    let ok = serde_json::to_string(&gate_report("run-1", None, vec![])).unwrap();
    let res = send(
        guard_server::router(state),
        api_post("/api/gate-result", ok),
    )
    .await;
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.json()["error"], "store_unavailable");
}

#[tokio::test]
async fn gate_result_body_limit_is_eight_mebibytes() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    // 2 MiB is fine for gate results (over the 1 MiB webhook limit).
    let mut report = gate_report("run-1", None, vec![]);
    report.change_flags = vec!["x".repeat(2 * 1024 * 1024)];
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", serde_json::to_string(&report).unwrap()),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    let res = send(
        guard_server::router(fakes.state()),
        api_post("/api/gate-result", vec![b' '; 8 * 1024 * 1024 + 1]),
    )
    .await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE);
}

// ------------------------------------------------------------------ healthz

#[tokio::test]
async fn healthz_reports_configured_adapters_without_auth() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let res = send(guard_server::router(fakes.state()), get("/healthz")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.json(),
        json!({"status": "ok", "version": "0.1.0", "checks": {
            "database": "ok", "engine": "configured", "tracker": "configured",
            "code_host": "configured", "evidence": "configured"
        }})
    );
    let res = send(guard_server::router(fakes.bare_state()), get("/healthz")).await;
    assert_eq!(res.json()["checks"]["database"], "disabled");
    assert_eq!(res.json()["checks"]["tracker"], "disabled");
    assert_eq!(res.json()["checks"]["evidence"], "disabled");

    let mut state = fakes.state();
    state.gates = Some(Arc::new(DownGates));
    let res = send(guard_server::router(state), get("/healthz")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.json()["checks"]["database"], "unavailable");
}
