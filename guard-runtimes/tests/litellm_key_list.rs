//! `LiteLlmGateway::list_keys` against wiremock, shaped after the LiteLLM
//! proxy source (BerriAI/litellm main, `key_management_endpoints.py`, read
//! 2026-11-01):
//! - `GET /key/list` query `page` (from 1), `size` (`le=100`, default 10),
//!   `return_full_object` (default false: `keys` is then a list of hashed
//!   tokens only), `key_alias` (an **exact** match unless
//!   `substring_matching=true`, a recent addition).
//! - Response `KeyListResponseObject`: `{keys, total_count, current_page,
//!   total_pages}`; with `return_full_object=true` each key is a
//!   `UserAPIKeyAuth` (`token` = the hashed id `/key/delete` accepts,
//!   `key_alias`, `created_at`, `expires`, `metadata`).
//!
//! The prefix filter is applied client-side (no server-side alias filter),
//! so proxies without `substring_matching` still list every candidate.

use guard_core::ports::model_gateway::ModelGateway;
use guard_core::ports::secret::Secret;
use guard_runtimes::gateway::LiteLlmGateway;
use serde_json::json;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn key(token: &str, alias: Option<&str>, run_id: &str) -> serde_json::Value {
    json!({
        "token": token,
        "key_alias": alias,
        "key_name": "sk-...abcd",
        "created_at": "2026-11-01T10:00:00.123000Z",
        "expires": "2026-11-01T12:00:00+00:00",
        "metadata": {"run_id": run_id, "role": "coder", "attempt": 1},
        "models": ["anthropic/claude-sonnet-4-5"],
        "max_budget": 5.0,
        "blocked": null
    })
}

#[tokio::test]
async fn list_keys_pages_full_objects_and_filters_by_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/key/list"))
        .and(header("authorization", "Bearer sk-master-test"))
        .and(query_param("return_full_object", "true"))
        .and(query_param("size", "100"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": [
                key("hash-1", Some("a17-shop-1:coder:1"), "a17-shop-1"),
                key("hash-2", Some("exp:memory:SHOP-1:r1"), "exp-memory-shop-1-r1"),
                key("hash-3", None, "x"),
            ],
            "total_count": 4,
            "current_page": 1,
            "total_pages": 2
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/key/list"))
        .and(query_param("page", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": [key("hash-4", Some("a17-shop-1:qa:1"), "a17-shop-1")],
            "total_count": 4,
            "current_page": 2,
            "total_pages": 2
        })))
        .expect(1)
        .mount(&server)
        .await;

    let gw = LiteLlmGateway::new(&server.uri(), Secret::new("sk-master-test")).unwrap();
    let keys = gw.list_keys("a17-").await.unwrap();
    let got: Vec<_> = keys
        .iter()
        .map(|k| (k.key_id.as_str(), k.alias.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("hash-1", Some("a17-shop-1:coder:1")),
            ("hash-4", Some("a17-shop-1:qa:1"))
        ]
    );
    assert_eq!(
        keys[0].created_at.as_deref(),
        Some("2026-11-01T10:00:00.123000Z")
    );
    assert_eq!(keys[0].expires_at.as_deref(), Some("2026-11-01T12:00:00Z"));
    assert_eq!(keys[0].metadata["run_id"], "a17-shop-1");
    assert_eq!(keys[0].metadata["attempt"], "1");
}

#[tokio::test]
async fn list_keys_reports_a_rejection_and_a_token_only_listing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/key/list"))
        .respond_with(ResponseTemplate::new(403).set_body_string("not allowed"))
        .mount(&server)
        .await;
    let gw = LiteLlmGateway::new(&server.uri(), Secret::new("sk-master-test")).unwrap();
    let err = gw.list_keys("a").await.unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
    assert!(!err.to_string().contains("sk-master-test"));

    // A proxy that ignores return_full_object answers bare token strings: no
    // alias to match, so nothing is listed (never a guess).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/key/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": ["hash-1", "hash-2"],
            "total_count": 2,
            "current_page": 1,
            "total_pages": 1
        })))
        .mount(&server)
        .await;
    let gw = LiteLlmGateway::new(&server.uri(), Secret::new("sk-master-test")).unwrap();
    assert!(gw.list_keys("a").await.unwrap().is_empty());
}
