//! `whoami`: the token check of `guard connect github|gitlab` (cli.md).

use guard_codehost::whoami::{github_user, gitlab_user};
use guard_core::ports::CodeHostError;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn github_user_returns_the_login() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", "Bearer ghp_x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"login": "ada", "id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    let login = github_user(&format!("{}/", server.uri()), "ghp_x")
        .await
        .unwrap();
    assert_eq!(login, "ada");
}

#[tokio::test]
async fn gitlab_user_returns_the_username() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/user"))
        .and(header("private-token", "glpat-x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"username": "ada", "id": 1})))
        .expect(1)
        .mount(&server)
        .await;
    let user = gitlab_user(&format!("{}/api/v4", server.uri()), "glpat-x")
        .await
        .unwrap();
    assert_eq!(user, "ada");
}

#[tokio::test]
async fn a_rejected_token_is_unauthorized() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    assert_eq!(
        github_user(&server.uri(), "bad").await.unwrap_err(),
        CodeHostError::Unauthorized(401)
    );
    assert_eq!(
        gitlab_user(&server.uri(), "bad").await.unwrap_err(),
        CodeHostError::Unauthorized(401)
    );
}

#[tokio::test]
async fn a_response_without_the_name_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    assert!(matches!(
        github_user(&server.uri(), "t").await.unwrap_err(),
        CodeHostError::Rejected { .. }
    ));
}

#[tokio::test]
async fn the_token_never_follows_a_redirect_to_another_origin() {
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"username": "eve"})))
        .mount(&elsewhere)
        .await;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/user"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/api/v4/user", elsewhere.uri())),
        )
        .mount(&server)
        .await;
    let result = gitlab_user(&format!("{}/api/v4", server.uri()), "glpat-x").await;
    assert!(result.is_err(), "{result:?}");
    assert!(
        elsewhere.received_requests().await.unwrap().is_empty(),
        "the redirect target was never contacted"
    );
}
