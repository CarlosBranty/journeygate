//! HTTP behavior of `JiraTracker` against a wiremock Jira (recorded fixtures
//! under `tests/fixtures/`).

use guard_core::ports::{
    Attachment, IssueTracker, NewBug, StatusCategory, TrackerError, TrackerEvent, WebhookRequest,
};
use guard_tracker::{JiraConfig, JiraTracker};
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

const EMAIL: &str = "guard@acme.test";
const TOKEN: &str = "api-token-123";
/// base64("guard@acme.test:api-token-123")
const BASIC: &str = "Basic Z3VhcmRAYWNtZS50ZXN0OmFwaS10b2tlbi0xMjM=";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn fixture_json(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn config(server: &MockServer) -> JiraConfig {
    let env = |name: &str| -> Option<String> {
        match name {
            "JIRA_BASE_URL" => Some(format!("{}/", server.uri())),
            "JIRA_EMAIL" => Some(EMAIL.into()),
            "MY_JIRA_TOKEN" => Some(TOKEN.into()),
            "GUARD_JIRA_WEBHOOK_SECRET" => Some("s3cret".into()),
            _ => None,
        }
    };
    JiraConfig::from_lookup("SHOP", "MY_JIRA_TOKEN", env).unwrap()
}

fn tracker(server: &MockServer) -> JiraTracker {
    JiraTracker::new(config(server)).unwrap()
}

fn doc() -> Value {
    json!({"type": "doc", "version": 1, "content": [
        {"type": "paragraph", "content": [{"type": "text", "text": "hi"}]}
    ]})
}

async fn status_mock(server: &MockServer, verb: &str, status: u16, body: &str) {
    Mock::given(method(verb))
        .respond_with(ResponseTemplate::new(status).set_body_string(body.to_string()))
        .mount(server)
        .await;
}

// ------------------------------------------------------------ config

#[test]
fn config_reads_the_token_from_the_named_variable_and_trims_the_base_url() {
    let env = |name: &str| -> Option<String> {
        match name {
            "JIRA_BASE_URL" => Some("https://acme.atlassian.net/".into()),
            "JIRA_EMAIL" => Some(EMAIL.into()),
            "CUSTOM_TOKEN" => Some(TOKEN.into()),
            "JIRA_SUGGESTED_FIELD" => Some("customfield_10123".into()),
            "JIRA_ACCEPTANCE_CRITERIA_FIELD" => Some("customfield_10050".into()),
            _ => None,
        }
    };
    let cfg = JiraConfig::from_lookup("SHOP", "CUSTOM_TOKEN", env).unwrap();
    assert_eq!(cfg.base_url, "https://acme.atlassian.net");
    assert_eq!(cfg.email, EMAIL);
    assert_eq!(cfg.project, "SHOP");
    assert_eq!(cfg.suggested_field.as_deref(), Some("customfield_10123"));
    assert_eq!(
        cfg.acceptance_criteria_field.as_deref(),
        Some("customfield_10050")
    );
    assert!(cfg.webhook_secret.is_none());
    // The token never leaks through Debug.
    assert!(!format!("{cfg:?}").contains(TOKEN));
}

#[test]
fn config_missing_or_empty_variables_are_unconfigured() {
    let env = |name: &str| -> Option<String> {
        match name {
            "JIRA_BASE_URL" => Some("https://acme.atlassian.net".into()),
            "JIRA_EMAIL" => Some("  ".into()),
            _ => None,
        }
    };
    match JiraConfig::from_lookup("SHOP", "JIRA_API_TOKEN", env) {
        Err(TrackerError::Unconfigured(msg)) => {
            assert!(msg.contains("JIRA_EMAIL"), "{msg}");
            assert!(msg.contains("JIRA_API_TOKEN"), "{msg}");
        }
        other => panic!("expected Unconfigured, got {other:?}"),
    }
}

// ------------------------------------------------------------ find_by_label

#[tokio::test]
async fn find_by_label_posts_the_enhanced_jql_search() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(header("authorization", BASIC))
        .and(header("accept", "application/json"))
        .and(body_json(json!({
            "jql": "project = \"SHOP\" AND labels = \"guard-fp-1a2b3c4d\" ORDER BY created DESC",
            "fields": ["summary", "status", "labels"],
            "maxResults": 10
        })))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("search_jql.json")))
        .expect(1)
        .mount(&server)
        .await;

    let found = tracker(&server)
        .find_by_label("guard-fp-1a2b3c4d")
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].key, "SHOP-210");
    assert_eq!(found[0].url, format!("{}/browse/SHOP-210", server.uri()));
    assert_eq!(found[0].status_category, StatusCategory::InProgress);
    assert_eq!(found[1].status_category, StatusCategory::Done);
}

#[tokio::test]
async fn find_by_label_escapes_quotes_in_jql() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/search/jql"))
        .and(body_json(json!({
            "jql": "project = \"SHOP\" AND labels = \"a\\\"b\" ORDER BY created DESC",
            "fields": ["summary", "status", "labels"],
            "maxResults": 10
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issues": []})))
        .expect(1)
        .mount(&server)
        .await;
    assert!(
        tracker(&server)
            .find_by_label("a\"b")
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn http_errors_map_to_tracker_errors() {
    for (status, body, check) in [
        (
            401u16,
            fixture("error_401.json"),
            (|e: TrackerError| matches!(e, TrackerError::Unauthorized(401)))
                as fn(TrackerError) -> bool,
        ),
        (403, String::new(), |e| {
            matches!(e, TrackerError::Unauthorized(403))
        }),
        (429, String::new(), |e| {
            matches!(e, TrackerError::RateLimited)
        }),
        (400, fixture("error_400.json"), |e| match e {
            TrackerError::Rejected { status, body } => status == 400 && body.contains("label"),
            _ => false,
        }),
        (503, "down".into(), |e| {
            matches!(e, TrackerError::Rejected { status: 503, .. })
        }),
    ] {
        let server = MockServer::start().await;
        status_mock(&server, "POST", status, &body).await;
        let err = tracker(&server).find_by_label("x").await.unwrap_err();
        assert!(check(err.clone()), "HTTP {status} mapped to {err:?}");
    }
}

#[tokio::test]
async fn transport_failure_is_a_transport_error() {
    // wiremock pools servers (a dropped one keeps listening): use a closed port.
    let server = MockServer::start().await;
    let mut cfg = config(&server);
    cfg.base_url = "http://127.0.0.1:1".into();
    let err = JiraTracker::new(cfg)
        .unwrap()
        .find_by_label("x")
        .await
        .unwrap_err();
    assert!(matches!(err, TrackerError::Transport(_)), "{err:?}");
}

// ------------------------------------------------------------ create_bug

fn new_bug(priority: Option<&str>, linked: Option<&str>) -> NewBug {
    NewBug {
        project: "SHOP".into(),
        summary: "[guard] checkout.customer-card: Timeout <n>ms exceeded".into(),
        description_adf: doc(),
        labels: vec![
            "guard".into(),
            "guard-regression".into(),
            "guard-fp-1a2b3c4d".into(),
        ],
        priority: priority.map(Into::into),
        linked_issue: linked.map(Into::into),
    }
}

#[tokio::test]
async fn create_bug_sends_labels_priority_and_the_relates_link() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .and(header("authorization", BASIC))
        .and(body_json(json!({
            "fields": {
                "project": {"key": "SHOP"},
                "issuetype": {"name": "Bug"},
                "summary": "[guard] checkout.customer-card: Timeout <n>ms exceeded",
                "description": doc(),
                "labels": ["guard", "guard-regression", "guard-fp-1a2b3c4d"],
                "priority": {"name": "Highest"}
            },
            "update": {"issuelinks": [
                {"add": {"type": {"name": "Relates"}, "outwardIssue": {"key": "SHOP-123"}}}
            ]}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_string(fixture("create_issue.json")))
        .expect(1)
        .mount(&server)
        .await;

    let created = tracker(&server)
        .create_bug(&new_bug(Some("Highest"), Some("SHOP-123")))
        .await
        .unwrap();
    assert_eq!(created.key, "SHOP-211");
    assert_eq!(created.url, format!("{}/browse/SHOP-211", server.uri()));
    assert_eq!(created.status_category, StatusCategory::ToDo);
}

#[tokio::test]
async fn create_bug_omits_priority_and_update_when_unset() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue"))
        .and(body_json(json!({
            "fields": {
                "project": {"key": "SHOP"},
                "issuetype": {"name": "Bug"},
                "summary": "[guard] checkout.customer-card: Timeout <n>ms exceeded",
                "description": doc(),
                "labels": ["guard", "guard-regression", "guard-fp-1a2b3c4d"]
            }
        })))
        .respond_with(ResponseTemplate::new(201).set_body_string(fixture("create_issue.json")))
        .expect(1)
        .mount(&server)
        .await;
    tracker(&server)
        .create_bug(&new_bug(None, None))
        .await
        .unwrap();
}

#[tokio::test]
async fn create_bug_errors() {
    let server = MockServer::start().await;
    status_mock(&server, "POST", 401, &fixture("error_401.json")).await;
    let err = tracker(&server)
        .create_bug(&new_bug(None, None))
        .await
        .unwrap_err();
    assert_eq!(err, TrackerError::Unauthorized(401));

    let server = MockServer::start().await;
    status_mock(&server, "POST", 429, "").await;
    let err = tracker(&server)
        .create_bug(&new_bug(None, None))
        .await
        .unwrap_err();
    assert_eq!(err, TrackerError::RateLimited);

    let server = MockServer::start().await;
    status_mock(&server, "POST", 404, &fixture("error_404.json")).await;
    let err = tracker(&server)
        .create_bug(&new_bug(None, Some("SHOP-999")))
        .await
        .unwrap_err();
    assert!(matches!(err, TrackerError::NotFound(_)), "{err:?}");
}

// ------------------------------------------------------------ comment_adf

#[tokio::test]
async fn comment_adf_posts_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/SHOP-210/comment"))
        .and(header("authorization", BASIC))
        .and(body_json(json!({"body": doc()})))
        .respond_with(ResponseTemplate::new(201).set_body_string(fixture("add_comment.json")))
        .expect(1)
        .mount(&server)
        .await;
    let comment = tracker(&server)
        .comment_adf("SHOP-210", &doc())
        .await
        .unwrap();
    assert_eq!(comment.id, "10500");
    assert_eq!(
        comment.url.as_deref(),
        Some(format!("{}/browse/SHOP-210?focusedCommentId=10500", server.uri()).as_str())
    );
}

#[tokio::test]
async fn comment_adf_errors() {
    for (status, expected) in [
        (401u16, TrackerError::Unauthorized(401)),
        (404, TrackerError::NotFound("SHOP-404".into())),
        (429, TrackerError::RateLimited),
    ] {
        let server = MockServer::start().await;
        status_mock(&server, "POST", status, "{}").await;
        let err = tracker(&server)
            .comment_adf("SHOP-404", &doc())
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

// ------------------------------------------------------------ attach_file

/// Matches a multipart body with one part `file` carrying `file_name` and PNG bytes.
struct MultipartFile {
    file_name: &'static str,
    bytes: &'static [u8],
}

impl Match for MultipartFile {
    fn matches(&self, request: &Request) -> bool {
        let content_type = request
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let body = String::from_utf8_lossy(&request.body);
        content_type.starts_with("multipart/form-data; boundary=")
            && body.contains(&format!(
                "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"",
                self.file_name
            ))
            && body.contains("Content-Type: image/png")
            && request
                .body
                .windows(self.bytes.len())
                .any(|w| w == self.bytes)
    }
}

fn png() -> Attachment {
    Attachment {
        file_name: "customer-card-failed.png".into(),
        content_type: "image/png".into(),
        bytes: b"\x89PNG".to_vec(),
    }
}

#[tokio::test]
async fn attach_file_sends_multipart_field_file_with_no_check_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/SHOP-211/attachments"))
        .and(header("authorization", BASIC))
        .and(header("x-atlassian-token", "no-check"))
        .and(MultipartFile {
            file_name: "customer-card-failed.png",
            bytes: b"\x89PNG",
        })
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("add_attachment.json")))
        .expect(1)
        .mount(&server)
        .await;
    let att = tracker(&server)
        .attach_file("SHOP-211", &png())
        .await
        .unwrap();
    assert_eq!(att.id, "10077");
    assert_eq!(att.file_name, "customer-card-failed.png");
}

#[tokio::test]
async fn attach_file_errors() {
    for (status, expected) in [
        (401u16, TrackerError::Unauthorized(401)),
        (404, TrackerError::NotFound("SHOP-211".into())),
        (429, TrackerError::RateLimited),
    ] {
        let server = MockServer::start().await;
        status_mock(&server, "POST", status, "").await;
        let err = tracker(&server)
            .attach_file("SHOP-211", &png())
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
    // 200 with an empty list is a rejection, not a silent success.
    let server = MockServer::start().await;
    status_mock(&server, "POST", 200, "[]").await;
    let err = tracker(&server)
        .attach_file("SHOP-211", &png())
        .await
        .unwrap_err();
    assert!(
        matches!(err, TrackerError::Rejected { status: 200, .. }),
        "{err:?}"
    );
}

// ------------------------------------------------------------ labels

#[tokio::test]
async fn add_labels_puts_update_add_operations() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/SHOP-7"))
        .and(header("authorization", BASIC))
        .and(body_json(json!({"update": {"labels": [
            {"add": "area:catalog"}, {"add": "lane:agent"}
        ]}})))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    tracker(&server)
        .add_labels("SHOP-7", &["area:catalog".into(), "lane:agent".into()])
        .await
        .unwrap();
}

#[tokio::test]
async fn add_labels_with_no_labels_makes_no_call() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;
    tracker(&server).add_labels("SHOP-7", &[]).await.unwrap();
}

#[tokio::test]
async fn add_labels_errors() {
    for (status, expected) in [
        (401u16, TrackerError::Unauthorized(401)),
        (404, TrackerError::NotFound("SHOP-7".into())),
        (429, TrackerError::RateLimited),
    ] {
        let server = MockServer::start().await;
        status_mock(&server, "PUT", status, "").await;
        let err = tracker(&server)
            .add_labels("SHOP-7", &["x".into()])
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

#[tokio::test]
async fn suggest_labels_writes_the_configured_field() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/rest/api/3/issue/SHOP-7"))
        .and(body_json(
            json!({"fields": {"customfield_10123": ["area:catalog", "lane:agent"]}}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let mut cfg = config(&server);
    cfg.suggested_field = Some("customfield_10123".into());
    JiraTracker::new(cfg)
        .unwrap()
        .suggest_labels("SHOP-7", &["area:catalog".into(), "lane:agent".into()])
        .await
        .unwrap();
}

#[tokio::test]
async fn suggest_labels_without_a_field_posts_a_jev_suggested_comment() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/rest/api/3/issue/SHOP-7/comment"))
        .and(body_json(json!({"body": {"type": "doc", "version": 1, "content": [
            {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "jev-suggested"}]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "area:catalog"}]}]},
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "lane:agent"}]}]}
            ]}
        ]}})))
        .respond_with(ResponseTemplate::new(201).set_body_string(fixture("add_comment.json")))
        .expect(1)
        .mount(&server)
        .await;
    tracker(&server)
        .suggest_labels("SHOP-7", &["area:catalog".into(), "lane:agent".into()])
        .await
        .unwrap();
}

#[tokio::test]
async fn suggest_labels_errors() {
    let server = MockServer::start().await;
    status_mock(&server, "POST", 429, "").await;
    let err = tracker(&server)
        .suggest_labels("SHOP-7", &["x".into()])
        .await
        .unwrap_err();
    assert_eq!(err, TrackerError::RateLimited);
}

// ------------------------------------------------------------ get_issue

#[tokio::test]
async fn get_issue_flattens_the_adf_description() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/issue/SHOP-7"))
        .and(query_param(
            "fields",
            "summary,description,labels,issuetype,status",
        ))
        .and(header("authorization", BASIC))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("get_issue.json")))
        .expect(1)
        .mount(&server)
        .await;
    let issue = tracker(&server).get_issue("SHOP-7").await.unwrap();
    assert_eq!(issue.key, "SHOP-7");
    assert_eq!(issue.url, format!("{}/browse/SHOP-7", server.uri()));
    assert_eq!(
        issue.summary,
        "Show 'Sold out' badge on products without stock"
    );
    assert_eq!(issue.issue_type, "Story");
    assert_eq!(issue.status, "To Do");
    assert_eq!(issue.status_category, StatusCategory::ToDo);
    assert_eq!(issue.labels, vec!["frontend", "catalog"]);
    assert_eq!(
        issue.description,
        "Products with zero stock look buyable. Touch src/catalog/StockBadge.tsx.\n\n\
         Acceptance criteria\n\n\
         - A product with 0 available units shows 'Sold out'\n\
         - The add-to-cart button is disabled"
    );
}

#[tokio::test]
async fn get_issue_errors() {
    for (status, expected) in [
        (401u16, TrackerError::Unauthorized(401)),
        (404, TrackerError::NotFound("SHOP-7".into())),
        (429, TrackerError::RateLimited),
    ] {
        let server = MockServer::start().await;
        status_mock(&server, "GET", status, &fixture("error_404.json")).await;
        let err = tracker(&server).get_issue("SHOP-7").await.unwrap_err();
        assert_eq!(err, expected);
    }
}

#[tokio::test]
async fn issue_keys_are_validated_before_any_request() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;
    let err = tracker(&server)
        .get_issue("../../myself")
        .await
        .unwrap_err();
    assert!(
        matches!(err, TrackerError::Rejected { status: 0, .. }),
        "{err:?}"
    );
}

// ------------------------------------------------------------ myself + webhook

fn webhook(body: &str, auth: Option<&str>, query: Option<&str>) -> WebhookRequest {
    let mut req = WebhookRequest {
        body: body.as_bytes().to_vec(),
        query: query.map(Into::into),
        ..WebhookRequest::default()
    };
    if let Some(a) = auth {
        req.headers.insert("authorization".into(), a.into());
    }
    req
}

#[tokio::test]
async fn load_self_account_id_calls_myself_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .and(header("authorization", BASIC))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture("myself.json")))
        .expect(1)
        .mount(&server)
        .await;
    let t = tracker(&server);
    assert_eq!(
        t.load_self_account_id().await.unwrap(),
        "5b10ac8d82e05b22cc7d4ef5"
    );
    // Cached: no second request.
    assert_eq!(
        t.load_self_account_id().await.unwrap(),
        "5b10ac8d82e05b22cc7d4ef5"
    );
}

#[tokio::test]
async fn load_self_account_id_401() {
    let server = MockServer::start().await;
    status_mock(&server, "GET", 401, &fixture("error_401.json")).await;
    assert_eq!(
        tracker(&server).load_self_account_id().await.unwrap_err(),
        TrackerError::Unauthorized(401)
    );
}

#[tokio::test]
async fn parse_webhook_authenticates_and_maps_events() {
    let server = MockServer::start().await;
    let t = tracker(&server).with_self_account_id("5b10ac8d82e05b22cc7d4ef5");
    let created = fixture("webhook_issue_created.json");

    assert_eq!(
        t.parse_webhook(&webhook(&created, Some("Bearer s3cret"), None))
            .unwrap(),
        TrackerEvent::IssueCreated {
            key: "SHOP-7".into()
        }
    );
    assert_eq!(
        t.parse_webhook(&webhook(&created, None, Some("a=1&secret=s3cret")))
            .unwrap(),
        TrackerEvent::IssueCreated {
            key: "SHOP-7".into()
        }
    );
    assert!(t.parse_webhook(&webhook(&created, None, None)).is_err());
    assert!(
        t.parse_webhook(&webhook(&created, Some("Bearer wrong"), None))
            .is_err()
    );

    let labels = fixture("webhook_issue_updated_labels.json");
    assert_eq!(
        t.parse_webhook(&webhook(&labels, Some("Bearer s3cret"), None))
            .unwrap(),
        TrackerEvent::LabelsChanged {
            key: "SHOP-7".into(),
            labels: vec![
                "area:catalog".into(),
                "catalog".into(),
                "lane:agent".into(),
                "type:minor_feature".into()
            ],
        }
    );
}

#[tokio::test]
async fn parse_ticket_webhook_reads_the_configured_acceptance_criteria_field() {
    let server = MockServer::start().await;
    let mut cfg = config(&server);
    cfg.acceptance_criteria_field = Some("customfield_10050".into());
    let t = JiraTracker::new(cfg).unwrap();
    let parsed = t
        .parse_ticket_webhook(&webhook(
            &fixture("webhook_issue_created.json"),
            Some("Bearer s3cret"),
            None,
        ))
        .unwrap();
    let ticket = parsed.ticket.unwrap();
    assert_eq!(
        ticket.acceptance_criteria,
        vec![
            "A product with 0 available units shows 'Sold out'",
            "The add-to-cart button is disabled"
        ]
    );
    assert_eq!(
        fixture_json("webhook_issue_created.json")["issue"]["key"],
        json!(ticket.key)
    );
}

// ------------------------------------------------------------ guard connect

#[tokio::test]
async fn myself_for_credentials_returns_the_account_without_a_project() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        .and(header("authorization", BASIC))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "accountId": "a1",
            "displayName": "Ada Lovelace",
            "emailAddress": "ada@acme.test"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let config = JiraConfig::for_credentials(format!("{}/", server.uri()), EMAIL, TOKEN);
    assert_eq!(config.base_url, server.uri());
    let account = JiraTracker::new(config).unwrap().myself().await.unwrap();
    assert_eq!(account.display_name, "Ada Lovelace");
    assert_eq!(account.email.as_deref(), Some("ada@acme.test"));
}

#[tokio::test]
async fn myself_maps_rejected_credentials() {
    let server = MockServer::start().await;
    status_mock(&server, "GET", 403, "{}").await;
    let config = JiraConfig::for_credentials(server.uri(), EMAIL, TOKEN);
    let error = JiraTracker::new(config)
        .unwrap()
        .myself()
        .await
        .unwrap_err();
    assert_eq!(error, TrackerError::Unauthorized(403));
}
