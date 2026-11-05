//! GitHub adapter against a wiremock server and recorded fixtures.

use guard_codehost::PR_COMMENT_MARKER;
use guard_codehost::github::{GithubCodeHost, GithubConfig};
use guard_core::ports::{
    CheckState, CodeHost, CodeHostError, CodeHostEvent, CodeHostProvider, GateStatus, PrAction,
    PrRef, WebhookError, WebhookRequest,
};
use guard_core::ticket::Regex;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SECRET: &str = "It's a Secret to Everybody";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/github/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/github/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(path).unwrap()
}

fn host(server: &MockServer) -> GithubCodeHost {
    GithubCodeHost::new(
        GithubConfig::new(server.uri(), "acme/shop", "ghs_test_token").with_webhook_secret(SECRET),
    )
    .unwrap()
}

fn status(state: CheckState) -> GateStatus {
    GateStatus {
        head_sha: HEAD.into(),
        name: "guard".into(),
        state,
        title: "guard: green — 43 passed, 2 flaky".into(),
        summary: "### guard: green".into(),
        details_url: Some("https://ci.example/run/1".into()),
    }
}

fn sign(body: &[u8], secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn delivery(event: &str, body: Vec<u8>, signature: Option<String>) -> WebhookRequest {
    let mut req = WebhookRequest {
        body,
        ..WebhookRequest::default()
    };
    req.headers.insert("x-github-event".into(), event.into());
    if let Some(sig) = signature {
        req.headers.insert("x-hub-signature-256".into(), sig);
    }
    req
}

// ------------------------------------------------------------ pr_context

#[tokio::test]
async fn pr_context_reads_the_pull_and_paginates_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12"))
        .and(header("authorization", "Bearer ghs_test_token"))
        .and(header("accept", "application/vnd.github+json"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull.json")))
        .expect(1)
        .mount(&server)
        .await;
    let next = format!(
        "<{}/repos/acme/shop/pulls/12/files?per_page=100&page=2>; rel=\"next\", <{}/repos/acme/shop/pulls/12/files?per_page=100&page=2>; rel=\"last\"",
        server.uri(),
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12/files"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull_files_page2.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12/files"))
        .and(query_param("per_page", "100"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", next.as_str())
                .set_body_json(fixture("pull_files_page1.json")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let pattern = Regex::new(r"SHOP-\d+").unwrap();
    let ctx = host(&server)
        .pr_context(PrRef { number: 12 }, Some(&pattern))
        .await
        .unwrap();

    assert_eq!(ctx.provider, CodeHostProvider::Github);
    assert_eq!(ctx.repo, "acme/shop");
    assert_eq!(ctx.number, 12);
    assert_eq!(ctx.title, "Fix cart totals for SHOP-999");
    assert_eq!(ctx.url, "https://github.com/acme/shop/pull/12");
    assert_eq!(ctx.author, "dev");
    // The branch wins over the title.
    assert_eq!(ctx.ticket_key.as_deref(), Some("SHOP-123"));
    assert_eq!(ctx.branch, "SHOP-123-fix-cart");
    assert_eq!(ctx.base_branch, "main");
    assert_eq!(ctx.head_sha, HEAD);
    assert_eq!(ctx.base_sha, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    // Sorted; renames contribute both paths; removed files are included.
    assert_eq!(
        ctx.changed_files,
        vec![
            "README.md",
            "src/cart/new-name.ts",
            "src/cart/old-name.ts",
            "src/cart/totals.ts"
        ]
    );
}

#[tokio::test]
async fn pr_context_without_pattern_has_no_ticket() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12/files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    let ctx = host(&server)
        .pr_context(PrRef { number: 12 }, None)
        .await
        .unwrap();
    assert_eq!(ctx.ticket_key, None);
    assert!(ctx.changed_files.is_empty());
}

#[tokio::test]
async fn pagination_never_follows_a_link_to_another_host() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull.json")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls/12/files"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", "<https://evil.example/steal?page=2>; rel=\"next\"")
                .set_body_json(fixture("pull_files_page1.json")),
        )
        .mount(&server)
        .await;
    let err = host(&server)
        .pr_context(PrRef { number: 12 }, None)
        .await
        .unwrap_err();
    assert!(matches!(err, CodeHostError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn http_errors_map_to_code_host_errors() {
    let cases: Vec<(ResponseTemplate, CodeHostError)> = vec![
        (ResponseTemplate::new(404), CodeHostError::NotFound(12)),
        (ResponseTemplate::new(401), CodeHostError::Unauthorized(401)),
        (ResponseTemplate::new(403), CodeHostError::Unauthorized(403)),
        (
            ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0"),
            CodeHostError::RateLimited,
        ),
        (ResponseTemplate::new(429), CodeHostError::RateLimited),
        (
            ResponseTemplate::new(500).set_body_string("boom"),
            CodeHostError::Rejected {
                status: 500,
                body: "boom".into(),
            },
        ),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/shop/pulls/12"))
            .respond_with(response)
            .mount(&server)
            .await;
        let err = host(&server)
            .pr_context(PrRef { number: 12 }, None)
            .await
            .unwrap_err();
        assert_eq!(err, expected);
    }
}

// ------------------------------------------------------------ publish_status

#[tokio::test]
async fn publish_status_creates_a_completed_check_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/acme/shop/commits/{HEAD}/check-runs")))
        .and(query_param("check_name", "guard"))
        .and(query_param("filter", "latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("check_runs_empty.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/check-runs"))
        .and(body_json(json!({
            "name": "guard",
            "head_sha": HEAD,
            "status": "completed",
            "conclusion": "success",
            "details_url": "https://ci.example/run/1",
            "output": {"title": "guard: green — 43 passed, 2 flaky", "summary": "### guard: green"}
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("check_run.json")))
        .expect(1)
        .mount(&server)
        .await;
    host(&server)
        .publish_status(&status(CheckState::Success))
        .await
        .unwrap();
}

#[tokio::test]
async fn publish_status_updates_the_latest_check_run_with_that_name() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/repos/acme/shop/commits/{HEAD}/check-runs")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("check_runs_existing.json")))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/shop/check-runs/4242"))
        .and(body_json(json!({
            "name": "guard",
            "status": "completed",
            "conclusion": "failure",
            "details_url": "https://ci.example/run/1",
            "output": {"title": "guard: green — 43 passed, 2 flaky", "summary": "### guard: green"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("check_run.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/check-runs"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;
    // Infrastructure error is reported as a failed conclusion.
    host(&server)
        .publish_status(&status(CheckState::Error))
        .await
        .unwrap();
}

#[tokio::test]
async fn publish_status_maps_pending_and_running_without_conclusion() {
    for (state, expected) in [
        (CheckState::Pending, "queued"),
        (CheckState::Running, "in_progress"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/shop/commits/{HEAD}/check-runs")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fixture("check_runs_empty.json")),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/repos/acme/shop/check-runs"))
            .and(body_json(json!({
                "name": "guard",
                "head_sha": HEAD,
                "status": expected,
                "details_url": "https://ci.example/run/1",
                "output": {"title": "guard: green — 43 passed, 2 flaky", "summary": "### guard: green"}
            })))
            .respond_with(ResponseTemplate::new(201).set_body_json(fixture("check_run.json")))
            .expect(1)
            .mount(&server)
            .await;
        host(&server).publish_status(&status(state)).await.unwrap();
    }
}

// ------------------------------------------------------------ comment_pr

#[tokio::test]
async fn comment_pr_edits_the_marked_comment_found_on_a_later_page() {
    let server = MockServer::start().await;
    let next = format!(
        "<{}/repos/acme/shop/issues/12/comments?per_page=100&page=2>; rel=\"next\"",
        server.uri()
    );
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("issue_comments_page2.json")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .and(query_param("page", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", next.as_str())
                .set_body_json(fixture("issue_comments_page1.json")),
        )
        .mount(&server)
        .await;
    let body = format!("{PR_COMMENT_MARKER}\nnew");
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/shop/issues/comments/202"))
        .and(body_json(json!({ "body": body })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("issue_comment_updated.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .respond_with(ResponseTemplate::new(201))
        .expect(0)
        .mount(&server)
        .await;

    let comment = host(&server)
        .comment_pr(PrRef { number: 12 }, &body)
        .await
        .unwrap();
    assert_eq!(comment.id, "202");
    assert_eq!(
        comment.url.as_deref(),
        Some("https://github.com/acme/shop/pull/12#issuecomment-202")
    );
}

#[tokio::test]
async fn comment_pr_creates_a_comment_when_none_is_marked() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("issue_comments_page1.json")),
        )
        .mount(&server)
        .await;
    let body = format!("{PR_COMMENT_MARKER}\nnew");
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .and(body_json(json!({ "body": body })))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(fixture("issue_comment_created.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    let comment = host(&server)
        .comment_pr(PrRef { number: 12 }, &body)
        .await
        .unwrap();
    assert_eq!(comment.id, "303");
}

#[tokio::test]
async fn comment_pr_prepends_the_marker_when_the_body_lacks_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues/12/comments"))
        .and(body_json(
            json!({ "body": format!("{PR_COMMENT_MARKER}\n### guard: green") }),
        ))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(fixture("issue_comment_created.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    host(&server)
        .comment_pr(PrRef { number: 12 }, "### guard: green")
        .await
        .unwrap();
}

#[tokio::test]
async fn comment_pr_on_a_missing_pr_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/issues/99/comments"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let err = host(&server)
        .comment_pr(PrRef { number: 99 }, PR_COMMENT_MARKER)
        .await
        .unwrap_err();
    assert_eq!(err, CodeHostError::NotFound(99));
}

// ------------------------------------------------------------ webhooks

#[test]
fn verify_webhook_accepts_a_valid_signature() {
    let host = GithubCodeHost::new(
        GithubConfig::new("http://unused", "acme/shop", "t").with_webhook_secret(SECRET),
    )
    .unwrap();
    // Example from GitHub's "Validating webhook deliveries" docs.
    let body = b"Hello, World!".to_vec();
    let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
    assert_eq!(sign(&body, SECRET), sig);
    host.verify_webhook(&delivery("ping", body, Some(sig.into())))
        .unwrap();
}

#[test]
fn verify_webhook_rejects_bad_signatures() {
    let host = GithubCodeHost::new(
        GithubConfig::new("http://unused", "acme/shop", "t").with_webhook_secret(SECRET),
    )
    .unwrap();
    let body = b"{\"zen\":\"x\"}".to_vec();

    assert_eq!(
        host.verify_webhook(&delivery("ping", body.clone(), None)),
        Err(WebhookError::MissingSignature("X-Hub-Signature-256"))
    );
    for bad in [
        sign(&body, "wrong secret"),
        sign(b"tampered", SECRET),
        "sha1=0123".into(),
        "sha256=not-hex".into(),
        "sha256=".into(),
        sign(&body, SECRET).to_uppercase(),
    ] {
        assert_eq!(
            host.verify_webhook(&delivery("ping", body.clone(), Some(bad.clone()))),
            Err(WebhookError::InvalidSignature),
            "{bad}"
        );
    }
}

#[test]
fn verify_webhook_fails_closed_without_a_configured_secret() {
    let host = GithubCodeHost::new(GithubConfig::new("http://unused", "acme/shop", "t")).unwrap();
    let body = b"{}".to_vec();
    let sig = sign(&body, "");
    assert_eq!(
        host.verify_webhook(&delivery("ping", body, Some(sig))),
        Err(WebhookError::InvalidSignature)
    );
}

#[test]
fn parse_webhook_events() {
    let host = GithubCodeHost::new(GithubConfig::new("http://unused", "acme/shop", "t")).unwrap();

    assert_eq!(
        host.parse_webhook(&delivery(
            "ping",
            b"{\"zen\":\"Keep it simple\"}".to_vec(),
            None
        )),
        Ok(CodeHostEvent::Ping)
    );
    assert_eq!(
        host.parse_webhook(&delivery(
            "pull_request",
            fixture_bytes("webhook_pull_request_synchronize.json"),
            None
        )),
        Ok(CodeHostEvent::PullRequest {
            action: PrAction::Synchronize,
            number: 12,
            title: "Fix cart totals".into(),
            branch: "SHOP-123-fix-cart".into(),
            head_sha: HEAD.into(),
            base_sha: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
        })
    );
    match host.parse_webhook(&delivery(
        "pull_request",
        fixture_bytes("webhook_pull_request_closed_merged.json"),
        None,
    )) {
        Ok(CodeHostEvent::PullRequest { action, .. }) => assert_eq!(action, PrAction::Merged),
        other => panic!("{other:?}"),
    }
    let mut labeled: Value = fixture("webhook_pull_request_synchronize.json");
    labeled["action"] = json!("labeled");
    assert!(matches!(
        host.parse_webhook(&delivery(
            "pull_request",
            serde_json::to_vec(&labeled).unwrap(),
            None
        )),
        Ok(CodeHostEvent::Ignored { .. })
    ));
    assert!(matches!(
        host.parse_webhook(&delivery("push", b"{}".to_vec(), None)),
        Ok(CodeHostEvent::Ignored { .. })
    ));
    assert!(matches!(
        host.parse_webhook(&delivery("pull_request", b"not json".to_vec(), None)),
        Err(WebhookError::Malformed(_))
    ));
    let mut no_event = delivery("ping", b"{}".to_vec(), None);
    no_event.headers.clear();
    assert!(matches!(
        host.parse_webhook(&no_event),
        Err(WebhookError::Malformed(_))
    ));
}

#[test]
fn config_from_lookup_reads_the_environment_contract() {
    let env = |key: &str| match key {
        "GITHUB_TOKEN" => Some("tok".to_string()),
        "GITHUB_REPOSITORY" => Some("acme/shop".to_string()),
        "GUARD_GITHUB_WEBHOOK_SECRET" => Some("s".to_string()),
        _ => None,
    };
    let config = GithubConfig::from_lookup(env).unwrap();
    assert_eq!(config.api_url(), "https://api.github.com");
    assert_eq!(config.repo(), "acme/shop");
    assert!(config.has_webhook_secret());

    let missing = GithubConfig::from_lookup(|k| (k == "GITHUB_TOKEN").then(|| "t".to_string()));
    assert!(matches!(missing, Err(CodeHostError::Unconfigured(_))));

    let bad_repo = GithubConfig::from_lookup(|k| match k {
        "GITHUB_TOKEN" => Some("t".into()),
        "GITHUB_REPOSITORY" => Some("no-slash".into()),
        _ => None,
    });
    assert!(matches!(bad_repo, Err(CodeHostError::Unconfigured(_))));

    let custom = GithubConfig::from_lookup(|k| match k {
        "GITHUB_TOKEN" => Some("t".into()),
        "GITHUB_REPOSITORY" => Some("acme/shop".into()),
        "GITHUB_API_URL" => Some("https://ghe.example/api/v3/".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(custom.api_url(), "https://ghe.example/api/v3");
}

#[test]
fn debug_output_never_contains_secrets() {
    let config = GithubConfig::new("http://x", "acme/shop", "ghs_supersecret")
        .with_webhook_secret("hook-secret");
    let debug = format!("{config:?}");
    assert!(!debug.contains("ghs_supersecret"), "{debug}");
    assert!(!debug.contains("hook-secret"), "{debug}");
}
