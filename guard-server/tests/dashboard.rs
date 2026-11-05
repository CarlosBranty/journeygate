//! The read-only dashboard: auth, views, htmx fragments, empty states.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::*;
use guard_core::ports::fakes::FakeDecisionEngine;
use guard_core::ports::*;
use guard_server::auth::{AuthOutcome, AuthProvider, AuthRequest, Principal};
use std::sync::Arc;

const VIEWS: [&str; 8] = [
    "/",
    "/catalog",
    "/coverage",
    "/flaky",
    "/durations",
    "/guards",
    "/decisions",
    "/agents",
];

fn test_with(status: TestStatus, journey: &str, file: &str, duration_ms: u64) -> GateTest {
    GateTest {
        test_id: format!("chromium::{file}::{journey}"),
        title: journey.into(),
        file: file.into(),
        line: 3,
        project: "chromium".into(),
        tags: vec![],
        journey: Some(journey.into()),
        status,
        blocking: true,
        attempts: 1,
        duration_ms,
        error: None,
        baseline: None,
        fingerprint: None,
        evidence: None,
    }
}

fn report(run: &str, number: u64, tests: Vec<GateTest>, verdict: GateVerdict) -> GateReport {
    let mut pr = pr(number);
    pr.url = format!("https://github.com/acme/shop/pull/{number}");
    GateReport {
        version: 2,
        run_id: run.into(),
        generated_at: NOW.into(),
        provider: Some(CodeHostProvider::Github),
        repo: Some("acme/shop".into()),
        pr: Some(pr),
        ticket_key: Some("SHOP-123".into()),
        ticket_is_flow_change: false,
        head_sha: "b".repeat(40),
        base_sha: None,
        verdict,
        counts: GateCounts {
            regressions: usize::from(verdict == GateVerdict::Red),
            ..GateCounts::default()
        },
        tests,
        journey_changes: vec![JourneyChangeReport {
            journey: "checkout.customer-card".into(),
            file: "tests/journeys/checkout/card.spec.ts".into(),
            flags: vec!["assertions 5 -> 3".into()],
        }],
        change_flags: vec![],
        report_url: Some("https://minio/report.html".into()),
        ci_url: None,
    }
}

async fn seeded() -> Fakes {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let card = "tests/journeys/checkout/card.spec.ts";
    let pdf = "tests/journeys/billing/pdf.spec.ts";
    fakes
        .store
        .record_gate_result(
            &report(
                "run-1",
                11,
                vec![
                    test_with(TestStatus::Passed, "checkout.customer-card", card, 1000),
                    test_with(TestStatus::Flaky, "billing.invoice-pdf", pdf, 9000),
                ],
                GateVerdict::Green,
            ),
            &ReportOutcome::default(),
        )
        .await
        .unwrap();
    let outcome = ReportOutcome {
        bugs: vec![ReportedBug {
            fingerprint: Fingerprint("1a2b3c4d".into()),
            key: "SHOP-210".into(),
            url: "https://acme.atlassian.net/browse/SHOP-210".into(),
            action: BugAction::Created,
        }],
        ..ReportOutcome::default()
    };
    fakes
        .store
        .record_gate_result(
            &report(
                "run-2",
                12,
                vec![
                    test_with(TestStatus::Failed, "checkout.customer-card", card, 3000),
                    test_with(TestStatus::Passed, "billing.invoice-pdf", pdf, 7000),
                ],
                GateVerdict::Red,
            ),
            &outcome,
        )
        .await
        .unwrap();
    fakes
}

#[tokio::test]
async fn every_view_renders_html_when_open() {
    let fakes = seeded().await;
    for view in VIEWS {
        let res = send(guard_server::router(fakes.state()), get(view)).await;
        assert_eq!(res.status, StatusCode::OK, "{view}");
        assert_eq!(
            res.header("content-type"),
            Some("text/html; charset=utf-8"),
            "{view}"
        );
        let html = res.text();
        assert!(html.starts_with("<!doctype html>"), "{view}: {html}");
        assert!(html.contains("<main"), "{view}");
        assert!(html.contains("/assets/htmx-2.0.4.min.js"), "{view}");
        assert!(!html.contains("https://unpkg.com"), "{view}: CDN-free");
    }
}

#[tokio::test]
async fn hx_request_returns_only_the_main_fragment() {
    let fakes = seeded().await;
    let request = Request::get("/guards")
        .header("hx-request", "true")
        .body(Body::empty())
        .unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::OK);
    let html = res.text();
    assert!(html.trim_start().starts_with("<main"), "{html}");
    assert!(!html.contains("<html"));
}

#[tokio::test]
async fn htmx_asset_is_served_inline_without_auth() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_token("dash".into());
    let res = send(
        guard_server::router(state),
        get("/assets/htmx-2.0.4.min.js"),
    )
    .await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(
        res.header("content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert!(res.text().starts_with("var htmx="));
}

#[tokio::test]
async fn overview_lists_recent_gates() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/"))
        .await
        .text();
    assert!(html.contains("run-1") && html.contains("run-2"), "{html}");
    for link in [
        "/catalog",
        "/coverage",
        "/flaky",
        "/durations",
        "/guards",
        "/decisions",
        "/agents",
    ] {
        assert!(html.contains(&format!("href=\"{link}\"")), "{link}");
    }
}

#[tokio::test]
async fn catalog_shows_journeys_by_area_with_last_result() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/catalog"))
        .await
        .text();
    assert!(html.contains("checkout.customer-card"));
    assert!(html.contains("@journey:checkout.customer-card"));
    assert!(html.contains("billing.refund"));
    // Latest run (run-2): card failed, pdf passed.
    let card_row = html
        .split("<tr")
        .find(|row| row.contains("checkout.customer-card"))
        .unwrap();
    assert!(card_row.contains("failed"), "{card_row}");
}

#[tokio::test]
async fn coverage_lists_areas_with_zero_or_one_active_journey() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/coverage"))
        .await
        .text();
    // checkout has 1 active journey, billing has 1 active (refund is quarantined).
    assert!(html.contains("checkout"));
    assert!(html.contains("billing"));
}

#[tokio::test]
async fn flaky_shows_rate() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/flaky"))
        .await
        .text();
    assert!(html.contains("billing.invoice-pdf"), "{html}");
    assert!(html.contains("50%"), "{html}");
}

#[tokio::test]
async fn durations_shows_slowest_files() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/durations"))
        .await
        .text();
    assert!(
        html.contains("tests/journeys/billing/pdf.spec.ts"),
        "{html}"
    );
}

#[tokio::test]
async fn guards_link_jira_and_evidence() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/guards"))
        .await
        .text();
    assert!(
        html.contains("https://acme.atlassian.net/browse/SHOP-210"),
        "{html}"
    );
    assert!(html.contains("https://minio/report.html"));
    assert!(html.contains("checkout.customer-card"));
    assert!(html.contains("red"));
}

#[tokio::test]
async fn decisions_show_triage_vs_human_label_and_precision() {
    let fakes =
        Fakes::new(FakeDecisionEngine::new().with_triage(Ok(confident_answers("checkout"))));
    fakes.tracker.insert_issue(issue("SHOP-1", "x", "", &[]));
    let state = fakes.state();
    guard_server::triage::triage_ticket(&state, "SHOP-1")
        .await
        .unwrap();
    fakes
        .store
        .record_human_label(&guard_core::triage::human_label_from_labels(
            "SHOP-1",
            &["lane:agent".to_string()],
            NOW,
            LabelSource::JiraWebhook,
        ))
        .await
        .unwrap();
    let html = send(guard_server::router(state), get("/decisions"))
        .await
        .text();
    assert!(html.contains("SHOP-1"), "{html}");
    assert!(html.contains("100%"), "agent-lane precision: {html}");
}

#[tokio::test]
async fn agents_is_a_placeholder() {
    let fakes = seeded().await;
    let html = send(guard_server::router(fakes.state()), get("/agents"))
        .await
        .text();
    assert!(html.contains("No agent runs yet"));
}

#[tokio::test]
async fn data_views_without_database_show_an_empty_state() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    for view in ["/", "/flaky", "/durations", "/guards", "/decisions"] {
        let res = send(guard_server::router(fakes.bare_state()), get(view)).await;
        assert_eq!(res.status, StatusCode::OK, "{view}");
        assert!(res.text().contains("Decision log not configured"), "{view}");
    }
}

#[tokio::test]
async fn html_is_escaped() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut r = report("<script>x</script>", 1, vec![], GateVerdict::Green);
    r.run_id = "<script>alert(1)</script>".into();
    fakes
        .store
        .record_gate_result(&r, &ReportOutcome::default())
        .await
        .unwrap();
    let html = send(guard_server::router(fakes.state()), get("/"))
        .await
        .text();
    assert!(!html.contains("<script>alert(1)</script>"));
    assert!(
        html.contains("&lt;script&gt;") || html.contains("&#60;script&#62;"),
        "{html}"
    );
}

// ------------------------------------------------------------------ auth

#[tokio::test]
async fn token_dev_provider_rejects_anonymous_requests() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_token("dash".into());
    let res = send(guard_server::router(state), get("/catalog")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert!(!res.text().contains("checkout.customer-card"));
}

#[tokio::test]
async fn token_dev_provider_accepts_bearer_and_cookie() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_token("dash".into());
    let bearer = Request::get("/catalog")
        .header("authorization", "Bearer dash")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(guard_server::router(state.clone()), bearer)
            .await
            .status,
        StatusCode::OK
    );
    let cookie = Request::get("/catalog")
        .header("cookie", "other=1; guard_dashboard=dash")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(guard_server::router(state.clone()), cookie)
            .await
            .status,
        StatusCode::OK
    );
    let wrong = Request::get("/catalog")
        .header("cookie", "guard_dashboard=nope")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(guard_server::router(state), wrong).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn token_query_sets_the_cookie_and_redirects_without_it() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_token("dash".into());
    let res = send(guard_server::router(state), get("/guards?a=1&token=dash")).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.header("location"), Some("/guards?a=1"));
    let cookie = res.header("set-cookie").unwrap().to_string();
    assert!(cookie.starts_with("guard_dashboard="), "{cookie}");
    // A signed, expiring session: never the long-lived token itself.
    assert!(!cookie.starts_with("guard_dashboard=dash;"), "{cookie}");
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains("Max-Age="), "{cookie}");
    assert!(!cookie.contains("Secure"), "plain HTTP: {cookie}");
    // The session cookie logs in.
    let session = cookie.split(';').next().unwrap().to_string();
    let request = Request::get("/catalog")
        .header("cookie", session.clone())
        .body(Body::empty())
        .unwrap();
    let state = fakes.state().with_dashboard_token("dash".into());
    assert_eq!(
        send(guard_server::router(state.clone()), request)
            .await
            .status,
        StatusCode::OK
    );
    // A tampered session does not.
    let tampered = format!("{session}0");
    let request = Request::get("/catalog")
        .header("cookie", tampered)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(guard_server::router(state.clone()), request)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Behind a TLS proxy the cookie is Secure.
    let request = Request::get("/guards?token=dash")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())
        .unwrap();
    let res = send(guard_server::router(state), request).await;
    assert!(res.header("set-cookie").unwrap().contains("; Secure"));
}

#[tokio::test]
async fn wrong_token_query_is_401() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_token("dash".into());
    let res = send(guard_server::router(state), get("/guards?token=nope")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

struct DenyAll;

#[async_trait::async_trait]
impl AuthProvider for DenyAll {
    async fn authenticate(&self, _: &AuthRequest<'_>) -> AuthOutcome {
        AuthOutcome::Denied
    }
}

struct AllowOctocat;

#[async_trait::async_trait]
impl AuthProvider for AllowOctocat {
    async fn authenticate(&self, _: &AuthRequest<'_>) -> AuthOutcome {
        AuthOutcome::Allowed(Principal {
            login: "octocat".into(),
            provider: "github".into(),
        })
    }
}

#[tokio::test]
async fn a_custom_auth_provider_replaces_the_token_provider() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_auth(Arc::new(DenyAll));
    let res = send(guard_server::router(state), get("/")).await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);

    let state = fakes
        .state()
        .with_dashboard_token("dash".into())
        .with_dashboard_auth(Arc::new(AllowOctocat));
    let res = send(guard_server::router(state), get("/")).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.text().contains("octocat"));
}

#[tokio::test]
async fn dashboard_auth_does_not_guard_the_api_or_healthz() {
    let fakes = seeded().await;
    let state = fakes.state().with_dashboard_auth(Arc::new(DenyAll));
    let res = send(guard_server::router(state.clone()), get("/healthz")).await;
    assert_eq!(res.status, StatusCode::OK);
    let res = send(guard_server::router(state), api_get("/api/pr-context")).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn dashboard_never_writes() {
    let fakes = seeded().await;
    let request = Request::post("/catalog").body(Body::empty()).unwrap();
    let res = send(guard_server::router(fakes.state()), request).await;
    assert_eq!(res.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn stored_links_render_only_with_an_http_scheme_and_pages_carry_a_csp() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let mut evil = report("run-x", 13, vec![], GateVerdict::Red);
    evil.report_url = Some("javascript:alert(document.cookie)".into());
    evil.ci_url = Some("data:text/html,<script>alert(1)</script>".into());
    if let Some(pr) = evil.pr.as_mut() {
        pr.url = " JavaScript:alert(1)".into();
    }
    let outcome = ReportOutcome {
        bugs: vec![ReportedBug {
            fingerprint: Fingerprint("1a2b3c4d".into()),
            key: "SHOP-666".into(),
            url: "vbscript:msgbox(1)".into(),
            action: BugAction::Created,
        }],
        ..ReportOutcome::default()
    };
    fakes
        .store
        .record_gate_result(&evil, &outcome)
        .await
        .unwrap();
    let res = send(guard_server::router(fakes.state()), get("/guards")).await;
    assert_eq!(res.status, StatusCode::OK);
    let html = res.text().to_ascii_lowercase();
    for scheme in ["javascript:", "data:", "vbscript:"] {
        assert!(!html.contains(&format!("href=\"{scheme}")), "{scheme}");
        assert!(!html.contains(&format!("href=\" {scheme}")), "{scheme}");
    }
    assert!(html.contains("shop-666"), "the label is still shown");
    let csp = res
        .headers
        .get("content-security-policy")
        .expect("dashboard pages carry a CSP")
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("script-src 'self'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
}
