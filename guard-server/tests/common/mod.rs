//! Shared harness: an `AppState` wired with the in-memory fakes of every port.
#![allow(dead_code)]

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use guard_core::catalog::{Catalog, parse_catalog};
use guard_core::config::{TargetConfig, parse_target};
use guard_core::ports::fakes::{
    FAKE_WEBHOOK_HEADER, FakeCodeHost, FakeDecisionEngine, FakeImpactAnalyzer,
    InMemoryEvidenceStore, InMemoryStore, InMemoryTracker,
};
use guard_core::ports::*;
use guard_server::{AppState, FixedClock};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

pub const API_TOKEN: &str = "api-secret";
pub const HOOK_SECRET: &str = "hook-secret";
pub const JIRA_SECRET: &str = "jira-secret";
pub const NOW: &str = "2026-11-01T12:00:00Z";

pub const TARGET: &str = r#"
version = 1

[app]
name = "shop"
base_url = "http://127.0.0.1:9"

[areas.checkout]
description = "Cart, payment and order confirmation"
paths = ["web/src/checkout/**"]

[areas.billing]
description = "Invoices"
paths = ["services/billing/**"]
sensitive = true

[hard_rules]
human_only_paths = ["infra/**"]
max_fan_out_for_agent = 10

[tracker]
kind = "jira"
project = "SHOP"
ticket_pattern = 'SHOP-\d+'
"#;

pub const CATALOG: &str = r#"{
  "version": 1,
  "journeys": [
    {"id": "checkout.customer-card", "area": "checkout", "title": "Customer pays by card",
     "impact_question": "This diff can change checkout.", "paths": ["web/src/checkout/**"],
     "smoke": true, "quarantined": false},
    {"id": "billing.invoice-pdf", "area": "billing", "title": "Invoice PDF",
     "impact_question": "This diff can change invoices.", "paths": ["services/billing/**"],
     "smoke": false, "quarantined": false},
    {"id": "billing.refund", "area": "billing", "title": "Refund",
     "impact_question": "This diff can change refunds.", "paths": ["services/billing/**"],
     "smoke": false, "quarantined": true}
  ]
}"#;

pub fn config() -> TargetConfig {
    parse_target(TARGET).expect("valid target.toml")
}

pub fn catalog() -> Catalog {
    parse_catalog(CATALOG).expect("valid catalog")
}

/// Every port faked; all optional adapters configured.
pub struct Fakes {
    pub engine: Arc<FakeDecisionEngine>,
    pub tracker: Arc<InMemoryTracker>,
    pub code_host: Arc<FakeCodeHost>,
    pub store: Arc<InMemoryStore>,
    pub evidence: Arc<InMemoryEvidenceStore>,
    pub impact: Arc<FakeImpactAnalyzer>,
}

impl Fakes {
    pub fn new(engine: FakeDecisionEngine) -> Self {
        Self::with(
            engine,
            FakeImpactAnalyzer::new(),
            FakeCodeHost::new(HOOK_SECRET),
        )
    }

    pub fn with(
        engine: FakeDecisionEngine,
        impact: FakeImpactAnalyzer,
        host: FakeCodeHost,
    ) -> Self {
        Self {
            engine: Arc::new(engine),
            tracker: Arc::new(InMemoryTracker::new("SHOP")),
            code_host: Arc::new(host),
            store: Arc::new(InMemoryStore::new()),
            evidence: Arc::new(InMemoryEvidenceStore::new()),
            impact: Arc::new(impact),
        }
    }

    /// Fully configured state (the tracker wrapper checks the Jira secret).
    pub fn state(&self) -> AppState {
        AppState::new(
            self.engine.clone(),
            self.impact.clone(),
            config(),
            catalog(),
            Thresholds::default(),
            API_TOKEN.to_string(),
        )
        .with_tracker(Arc::new(SecretTracker {
            inner: self.tracker.clone(),
        }))
        .with_code_host(self.code_host.clone())
        .with_decisions(self.store.clone())
        .with_gates(self.store.clone())
        .with_evidence(self.evidence.clone())
        .with_clock(Arc::new(FixedClock(NOW.to_string())))
    }

    /// Only the mandatory ports (engine + impact).
    pub fn bare_state(&self) -> AppState {
        AppState::new(
            self.engine.clone(),
            self.impact.clone(),
            config(),
            catalog(),
            Thresholds::default(),
            API_TOKEN.to_string(),
        )
        .with_clock(Arc::new(FixedClock(NOW.to_string())))
    }
}

/// `InMemoryTracker` plus the Jira webhook authentication of the contract:
/// `Authorization: Bearer <secret>` or `?secret=<secret>`.
pub struct SecretTracker {
    pub inner: Arc<InMemoryTracker>,
}

#[async_trait::async_trait]
impl IssueTracker for SecretTracker {
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
        self.inner.get_issue(key).await
    }
    fn parse_webhook(&self, request: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        let bearer = request.header("authorization") == Some(&format!("Bearer {JIRA_SECRET}"));
        let query = request
            .query
            .as_deref()
            .is_some_and(|q| q.split('&').any(|p| p == format!("secret={JIRA_SECRET}")));
        if !(bearer || query) {
            return Err(TrackerWebhookError::Unauthorized);
        }
        self.inner.parse_webhook(request)
    }
}

/// A `FakeCodeHost` that reports itself as GitLab.
pub struct GitlabHost(pub FakeCodeHost);

#[async_trait::async_trait]
impl CodeHost for GitlabHost {
    fn provider(&self) -> CodeHostProvider {
        CodeHostProvider::Gitlab
    }
    async fn pr_context(
        &self,
        pr: PrRef,
        pattern: Option<&regex::Regex>,
    ) -> Result<PrContext, CodeHostError> {
        self.0.pr_context(pr, pattern).await
    }
    async fn publish_status(&self, status: &GateStatus) -> Result<(), CodeHostError> {
        self.0.publish_status(status).await
    }
    async fn comment_pr(&self, pr: PrRef, body: &str) -> Result<CommentRef, CodeHostError> {
        self.0.comment_pr(pr, body).await
    }
    fn verify_webhook(&self, request: &WebhookRequest) -> Result<(), WebhookError> {
        self.0.verify_webhook(request)
    }
    fn parse_webhook(&self, request: &WebhookRequest) -> Result<CodeHostEvent, WebhookError> {
        self.0.parse_webhook(request)
    }
}

pub fn issue(key: &str, summary: &str, description: &str, labels: &[&str]) -> Issue {
    Issue {
        key: key.to_string(),
        url: format!("memory://jira/browse/{key}"),
        summary: summary.to_string(),
        description: description.to_string(),
        issue_type: "Story".into(),
        status: "To Do".into(),
        status_category: StatusCategory::ToDo,
        labels: labels.iter().map(|l| l.to_string()).collect(),
    }
}

/// Engine answers that pass every calibrated check of the default thresholds.
pub fn confident_answers(area: &str) -> TriageAnswers {
    TriageAnswers {
        model: "jev-1.13.0".into(),
        area: Classified {
            value: Area::new(area),
            confidence: 0.95,
        },
        kind: Classified {
            value: Kind::Bugfix,
            confidence: 0.9,
        },
        risk: Classified {
            value: 0.2,
            confidence: 0.9,
        },
        clear_e2e_criteria: 0.9,
        touches_money_or_security: 0.01,
        agent_suitable: 0.95,
        changes_existing_flow: 0.05,
        raw: serde_json::json!({}),
    }
}

pub fn pr(number: u64) -> PrContext {
    PrContext {
        provider: CodeHostProvider::Github,
        repo: "acme/shop".into(),
        number,
        title: "SHOP-123 fix cart".into(),
        url: format!("https://github.com/acme/shop/pull/{number}"),
        author: "dev".into(),
        ticket_key: Some("SHOP-123".into()),
        branch: "SHOP-123-fix-cart".into(),
        base_branch: "main".into(),
        base_sha: "a".repeat(40),
        head_sha: "b".repeat(40),
        changed_files: vec!["web/src/checkout/cart.ts".into()],
    }
}

pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Response {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "body is not JSON ({e}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }
}

pub async fn send(app: Router, request: Request<Body>) -> Response {
    let response = app.oneshot(request).await.expect("infallible router");
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    Response {
        status,
        headers,
        body,
    }
}

pub fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

pub fn api_get(uri: &str) -> Request<Body> {
    Request::get(uri)
        .header("authorization", format!("Bearer {API_TOKEN}"))
        .body(Body::empty())
        .unwrap()
}

pub fn api_post(uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(uri)
        .header("authorization", format!("Bearer {API_TOKEN}"))
        .header("content-type", "application/json")
        .body(body.into())
        .unwrap()
}

pub fn code_host_hook(
    uri: &str,
    event_header: (&str, &str),
    body: serde_json::Value,
) -> Request<Body> {
    Request::post(uri)
        .header(FAKE_WEBHOOK_HEADER, HOOK_SECRET)
        .header(event_header.0, event_header.1)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub fn jira_hook(body: serde_json::Value) -> Request<Body> {
    Request::post("/webhooks/jira")
        .header("authorization", format!("Bearer {JIRA_SECRET}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}
