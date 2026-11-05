//! GitHub `PullRequestPublisher` against wiremock with recorded response shapes.
//!
//! Endpoints and shapes follow the GitHub REST docs (API version 2022-11-28):
//! - List / create / update pull requests:
//!   <https://docs.github.com/en/rest/pulls/pulls> (`head` filter is
//!   `user:ref-name`; create returns 201, 422 on validation failure such as
//!   "A pull request already exists"; update is `PATCH` and has no `draft`).
//! - Labels: <https://docs.github.com/en/rest/issues/labels> (create a label
//!   returns 201, 422 `already_exists`; add labels to an issue is
//!   `POST /issues/{n}/labels` with `{"labels": [...]}`, 200).
//! - Repository `default_branch`: <https://docs.github.com/en/rest/repos/repos#get-a-repository>.

use guard_codehost::publish::github::{GithubPublisher, GithubPublisherConfig};
use guard_codehost::publish::render_body;
use guard_core::ports::code_host::{AgentPullRequest, PullRequestPublisher};
use guard_core::ports::{CodeHostError, CodeHostProvider, SealedSpec};
use serde_json::{Value, json};
use wiremock::matchers::{
    body_json, body_partial_json, header, method, path, path_regex, query_param,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "ghs_push_token";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/github/publish/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn publisher(server: &MockServer, base: Option<&str>) -> GithubPublisher {
    let mut config = GithubPublisherConfig::new(server.uri(), "acme/shop", TOKEN)
        .with_labels(vec!["guard".into()]);
    if let Some(base) = base {
        config = config.with_base_branch(base);
    }
    GithubPublisher::new(config).unwrap()
}

fn request() -> AgentPullRequest {
    let mut pr = AgentPullRequest::new(
        "SHOP-1",
        "SHOP-1: add cart totals",
        SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"sealed spec"),
    );
    pr.summary = "Adds totals to the cart page.".into();
    pr.base_commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    pr.attempts = 3;
    pr.cost_usd = 1.234_56;
    pr.coder = Some("coder (claude-code, sonnet)".into());
    pr.labels = vec!["lane-a".into()];
    pr
}

/// Never merge, never enable auto-merge (REST merge endpoint and the
/// GraphQL `enablePullRequestAutoMerge` mutation).
async fn forbid_merge(server: &MockServer) {
    Mock::given(path_regex(r"^/repos/acme/shop/pulls/\d+/merge$"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
    Mock::given(path("/graphql"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(server)
        .await;
}

async fn mount_labels(server: &MockServer, number: u64) {
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/labels"))
        .and(body_partial_json(json!({"name": "agent"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("label_created.json")))
        .expect(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/labels"))
        .respond_with(
            ResponseTemplate::new(422).set_body_json(fixture("label_already_exists.json")),
        )
        .expect(2)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/repos/acme/shop/issues/{number}/labels")))
        .and(body_json(json!({"labels": ["agent", "lane-a", "guard"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("issue_labels.json")))
        .expect(1)
        .mount(server)
        .await;
}

fn body_of(requests: &[Request], verb: &str, url_path: &str) -> Value {
    let r = requests
        .iter()
        .find(|r| r.method.as_str() == verb && r.url.path() == url_path)
        .unwrap_or_else(|| panic!("no {verb} {url_path}"));
    serde_json::from_slice(&r.body).unwrap()
}

#[tokio::test]
async fn creates_a_draft_pr_on_the_default_branch_with_labels() {
    let server = MockServer::start().await;
    forbid_merge(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop"))
        .and(header("authorization", "Bearer ghs_push_token"))
        .and(header("x-github-api-version", "2022-11-28"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("repo.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("head", "acme:agent/SHOP-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pulls_empty.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/pulls"))
        .and(body_partial_json(json!({
            "title": "SHOP-1: add cart totals",
            "head": "agent/SHOP-1",
            "base": "trunk",
            "draft": true,
            "maintainer_can_modify": false
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("pull_created.json")))
        .expect(1)
        .mount(&server)
        .await;
    mount_labels(&server, 42).await;

    let pr = request();
    let opened = publisher(&server, None)
        .open_pull_request(&pr)
        .await
        .unwrap();

    assert_eq!(opened.provider, CodeHostProvider::Github);
    assert_eq!(opened.number, 42);
    assert_eq!(opened.url, "https://github.com/acme/shop/pull/42");
    assert!(opened.created);
    assert!(opened.draft);
    assert_eq!(opened.labels, vec!["agent", "lane-a", "guard"]);

    let requests = server.received_requests().await.unwrap();
    let body = body_of(&requests, "POST", "/repos/acme/shop/pulls");
    assert_eq!(body["body"].as_str().unwrap(), render_body(&pr));
    let text = body["body"].as_str().unwrap();
    assert!(text.contains("SHOP-1"));
    assert!(text.contains(&pr.sealed_spec.sha256));
    assert!(text.contains("| Attempts | 3 |"));
    assert!(text.contains("| Cost | $1.2346 |"));
    assert!(text.contains("never merges"));
    // No merge-related fields are ever sent.
    assert!(body.get("auto_merge").is_none());
}

#[tokio::test]
async fn an_open_pr_for_the_branch_is_updated_not_duplicated() {
    let server = MockServer::start().await;
    forbid_merge(&server).await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .and(query_param("head", "acme:agent/SHOP-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pulls_open.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("pull_created.json")))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/shop/pulls/7"))
        .and(body_partial_json(
            json!({"title": "SHOP-1: add cart totals"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull_updated.json")))
        .expect(1)
        .mount(&server)
        .await;
    mount_labels(&server, 7).await;

    let pr = request();
    // A configured base branch skips the repository lookup (no GET mounted).
    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&pr)
        .await
        .unwrap();
    assert_eq!(opened.number, 7);
    assert!(!opened.created);
    assert!(opened.draft);
    assert_eq!(opened.url, "https://github.com/acme/shop/pull/7");

    let requests = server.received_requests().await.unwrap();
    let patch = body_of(&requests, "PATCH", "/repos/acme/shop/pulls/7");
    assert_eq!(patch["body"].as_str().unwrap(), render_body(&pr));
    // PATCH never changes state, base or draft.
    assert!(patch.get("state").is_none());
    assert!(patch.get("draft").is_none());
}

#[tokio::test]
async fn a_concurrent_create_falls_back_to_updating_the_existing_pr() {
    let server = MockServer::start().await;
    forbid_merge(&server).await;
    // First listing sees nothing, the second (after the 422) sees the PR.
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pulls_empty.json")))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pulls_open.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(422).set_body_json(fixture("pull_already_exists.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/shop/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull_updated.json")))
        .expect(1)
        .mount(&server)
        .await;
    mount_labels(&server, 7).await;

    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap();
    assert_eq!(opened.number, 7);
    assert!(!opened.created);
}

#[tokio::test]
async fn a_non_draft_request_is_honoured_on_create() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pulls_empty.json")))
        .mount(&server)
        .await;
    let mut created = fixture("pull_created.json");
    created["draft"] = json!(false);
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/pulls"))
        .and(body_partial_json(json!({"draft": false})))
        .respond_with(ResponseTemplate::new(201).set_body_json(created))
        .expect(1)
        .mount(&server)
        .await;
    mount_labels(&server, 42).await;
    let mut pr = request();
    pr.draft = false;
    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&pr)
        .await
        .unwrap();
    assert!(!opened.draft);
}

#[tokio::test]
async fn refused_credentials_and_other_rejections_map_to_port_errors() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({"message": "Bad credentials"})),
        )
        .mount(&server)
        .await;
    let err = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap_err();
    assert_eq!(err, CodeHostError::Unauthorized(401));
    assert!(!err.to_string().contains(TOKEN));

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pulls_empty.json")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(422).set_body_json(json!({
            "message": "Validation Failed",
            "errors": [{"resource": "PullRequest", "field": "base", "code": "invalid"}]
        })))
        .mount(&server)
        .await;
    let err = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap_err();
    assert!(
        matches!(err, CodeHostError::Rejected { status: 422, .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn invalid_requests_are_rejected_before_any_request() {
    let server = MockServer::start().await;
    let mut pr = request();
    pr.ticket = "../evil".into();
    let err = publisher(&server, Some("trunk"))
        .open_pull_request(&pr)
        .await
        .unwrap_err();
    assert!(matches!(err, CodeHostError::Rejected { status: 0, .. }));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn config_reads_the_token_from_the_named_env_var_and_derives_the_push_url() {
    let env = |k: &str| match k {
        "GUARD_PR_TOKEN_ENV" => Some("BOT_PUSH_TOKEN".to_string()),
        "BOT_PUSH_TOKEN" => Some("ghp_x".to_string()),
        "GITHUB_REPOSITORY" => Some("acme/shop".to_string()),
        "GUARD_PR_LABELS" => Some("lane-a, guard ,".to_string()),
        "GUARD_PR_BASE_BRANCH" => Some("release".to_string()),
        _ => None,
    };
    let config = GithubPublisherConfig::from_lookup(env).unwrap();
    assert_eq!(config.api_url(), "https://api.github.com");
    assert_eq!(config.push_url(), Some("https://github.com/acme/shop.git"));
    assert_eq!(config.base_branch(), Some("release"));
    assert_eq!(config.labels(), ["lane-a", "guard"]);
    assert!(
        !format!("{config:?}").contains("ghp_x"),
        "Debug redacts the token"
    );

    // GHES: the web host is the API URL without /api/v3.
    let ghes = GithubPublisherConfig::new("https://ghe.example/api/v3/", "acme/shop", "t");
    assert_eq!(ghes.push_url(), Some("https://ghe.example/acme/shop.git"));

    // The default token variable is GITHUB_TOKEN.
    let missing = GithubPublisherConfig::from_lookup(|k| match k {
        "GITHUB_REPOSITORY" => Some("acme/shop".into()),
        _ => None,
    })
    .unwrap_err();
    assert!(matches!(missing, CodeHostError::Unconfigured(m) if m.contains("GITHUB_TOKEN")));

    // An unknown API host needs an explicit push URL.
    let custom = |k: &str| match k {
        "GITHUB_TOKEN" => Some("t".to_string()),
        "GITHUB_REPOSITORY" => Some("acme/shop".to_string()),
        "GITHUB_API_URL" => Some("http://127.0.0.1:9".to_string()),
        _ => None,
    };
    assert!(matches!(
        GithubPublisherConfig::from_lookup(custom),
        Err(CodeHostError::Unconfigured(m)) if m.contains("GUARD_PR_PUSH_URL")
    ));
    // A cleartext push URL is refused (the token would travel in clear).
    let cleartext = |k: &str| match k {
        "GUARD_PR_PUSH_URL" => Some("http://git.example/acme/shop.git".to_string()),
        other => custom(other),
    };
    assert!(GithubPublisherConfig::from_lookup(cleartext).is_err());
    // An invalid token variable name is reported as such.
    let bad = |k: &str| match k {
        "GUARD_PR_TOKEN_ENV" => Some("not a name".to_string()),
        other => custom(other),
    };
    assert!(matches!(
        GithubPublisherConfig::from_lookup(bad),
        Err(CodeHostError::Unconfigured(m)) if m.contains("GUARD_PR_TOKEN_ENV")
    ));
}

/// Re-publishing force-pushed new commits onto `agent/<KEY>`: an open PR a
/// human already marked ready, with auto-merge on, must not merge them
/// unreviewed. GraphQL docs:
/// <https://docs.github.com/en/graphql/reference/mutations#disablepullrequestautomerge>,
/// <https://docs.github.com/en/graphql/reference/mutations#convertpullrequesttodraft>
/// (both take the PR `node_id` as `pullRequestId`).
#[tokio::test]
async fn republishing_onto_a_ready_pr_disables_auto_merge_and_re_drafts_it() {
    use wiremock::matchers::body_string_contains;
    let server = MockServer::start().await;
    Mock::given(path_regex(r"^/repos/acme/shop/pulls/\d+/merge$"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("pulls_open_ready_auto_merge.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("disablePullRequestAutoMerge"))
        .and(body_partial_json(
            json!({"variables": {"id": "PR_kwDOAbCdEf4AAAAH"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("graphql_auto_merge_disabled.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("convertPullRequestToDraft"))
        .and(body_partial_json(
            json!({"variables": {"id": "PR_kwDOAbCdEf4AAAAH"}}),
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(fixture("graphql_converted_to_draft.json")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/acme/shop/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull_updated.json")))
        .expect(1)
        .mount(&server)
        .await;
    mount_labels(&server, 7).await;

    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap();
    assert!(!opened.created);
    assert!(
        opened.draft,
        "back to draft: a human has to mark it ready again"
    );

    let requests = server.received_requests().await.unwrap();
    let order: Vec<String> = requests
        .iter()
        .filter(|r| r.method.as_str() != "GET")
        .map(|r| {
            let body = String::from_utf8_lossy(&r.body).to_string();
            assert!(!body.contains("enablePullRequestAutoMerge"), "{body}");
            assert!(!body.contains("mergePullRequest"), "{body}");
            if body.contains("disablePullRequestAutoMerge") {
                "disable".into()
            } else if body.contains("convertPullRequestToDraft") {
                "draft".into()
            } else {
                format!("{} {}", r.method, r.url.path())
            }
        })
        .take(3)
        .collect();
    // Both safety steps run before the title/body update.
    assert_eq!(
        order,
        ["disable", "draft", "PATCH /repos/acme/shop/pulls/7"]
    );
}

#[tokio::test]
async fn a_failed_re_draft_fails_the_publish() {
    use wiremock::matchers::body_string_contains;
    let server = MockServer::start().await;
    let mut ready = fixture("pulls_open_ready_auto_merge.json");
    ready[0]["auto_merge"] = Value::Null;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ready))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("disablePullRequestAutoMerge"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("graphql_error.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("pull_updated.json")))
        .expect(0)
        .mount(&server)
        .await;
    let err = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("Resource not accessible by integration"),
        "{err}"
    );
}
