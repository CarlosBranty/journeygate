//! `LiteLlmGateway` against wiremock, shaped after the LiteLLM proxy source
//! (BerriAI/litellm main @ 60fc121, 2026-11-01):
//! - `/key/generate`: `GenerateKeyResponse` in `litellm/proxy/_types.py`
//!   (`key`, `token`, `token_id`, `key_alias`, `expires`, ...).
//! - `/key/delete`: body `{"keys": [...]}`, answers `{"deleted_keys": [...]}`,
//!   404 for an unknown key (`key_management_endpoints.py`).
//! - `/spend/logs/v2`: query `key_alias`, `start_date`, `end_date`, `page`,
//!   `page_size` (`le=1000`); response `{data, total, page, page_size, total_pages}`
//!   (`spend_management_endpoints.py::ui_view_spend_logs`). Rows follow
//!   `SpendLogsPayload`; `metadata.user_api_key_alias`, `metadata.usage_object`.
//!   The `key_alias` filter is a substring match on the server, so the client
//!   filters rows to the exact alias.

use guard_core::ports::model_gateway::{GatewayError, KeyRequest, ModelGateway, SpendQuery};
use guard_core::ports::secret::Secret;
use guard_runtimes::gateway::LiteLlmGateway;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[path = "runtimes/common.rs"]
mod common;

fn request() -> KeyRequest {
    let mut metadata = BTreeMap::new();
    metadata.insert("run_id".to_string(), "run-1".to_string());
    metadata.insert("role".to_string(), "coder".to_string());
    metadata.insert("arm".to_string(), "baseline".to_string());
    KeyRequest {
        alias: "run-1:coder:1".into(),
        models: vec!["anthropic/claude-sonnet-4-5".into()],
        max_budget_usd: 5.0,
        rpm_limit: Some(60),
        tpm_limit: Some(400_000),
        max_parallel_requests: Some(2),
        duration: Duration::from_secs(7200),
        metadata,
    }
}

fn gateway(server: &MockServer) -> LiteLlmGateway {
    LiteLlmGateway::new(&server.uri(), Secret::new("sk-master-test")).unwrap()
}

#[tokio::test]
async fn mint_key_sends_the_documented_body_and_keeps_the_token_secret() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/key/generate"))
        .and(header("authorization", "Bearer sk-master-test"))
        .and(body_json(json!({
            "key_alias": "run-1:coder:1",
            "models": ["anthropic/claude-sonnet-4-5"],
            "max_budget": 5.0,
            "rpm_limit": 60,
            "tpm_limit": 400000,
            "max_parallel_requests": 2,
            "duration": "7200s",
            "metadata": {"arm": "baseline", "role": "coder", "run_id": "run-1"}
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key": "sk-virtual-123",
            "key_name": "sk-...-123",
            "key_alias": "run-1:coder:1",
            "token": "hashed-token-abc",
            "token_id": "hashed-token-abc",
            "expires": "2026-11-01T12:00:00Z",
            "models": ["anthropic/claude-sonnet-4-5"],
            "max_budget": 5.0
        })))
        .expect(1)
        .mount(&server)
        .await;

    let key = gateway(&server).mint_key(&request()).await.unwrap();
    assert_eq!(key.key_id, "hashed-token-abc");
    assert_eq!(key.alias, "run-1:coder:1");
    assert_eq!(key.token.expose(), "sk-virtual-123");
    assert_eq!(key.expires_at.as_deref(), Some("2026-11-01T12:00:00Z"));
    assert_eq!(key.max_budget_usd, 5.0);
    assert!(!format!("{key:?}").contains("sk-virtual-123"));
}

#[tokio::test]
async fn mint_key_omits_unset_limits() {
    let server = MockServer::start().await;
    let mut r = request();
    r.rpm_limit = None;
    r.tpm_limit = None;
    r.max_parallel_requests = None;
    r.metadata.clear();
    Mock::given(method("POST"))
        .and(path("/key/generate"))
        .and(body_json(json!({
            "key_alias": "run-1:coder:1",
            "models": ["anthropic/claude-sonnet-4-5"],
            "max_budget": 5.0,
            "duration": "7200s",
            "metadata": {}
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"key": "sk-v", "token": "h1", "key_alias": "run-1:coder:1"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let key = gateway(&server).mint_key(&r).await.unwrap();
    assert_eq!(key.key_id, "h1");
    assert_eq!(key.expires_at, None);
}

#[tokio::test]
async fn mint_key_validates_before_sending() {
    let server = MockServer::start().await;
    let mut r = request();
    r.models.clear();
    let err = gateway(&server).mint_key(&r).await.unwrap_err();
    assert!(matches!(err, GatewayError::InvalidRequest(_)), "{err:?}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn mint_key_without_a_token_id_is_a_transport_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/key/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "sk-v"})))
        .mount(&server)
        .await;
    let err = gateway(&server).mint_key(&request()).await.unwrap_err();
    assert!(matches!(err, GatewayError::Transport(_)), "{err:?}");
    assert!(!err.to_string().contains("sk-v"));
}

#[tokio::test]
async fn gateway_rejection_carries_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/key/generate"))
        .respond_with(ResponseTemplate::new(401).set_body_string("{\"error\":\"bad master key\"}"))
        .mount(&server)
        .await;
    let err = gateway(&server).mint_key(&request()).await.unwrap_err();
    match err {
        GatewayError::Rejected { status, body } => {
            assert_eq!(status, 401);
            assert!(body.contains("bad master key"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn revoke_posts_keys_and_maps_404_to_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/key/delete"))
        .and(body_json(json!({"keys": ["hashed-token-abc"]})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"deleted_keys": ["hashed-token-abc"]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/key/delete"))
        .and(body_json(json!({"keys": ["missing"]})))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"detail": "key not found"})))
        .mount(&server)
        .await;
    let g = gateway(&server);
    g.revoke("hashed-token-abc").await.unwrap();
    let err = g.revoke("missing").await.unwrap_err();
    assert_eq!(err, GatewayError::NotFound("missing".into()));
}

#[tokio::test]
async fn revoke_with_nothing_deleted_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/key/delete"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted_keys": []})))
        .mount(&server)
        .await;
    let err = gateway(&server).revoke("k").await.unwrap_err();
    assert_eq!(err, GatewayError::NotFound("k".into()));
}

fn spend_query(page: u32) -> SpendQuery {
    SpendQuery {
        key_alias: Some("run-1:coder:1".into()),
        start: "2026-11-01 00:00:00".into(),
        end: "2026-11-01 23:59:59".into(),
        page,
        page_size: 2,
    }
}

#[tokio::test]
async fn spend_logs_pages_and_normalizes_rows() {
    let server = MockServer::start().await;
    let body = common::fixture("litellm/spend_logs_v2_page1.json");
    Mock::given(method("GET"))
        .and(path("/spend/logs/v2"))
        .and(query_param("key_alias", "run-1:coder:1"))
        .and(query_param("start_date", "2026-11-01 00:00:00"))
        .and(query_param("end_date", "2026-11-01 23:59:59"))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "2"))
        .and(header("authorization", "Bearer sk-master-test"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
        .expect(1)
        .mount(&server)
        .await;

    let page = gateway(&server).spend_logs(&spend_query(1)).await.unwrap();
    assert_eq!(page.page, 1);
    assert_eq!(page.total_pages, 2);
    // The fixture also carries a row of "run-1:coder:10", which the server's
    // substring filter lets through; the client drops it.
    assert_eq!(page.records.len(), 2);

    let a = &page.records[0];
    assert_eq!(a.request_id, "chatcmpl-a1");
    assert_eq!(a.key_alias.as_deref(), Some("run-1:coder:1"));
    assert_eq!(a.model, "anthropic/claude-sonnet-4-5");
    assert_eq!(a.spend_usd, 0.0123);
    assert_eq!(a.prompt_tokens, 12000);
    assert_eq!(a.completion_tokens, 300);
    assert_eq!(a.cached_tokens, 10000);
    assert_eq!(a.cache_creation_input_tokens, 1500);
    assert_eq!(a.cache_hit, Some(false));
    assert_eq!(a.start_time, "2026-11-01T10:00:00.123000Z");
    assert_eq!(a.end_time, "2026-11-01T10:00:04Z");
    assert_eq!(a.metadata.get("run_id").map(String::as_str), Some("run-1"));
    assert_eq!(a.metadata.get("arm").map(String::as_str), Some("baseline"));

    let b = &page.records[1];
    assert_eq!(b.request_id, "resp-b2");
    assert_eq!(b.model, "openai/gpt-5");
    assert_eq!(
        b.cached_tokens, 4096,
        "OpenAI prompt_tokens_details.cached_tokens"
    );
    assert_eq!(b.cache_creation_input_tokens, 0);
    assert_eq!(b.cache_hit, None);
    assert_eq!(
        b.start_time, "2026-11-01T11:00:00Z",
        "naive timestamp read as UTC"
    );
    assert!(
        b.metadata.is_empty(),
        "metadata given as a JSON string without spend_logs_metadata"
    );
}

#[tokio::test]
async fn spend_logs_without_alias_sends_no_alias_filter_and_computes_total_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/spend/logs/v2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [], "total": 5, "page": 3, "page_size": 2
        })))
        .mount(&server)
        .await;
    let mut q = spend_query(3);
    q.key_alias = None;
    let page = gateway(&server).spend_logs(&q).await.unwrap();
    assert_eq!(page.total_pages, 3);
    assert!(page.records.is_empty());
    let reqs = server.received_requests().await.unwrap();
    assert!(!reqs[0].url.query().unwrap_or("").contains("key_alias"));
}

#[tokio::test]
async fn spend_logs_validates_paging() {
    let server = MockServer::start().await;
    let mut q = spend_query(1);
    q.page_size = 1001;
    let err = gateway(&server).spend_logs(&q).await.unwrap_err();
    assert!(matches!(err, GatewayError::InvalidRequest(_)));
}

#[test]
fn from_env_requires_the_master_key_variable() {
    let err =
        LiteLlmGateway::from_env("http://llm-gateway:4000", "GUARD_TEST_UNSET_MASTER_KEY_ENV")
            .unwrap_err();
    assert!(matches!(err, GatewayError::Unconfigured(_)));
    assert!(err.to_string().contains("GUARD_TEST_UNSET_MASTER_KEY_ENV"));
}

#[test]
fn new_rejects_a_bad_url() {
    let err = LiteLlmGateway::new("ftp://x", Secret::new("k")).unwrap_err();
    assert!(matches!(err, GatewayError::Unconfigured(_)));
}
