//! GitLab `PullRequestPublisher` against wiremock with recorded response shapes.
//!
//! Endpoints and shapes follow the GitLab REST v4 docs:
//! - Merge requests: <https://docs.gitlab.com/api/merge_requests/> (list
//!   filters `state=opened` and `source_branch`; create
//!   `POST /projects/:id/merge_requests` returns 201, `labels` is a
//!   comma-separated string and unknown labels are created, 409 when another
//!   open MR exists for the source branch; update
//!   `PUT /projects/:id/merge_requests/:iid` takes `add_labels`; the
//!   response carries `draft`, deprecated `work_in_progress`).
//! - Drafts: <https://docs.gitlab.com/user/project/merge_requests/drafts/>
//!   (a `Draft:` title prefix marks the MR as a draft).
//! - Project `default_branch`: <https://docs.gitlab.com/api/projects/#get-a-single-project>.

use guard_codehost::publish::gitlab::{GitlabPublisher, GitlabPublisherConfig};
use guard_codehost::publish::render_body;
use guard_core::ports::code_host::{AgentPullRequest, PullRequestPublisher};
use guard_core::ports::{CodeHostError, CodeHostProvider, SealedSpec};
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TOKEN: &str = "glpat-push-token";
const MRS: &str = "/projects/acme%2Fshop/merge_requests";

fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/gitlab/publish/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn publisher(server: &MockServer, base: Option<&str>) -> GitlabPublisher {
    let mut config = GitlabPublisherConfig::new(server.uri(), "acme/shop", TOKEN)
        .with_labels(vec!["guard".into()]);
    if let Some(base) = base {
        config = config.with_base_branch(base);
    }
    GitlabPublisher::new(config).unwrap()
}

fn request() -> AgentPullRequest {
    let mut pr = AgentPullRequest::new(
        "SHOP-1",
        "SHOP-1: add cart totals",
        SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"sealed spec"),
    );
    pr.summary = "Adds totals to the cart page.".into();
    pr.base_commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into();
    pr.attempts = 2;
    pr.cost_usd = 0.5;
    pr.labels = vec!["lane-a".into()];
    pr
}

/// Never merge, never set auto-merge (`merge_when_pipeline_succeeds` lives
/// on the merge endpoint).
async fn forbid_merge(server: &MockServer) {
    Mock::given(path_regex(r"/merge_requests/\d+/merge$"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
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
async fn creates_a_draft_mr_on_the_default_branch_with_labels() {
    let server = MockServer::start().await;
    forbid_merge(&server).await;
    Mock::given(method("GET"))
        .and(path("/projects/acme%2Fshop"))
        .and(header("private-token", TOKEN))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("project.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(MRS))
        .and(query_param("state", "opened"))
        .and(query_param("source_branch", "agent/SHOP-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mrs_empty.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(MRS))
        .and(body_partial_json(json!({
            "source_branch": "agent/SHOP-1",
            "target_branch": "trunk",
            "title": "Draft: SHOP-1: add cart totals",
            "labels": "agent,lane-a,guard"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("mr_created.json")))
        .expect(1)
        .mount(&server)
        .await;

    let pr = request();
    let opened = publisher(&server, None)
        .open_pull_request(&pr)
        .await
        .unwrap();
    assert_eq!(opened.provider, CodeHostProvider::Gitlab);
    assert_eq!(opened.number, 9);
    assert_eq!(
        opened.url,
        "https://gitlab.example.com/acme/shop/-/merge_requests/9"
    );
    assert!(opened.created);
    assert!(opened.draft);
    assert_eq!(opened.labels, vec!["agent", "lane-a", "guard"]);

    let requests = server.received_requests().await.unwrap();
    let body = body_of(&requests, "POST", MRS);
    assert_eq!(body["description"].as_str().unwrap(), render_body(&pr));
    assert!(body.get("merge_when_pipeline_succeeds").is_none());
    let text = body["description"].as_str().unwrap();
    assert!(text.contains(&pr.sealed_spec.sha256));
    assert!(text.contains("| Attempts | 2 |"));
    assert!(text.contains("| Cost | $0.5000 |"));
}

#[tokio::test]
async fn an_open_mr_for_the_branch_is_updated_and_stays_a_draft() {
    let server = MockServer::start().await;
    forbid_merge(&server).await;
    Mock::given(method("GET"))
        .and(path(MRS))
        .and(query_param("source_branch", "agent/SHOP-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mrs_open.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(MRS))
        .respond_with(ResponseTemplate::new(201).set_body_json(fixture("mr_created.json")))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{MRS}/5")))
        .and(body_partial_json(json!({
            "title": "Draft: SHOP-1: add cart totals",
            "add_labels": "agent,lane-a,guard"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mr_updated.json")))
        .expect(1)
        .mount(&server)
        .await;

    // Even a non-draft request never un-drafts an existing draft MR.
    let mut pr = request();
    pr.draft = false;
    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&pr)
        .await
        .unwrap();
    assert_eq!(opened.number, 5);
    assert!(!opened.created);
    assert!(opened.draft);

    let requests = server.received_requests().await.unwrap();
    let put = body_of(&requests, "PUT", &format!("{MRS}/5"));
    assert_eq!(put["description"].as_str().unwrap(), render_body(&pr));
    assert!(put.get("state_event").is_none());
    assert!(put.get("target_branch").is_none());
}

#[tokio::test]
async fn a_conflict_on_create_falls_back_to_updating_the_existing_mr() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(MRS))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mrs_empty.json")))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(MRS))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mrs_open.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(MRS))
        .respond_with(ResponseTemplate::new(409).set_body_json(fixture("mr_already_exists.json")))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{MRS}/5")))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mr_updated.json")))
        .expect(1)
        .mount(&server)
        .await;
    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap();
    assert_eq!(opened.number, 5);
    assert!(!opened.created);
}

#[tokio::test]
async fn legacy_work_in_progress_is_read_and_a_non_draft_mr_keeps_its_title() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(MRS))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!([fixture("mr_legacy_no_draft.json")])),
        )
        .mount(&server)
        .await;
    let mut updated = fixture("mr_legacy_no_draft.json");
    updated["title"] = json!("SHOP-1: add cart totals");
    Mock::given(method("PUT"))
        .and(path(format!("{MRS}/6")))
        .and(body_partial_json(
            json!({"title": "SHOP-1: add cart totals"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(updated))
        .expect(1)
        .mount(&server)
        .await;
    let mut pr = request();
    pr.draft = false;
    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&pr)
        .await
        .unwrap();
    assert_eq!(opened.number, 6);
    assert!(!opened.draft);
}

#[tokio::test]
async fn errors_map_to_port_errors_without_the_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(MRS))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({"message": "403 Forbidden"})))
        .mount(&server)
        .await;
    let err = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap_err();
    assert_eq!(err, CodeHostError::Unauthorized(403));
    assert!(!err.to_string().contains(TOKEN));
}

#[test]
fn config_derives_the_push_url_from_the_project_path() {
    let env = |k: &str| match k {
        "GITLAB_TOKEN" => Some("glpat-x".to_string()),
        "GITLAB_PROJECT" => Some("acme/sub/shop".to_string()),
        "GITLAB_API_URL" => Some("https://gitlab.example.com/api/v4/".to_string()),
        _ => None,
    };
    let config = GitlabPublisherConfig::from_lookup(env).unwrap();
    assert_eq!(
        config.push_url(),
        Some("https://gitlab.example.com/acme/sub/shop.git")
    );
    assert!(!format!("{config:?}").contains("glpat-x"));

    // A numeric project id needs an explicit push URL.
    let numeric = |k: &str| match k {
        "GITLAB_PROJECT" => Some("1234".to_string()),
        other => env(other),
    };
    assert!(matches!(
        GitlabPublisherConfig::from_lookup(numeric),
        Err(CodeHostError::Unconfigured(m)) if m.contains("GUARD_PR_PUSH_URL")
    ));
    let explicit = |k: &str| match k {
        "GUARD_PR_PUSH_URL" => Some("https://gitlab.example.com/acme/shop.git".to_string()),
        other => numeric(other),
    };
    assert_eq!(
        GitlabPublisherConfig::from_lookup(explicit)
            .unwrap()
            .push_url(),
        Some("https://gitlab.example.com/acme/shop.git")
    );
}

/// Re-publishing force-pushed new commits: an MR a human un-drafted with
/// "merge when pipeline succeeds" set goes back to draft and loses the
/// auto-merge, so nothing merges unreviewed
/// (<https://docs.gitlab.com/api/merge_requests/#cancel-merge-when-pipeline-succeeds>).
#[tokio::test]
async fn republishing_onto_a_ready_mr_cancels_auto_merge_and_re_drafts_it() {
    let server = MockServer::start().await;
    forbid_merge(&server).await;
    let mut ready = fixture("mrs_open.json");
    ready[0]["title"] = json!("SHOP-1: old title");
    ready[0]["draft"] = json!(false);
    ready[0]["work_in_progress"] = json!(false);
    ready[0]["merge_when_pipeline_succeeds"] = json!(true);
    Mock::given(method("GET"))
        .and(path(MRS))
        .respond_with(ResponseTemplate::new(200).set_body_json(ready.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let mut cancelled = ready[0].clone();
    cancelled["merge_when_pipeline_succeeds"] = json!(false);
    Mock::given(method("POST"))
        .and(path(format!("{MRS}/5/cancel_merge_when_pipeline_succeeds")))
        .respond_with(ResponseTemplate::new(200).set_body_json(cancelled))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{MRS}/5")))
        .and(body_partial_json(
            json!({"title": "Draft: SHOP-1: add cart totals"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(fixture("mr_updated.json")))
        .expect(1)
        .mount(&server)
        .await;
    let opened = publisher(&server, Some("trunk"))
        .open_pull_request(&request())
        .await
        .unwrap();
    assert!(opened.draft);
    let requests = server.received_requests().await.unwrap();
    let order: Vec<String> = requests
        .iter()
        .filter(|r| r.method.as_str() != "GET")
        .map(|r| format!("{} {}", r.method, r.url.path()))
        .collect();
    assert_eq!(
        order,
        [
            format!("POST {MRS}/5/cancel_merge_when_pipeline_succeeds"),
            format!("PUT {MRS}/5")
        ]
    );
}
