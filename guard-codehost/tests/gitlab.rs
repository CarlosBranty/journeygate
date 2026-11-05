//! GitLab adapter against a wiremock server and recorded fixtures.

use guard_codehost::PR_COMMENT_MARKER;
use guard_codehost::gitlab::{GitlabCodeHost, GitlabConfig};
use guard_core::ports::{
    CheckState, CodeHost, CodeHostError, CodeHostEvent, CodeHostProvider, GateStatus, PrAction,
    PrRef, WebhookError, WebhookRequest,
};
use guard_core::ticket::Regex;
use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HEAD: &str = "cccccccccccccccccccccccccccccccccccccccc";
const SECRET: &str = "gitlab-hook-secret";
/// `acme/platform/shop`, URL-encoded as one path segment.
const PROJECT: &str = "/api/v4/projects/acme%2Fplatform%2Fshop";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/gitlab/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/gitlab/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn host(server: &MockServer) -> GitlabCodeHost {
    GitlabCodeHost::new(
        GitlabConfig::new(
            format!("{}/api/v4", server.uri()),
            "acme/platform/shop",
            "glpat-test",
        )
        .with_webhook_secret(SECRET),
    )
    .unwrap()
}

fn status(state: CheckState) -> GateStatus {
    GateStatus {
        head_sha: HEAD.into(),
        name: "guard".into(),
        state,
        title: "guard: red — 1 regression".into(),
        summary: "ignored by GitLab".into(),
        details_url: Some("https://ci.example/1".into()),
    }
}

fn delivery(event: Option<&str>, body: Vec<u8>, token: Option<&str>) -> WebhookRequest {
    let mut req = WebhookRequest {
        body,
        ..WebhookRequest::default()
    };
    if let Some(event) = event {
        req.headers.insert("x-gitlab-event".into(), event.into());
    }
    if let Some(token) = token {
        req.headers.insert("x-gitlab-token".into(), token.into());
    }
    req
}

async fn mount_mr(server: &MockServer, fixture_name: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7")))
        .and(header("private-token", "glpat-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture(fixture_name)))
        .mount(server)
        .await;
}

// ------------------------------------------------------------ pr_context

#[tokio::test]
async fn pr_context_reads_the_mr_and_paginates_diffs() {
    let server = MockServer::start().await;
    mount_mr(&server, "merge_request.json").await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/diffs")))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-next-page", "")
                .set_body_json(fixture("mr_diffs_page2.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/diffs")))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-next-page", "2")
                .set_body_json(fixture("mr_diffs_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let pattern = Regex::new(r"SHOP-\d+").unwrap();
    let ctx = host(&server)
        .pr_context(PrRef { number: 7 }, Some(&pattern))
        .await
        .unwrap();
    assert_eq!(ctx.provider, CodeHostProvider::Gitlab);
    assert_eq!(ctx.repo, "acme/platform/shop");
    assert_eq!(ctx.number, 7);
    assert_eq!(ctx.title, "Draft: SHOP-77 checkout copy");
    assert_eq!(
        ctx.url,
        "https://gitlab.com/acme/platform/shop/-/merge_requests/7"
    );
    assert_eq!(ctx.author, "dev");
    assert_eq!(ctx.ticket_key.as_deref(), Some("SHOP-456"));
    assert_eq!(ctx.branch, "feature/SHOP-456-checkout");
    assert_eq!(ctx.base_branch, "main");
    assert_eq!(ctx.head_sha, HEAD);
    assert_eq!(ctx.base_sha, "dddddddddddddddddddddddddddddddddddddddd");
    assert_eq!(
        ctx.changed_files,
        vec![
            "app/checkout.rb",
            "app/new.rb",
            "app/old.rb",
            "db/schema.rb"
        ]
    );
}

#[tokio::test]
async fn pr_context_falls_back_to_the_legacy_changes_endpoint() {
    let server = MockServer::start().await;
    mount_mr(&server, "merge_request.json").await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/diffs")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/changes")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mr_changes.json")))
        .expect(1)
        .mount(&server)
        .await;
    // The title is used when the branch has no match.
    let pattern = Regex::new(r"SHOP-7\d").unwrap();
    let ctx = host(&server)
        .pr_context(PrRef { number: 7 }, Some(&pattern))
        .await
        .unwrap();
    assert_eq!(ctx.changed_files, vec!["lib/a.rb"]);
    assert_eq!(ctx.ticket_key.as_deref(), Some("SHOP-77"));
}

#[tokio::test]
async fn pr_context_without_diff_refs_is_rejected() {
    let server = MockServer::start().await;
    mount_mr(&server, "merge_request_no_diff_refs.json").await;
    let err = host(&server)
        .pr_context(PrRef { number: 7 }, None)
        .await
        .unwrap_err();
    assert!(matches!(err, CodeHostError::Rejected { .. }), "{err:?}");
}

#[tokio::test]
async fn numeric_project_ids_are_used_verbatim() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/42/merge_requests/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("merge_request.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/projects/42/merge_requests/7/diffs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let host = GitlabCodeHost::new(GitlabConfig::new(
        format!("{}/api/v4", server.uri()),
        "42",
        "glpat-test",
    ))
    .unwrap();
    let ctx = host.pr_context(PrRef { number: 7 }, None).await.unwrap();
    // The full path comes from the MR references, not from the numeric id.
    assert_eq!(ctx.repo, "acme/platform/shop");
}

#[tokio::test]
async fn http_errors_map_to_code_host_errors() {
    let cases: Vec<(u16, CodeHostError)> = vec![
        (404, CodeHostError::NotFound(7)),
        (401, CodeHostError::Unauthorized(401)),
        (403, CodeHostError::Unauthorized(403)),
        (429, CodeHostError::RateLimited),
        (
            502,
            CodeHostError::Rejected {
                status: 502,
                body: String::new(),
            },
        ),
    ];
    for (code, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("{PROJECT}/merge_requests/7")))
            .respond_with(ResponseTemplate::new(code))
            .mount(&server)
            .await;
        let err = host(&server)
            .pr_context(PrRef { number: 7 }, None)
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

// ------------------------------------------------------------ publish_status

#[tokio::test]
async fn publish_status_posts_a_commit_status() {
    for (state, expected) in [
        (CheckState::Pending, "pending"),
        (CheckState::Running, "running"),
        (CheckState::Success, "success"),
        (CheckState::Failure, "failed"),
        (CheckState::Error, "failed"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(format!("{PROJECT}/statuses/{HEAD}")))
            .and(body_json(json!({
                "state": expected,
                "name": "guard",
                "description": "guard: red — 1 regression",
                "target_url": "https://ci.example/1"
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(fixture("commit_status.json")))
            .expect(1)
            .mount(&server)
            .await;
        host(&server).publish_status(&status(state)).await.unwrap();
    }
}

#[tokio::test]
async fn publish_status_truncates_the_description_to_140_characters() {
    let server = MockServer::start().await;
    let mut long = status(CheckState::Success);
    long.title = "é".repeat(200);
    long.details_url = None;
    let expected: String = "é".repeat(139) + "…";
    Mock::given(method("POST"))
        .and(path(format!("{PROJECT}/statuses/{HEAD}")))
        .and(body_json(json!({
            "state": "success",
            "name": "guard",
            "description": expected,
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("commit_status.json")))
        .expect(1)
        .mount(&server)
        .await;
    host(&server).publish_status(&long).await.unwrap();
}

#[tokio::test]
async fn publish_status_treats_an_unchanged_state_as_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{PROJECT}/statuses/{HEAD}")))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"message": "Cannot transition status via :run from :running (Reason(s): Status cannot transition via \"run\")"}),
        ))
        .mount(&server)
        .await;
    host(&server)
        .publish_status(&status(CheckState::Running))
        .await
        .unwrap();
}

#[tokio::test]
async fn publish_status_reports_other_400s() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{PROJECT}/statuses/{HEAD}")))
        .respond_with(ResponseTemplate::new(400).set_body_string("{\"message\":\"bad\"}"))
        .mount(&server)
        .await;
    let err = host(&server)
        .publish_status(&status(CheckState::Running))
        .await
        .unwrap_err();
    assert!(
        matches!(err, CodeHostError::Rejected { status: 400, .. }),
        "{err:?}"
    );
}

// ------------------------------------------------------------ comment_pr

#[tokio::test]
async fn comment_pr_edits_the_marked_note_found_on_a_later_page() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("notes_page2.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .and(query_param("page", "1"))
        .and(query_param("sort", "asc"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-next-page", "2")
                .set_body_json(fixture("notes_page1.json")),
        )
        .mount(&server)
        .await;
    let body = format!("{PR_COMMENT_MARKER}\nnew");
    Mock::given(method("PUT"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes/13")))
        .and(body_json(json!({ "body": body })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("note_updated.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;
    let note = host(&server)
        .comment_pr(PrRef { number: 7 }, &body)
        .await
        .unwrap();
    assert_eq!(note.id, "13");
}

#[tokio::test]
async fn comment_pr_creates_a_note_when_none_is_marked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("notes_page1.json")))
        .mount(&server)
        .await;
    let body = format!("{PR_COMMENT_MARKER}\nnew");
    Mock::given(method("POST"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .and(body_json(json!({ "body": body })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("note_created.json")))
        .expect(1)
        .mount(&server)
        .await;
    let note = host(&server)
        .comment_pr(PrRef { number: 7 }, &body)
        .await
        .unwrap();
    assert_eq!(note.id, "14");
}

#[tokio::test]
async fn system_notes_are_never_edited() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"id": 1, "body": format!("{PR_COMMENT_MARKER} quoted"), "system": true}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{PROJECT}/merge_requests/7/notes")))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("note_created.json")))
        .expect(1)
        .mount(&server)
        .await;
    host(&server)
        .comment_pr(PrRef { number: 7 }, PR_COMMENT_MARKER)
        .await
        .unwrap();
}

// ------------------------------------------------------------ webhooks

#[test]
fn verify_webhook_checks_the_token() {
    let host = GitlabCodeHost::new(
        GitlabConfig::new("http://unused", "acme/shop", "t").with_webhook_secret(SECRET),
    )
    .unwrap();
    let body = b"{}".to_vec();
    host.verify_webhook(&delivery(None, body.clone(), Some(SECRET)))
        .unwrap();
    assert_eq!(
        host.verify_webhook(&delivery(None, body.clone(), None)),
        Err(WebhookError::MissingSignature("X-Gitlab-Token"))
    );
    for bad in ["", "gitlab-hook-secreT", "gitlab-hook-secret ", "x"] {
        assert_eq!(
            host.verify_webhook(&delivery(None, body.clone(), Some(bad))),
            Err(WebhookError::InvalidSignature),
            "{bad:?}"
        );
    }
}

#[test]
fn verify_webhook_fails_closed_without_a_configured_secret() {
    let host = GitlabCodeHost::new(GitlabConfig::new("http://unused", "acme/shop", "t")).unwrap();
    assert_eq!(
        host.verify_webhook(&delivery(None, b"{}".to_vec(), Some(""))),
        Err(WebhookError::InvalidSignature)
    );
}

#[test]
fn parse_webhook_events() {
    let host = GitlabCodeHost::new(GitlabConfig::new("http://unused", "acme/shop", "t")).unwrap();
    assert_eq!(
        host.parse_webhook(&delivery(
            Some("Merge Request Hook"),
            fixture_bytes("webhook_merge_request_update.json"),
            None
        )),
        Ok(CodeHostEvent::PullRequest {
            action: PrAction::Synchronize,
            number: 7,
            title: "SHOP-456 checkout copy".into(),
            branch: "feature/SHOP-456-checkout".into(),
            head_sha: HEAD.into(),
            base_sha: String::new(),
        })
    );
    for (raw, expected) in [
        ("open", PrAction::Opened),
        ("reopen", PrAction::Reopened),
        ("close", PrAction::Closed),
        ("merge", PrAction::Merged),
    ] {
        let mut payload = fixture("webhook_merge_request_update.json");
        payload["object_attributes"]["action"] = json!(raw);
        match host.parse_webhook(&delivery(
            Some("Merge Request Hook"),
            serde_json::to_vec(&payload).unwrap(),
            None,
        )) {
            Ok(CodeHostEvent::PullRequest { action, .. }) => assert_eq!(action, expected, "{raw}"),
            other => panic!("{raw}: {other:?}"),
        }
    }
    assert!(matches!(
        host.parse_webhook(&delivery(
            Some("Merge Request Hook"),
            fixture_bytes("webhook_merge_request_approved.json"),
            None
        )),
        Ok(CodeHostEvent::Ignored { .. })
    ));
    assert!(matches!(
        host.parse_webhook(&delivery(Some("Push Hook"), b"{}".to_vec(), None)),
        Ok(CodeHostEvent::Ignored { .. })
    ));
    assert!(matches!(
        host.parse_webhook(&delivery(Some("Merge Request Hook"), b"[".to_vec(), None)),
        Err(WebhookError::Malformed(_))
    ));
    assert!(matches!(
        host.parse_webhook(&delivery(None, b"{}".to_vec(), None)),
        Err(WebhookError::Malformed(_))
    ));
}

#[test]
fn config_from_lookup_reads_the_environment_contract() {
    let config = GitlabConfig::from_lookup(|k| match k {
        "GITLAB_TOKEN" => Some("t".into()),
        "GITLAB_PROJECT" => Some("acme/shop".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(config.api_url(), "https://gitlab.com/api/v4");
    assert_eq!(config.project(), "acme/shop");
    assert!(!config.has_webhook_secret());

    // Inside GitLab CI the predefined variables are used.
    let ci = GitlabConfig::from_lookup(|k| match k {
        "GITLAB_TOKEN" => Some("t".into()),
        "CI_PROJECT_ID" => Some("42".into()),
        "CI_API_V4_URL" => Some("https://gitlab.acme.dev/api/v4".into()),
        "GUARD_GITLAB_WEBHOOK_SECRET" => Some("s".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(ci.api_url(), "https://gitlab.acme.dev/api/v4");
    assert_eq!(ci.project(), "42");
    assert!(ci.has_webhook_secret());

    assert!(matches!(
        GitlabConfig::from_lookup(|k| (k == "GITLAB_PROJECT").then(|| "x".to_string())),
        Err(CodeHostError::Unconfigured(_))
    ));
}

#[test]
fn debug_output_never_contains_secrets() {
    let config =
        GitlabConfig::new("http://x", "acme/shop", "glpat-secret").with_webhook_secret("hook");
    let debug = format!("{config:?}");
    assert!(!debug.contains("glpat-secret"), "{debug}");
    assert!(!debug.contains("\"hook\""), "{debug}");
}
