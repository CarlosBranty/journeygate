//! HTTP behavior of the Jev client against wiremock: every status code,
//! timeout, retry and model drift.

mod common;

use common::{areas, diff, fixture, journeys, triage_request};
use guard_core::ports::{DecisionEngine, EngineError, Kind};
use guard_jev::{DEFAULT_URL, JevConfig, JevEngine, MODEL};
use indexmap::IndexMap;
use std::time::{Duration, Instant};
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-key-123";

fn config(server: &MockServer) -> JevConfig {
    JevConfig {
        timeout: Duration::from_millis(300),
        retry_backoff: Duration::from_millis(50),
        ..JevConfig::new(format!("{}/v1/systemone", server.uri()), KEY)
    }
}

fn engine(server: &MockServer) -> JevEngine {
    JevEngine::new(config(server)).unwrap()
}

fn ok(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "application/json")
}

fn status_with(code: u16, name: &str) -> ResponseTemplate {
    ResponseTemplate::new(code).set_body_raw(fixture(name), "application/json")
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap_or_default()
}

fn question_names(req: &Request) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Body {
        questions: IndexMap<String, serde_json::Value>,
    }
    let body: Body = serde_json::from_slice(&req.body).unwrap();
    body.questions.into_keys().collect()
}

// ------------------------------------------------------------ config

#[test]
fn defaults_follow_the_contract() {
    let c = JevConfig::new(DEFAULT_URL, KEY);
    assert_eq!(DEFAULT_URL, "https://api.typesafe.ai/v1/systemone");
    assert_eq!(MODEL, "jev-1.13.0");
    assert_eq!(c.timeout, Duration::from_millis(800));
    assert_eq!(c.retry_backoff, Duration::from_millis(200));
    // The key never shows up in Debug output.
    assert!(!format!("{c:?}").contains(KEY));
}

#[test]
fn config_from_lookup_reads_key_and_url() {
    let c = JevConfig::from_lookup(|name| match name {
        "TYPESAFE_API_KEY" => Some(KEY.into()),
        "JEV_URL" => Some("http://localhost:9/v1/systemone".into()),
        _ => None,
    })
    .unwrap();
    assert_eq!(c.url, "http://localhost:9/v1/systemone");

    let c =
        JevConfig::from_lookup(|name| (name == "TYPESAFE_API_KEY").then(|| KEY.into())).unwrap();
    assert_eq!(c.url, DEFAULT_URL);

    for missing in [None, Some("  ".to_string())] {
        let err = JevConfig::from_lookup(|_| missing.clone()).unwrap_err();
        assert!(matches!(err, EngineError::Unconfigured(_)), "{err:?}");
    }
}

#[test]
fn engine_reports_pinned_model() {
    let e = JevEngine::new(JevConfig::new(DEFAULT_URL, KEY)).unwrap();
    assert_eq!(e.model(), MODEL);
}

// ------------------------------------------------------------ triage happy path

#[tokio::test]
async fn triage_sends_bearer_pinned_model_and_ordered_questions() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(serde_json::json!({
            "model": MODEL,
            "state": {"blast_radius": "isolated"}
        })))
        .respond_with(ok("triage_response.json"))
        .expect(1)
        .mount(&server)
        .await;

    let answers = engine(&server).triage(&triage_request(None)).await.unwrap();
    assert_eq!(answers.kind.value, Kind::Bugfix);
    assert_eq!(answers.area.value.as_str(), "catalog");

    let reqs = requests(&server).await;
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body, common::fixture_json("triage_request.json"));
    assert_eq!(
        question_names(&reqs[0]),
        [
            "area",
            "kind",
            "risk",
            "clear_e2e_criteria",
            "touches_money_or_security",
            "agent_suitable",
            "changes_existing_flow"
        ]
    );
    // The ticket key never travels.
    assert!(!String::from_utf8_lossy(&reqs[0].body).contains("SHOP-42"));
}

#[tokio::test]
async fn triage_with_seed_sends_permuted_options() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json"))
        .mount(&server)
        .await;
    let seed = 42;
    engine(&server)
        .triage(&triage_request(Some(seed)))
        .await
        .unwrap();
    let reqs = requests(&server).await;
    let body: serde_json::Value = serde_json::from_slice(&reqs[0].body).unwrap();
    let expected = guard_jev::questions::triage_questions(&areas(), Some(seed)).unwrap();
    assert_eq!(body["questions"], serde_json::to_value(&expected).unwrap());
}

// ------------------------------------------------------------ non-retryable errors

#[tokio::test]
async fn http_400_is_rejected_with_parsed_detail_and_no_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(status_with(400, "error_400.json"))
        .expect(1)
        .mount(&server)
        .await;
    let err = engine(&server)
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Rejected {
            status: 400,
            body: "Unknown model 'jev-latest'. Pin an exact model version.".into()
        }
    );
}

#[tokio::test]
async fn http_422_is_rejected_with_validation_details_and_no_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(status_with(422, "error_422.json"))
        .expect(1)
        .mount(&server)
        .await;
    let err = engine(&server)
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::Rejected {
            status: 422,
            body: "body.questions.area.criteria: Choice needs at least 2 options; body.state: field required".into()
        }
    );
}

#[tokio::test]
async fn other_statuses_are_rejected_with_raw_body() {
    for code in [401u16, 403, 404, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(code).set_body_string("upstream says no"))
            .expect(1)
            .mount(&server)
            .await;
        let err = engine(&server)
            .triage(&triage_request(None))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            EngineError::Rejected {
                status: code,
                body: "upstream says no".into()
            }
        );
    }
}

// ------------------------------------------------------------ retry

#[tokio::test]
async fn http_429_and_529_retry_once_then_fail_retryable() {
    for code in [429u16, 529] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(status_with(code, "error_429.json"))
            .expect(2)
            .mount(&server)
            .await;
        let started = Instant::now();
        let err = engine(&server)
            .triage(&triage_request(None))
            .await
            .unwrap_err();
        assert_eq!(err, EngineError::Retryable(code));
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "backoff waited"
        );
    }
}

#[tokio::test]
async fn retry_succeeds_after_one_overload() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(status_with(529, "error_429.json"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json"))
        .mount(&server)
        .await;
    let answers = engine(&server).triage(&triage_request(None)).await.unwrap();
    assert_eq!(answers.model, MODEL);
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn retry_after_header_is_honored() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(status_with(429, "error_429.json").insert_header("retry-after", "1"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json"))
        .mount(&server)
        .await;
    let started = Instant::now();
    engine(&server).triage(&triage_request(None)).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "waited {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn retry_after_beyond_the_cap_fails_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(status_with(429, "error_429.json").insert_header("retry-after", "120"))
        .expect(1)
        .mount(&server)
        .await;
    let started = Instant::now();
    let err = engine(&server)
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Retryable(429));
    assert!(started.elapsed() < Duration::from_secs(2));
}

// ------------------------------------------------------------ timeout

#[tokio::test]
async fn timeout_retries_once_then_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json").set_delay(Duration::from_millis(1_000)))
        .expect(2)
        .mount(&server)
        .await;
    let started = Instant::now();
    let err = engine(&server)
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert_eq!(err, EngineError::Timeout);
    // Two attempts of 300 ms plus the 50 ms backoff, well under two full delays.
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(600), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(1_900), "{elapsed:?}");
}

#[tokio::test]
async fn timeout_then_success_recovers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json").set_delay(Duration::from_millis(1_000)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json"))
        .mount(&server)
        .await;
    assert!(engine(&server).triage(&triage_request(None)).await.is_ok());
}

#[tokio::test]
async fn default_timeout_is_800_ms() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response.json").set_delay(Duration::from_millis(1_500)))
        .mount(&server)
        .await;
    let cfg = JevConfig {
        retry_backoff: Duration::from_millis(10),
        ..JevConfig::new(format!("{}/v1/systemone", server.uri()), KEY)
    };
    let started = Instant::now();
    let err = JevEngine::new(cfg)
        .unwrap()
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    let elapsed = started.elapsed();
    assert_eq!(err, EngineError::Timeout);
    assert!(elapsed >= Duration::from_millis(1_600), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(2_900), "{elapsed:?}");
}

// ------------------------------------------------------------ drift and decoding

#[tokio::test]
async fn model_drift_is_never_accepted_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("triage_response_drift.json"))
        .expect(1)
        .mount(&server)
        .await;
    let err = engine(&server)
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        EngineError::ModelDrift {
            expected: MODEL.into(),
            actual: "jev-1.14.0".into()
        }
    );
}

#[tokio::test]
async fn malformed_200_body_is_a_transport_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"model\": "))
        .expect(1)
        .mount(&server)
        .await;
    let err = engine(&server)
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn connection_refused_is_a_transport_error() {
    let cfg = JevConfig {
        timeout: Duration::from_millis(300),
        retry_backoff: Duration::from_millis(10),
        ..JevConfig::new("http://127.0.0.1:9/v1/systemone", KEY)
    };
    let err = JevEngine::new(cfg)
        .unwrap()
        .triage(&triage_request(None))
        .await
        .unwrap_err();
    assert!(
        matches!(err, EngineError::Transport(_) | EngineError::Timeout),
        "{err:?}"
    );
}

// ------------------------------------------------------------ impact

#[tokio::test]
async fn impact_with_no_journeys_never_calls_the_engine() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ok("impact_response.json"))
        .expect(0)
        .mount(&server)
        .await;
    let map = engine(&server).impact(&diff(), &[]).await.unwrap();
    assert!(map.is_empty());
}

#[tokio::test]
async fn impact_sends_diff_state_and_maps_answers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_partial_json(serde_json::json!({
            "model": MODEL,
            "state": serde_json::to_value(diff()).unwrap(),
        })))
        .respond_with(ok("impact_response.json"))
        .expect(1)
        .mount(&server)
        .await;
    let map = engine(&server).impact(&diff(), &journeys(2)).await.unwrap();
    assert_eq!(map["shop.journey-00"], 0.93);
    assert_eq!(map["shop.journey-01"], 0.02);
    let reqs = requests(&server).await;
    assert_eq!(question_names(&reqs[0]), ["impact_0", "impact_1"]);
    // Journey ids never travel as question names or anywhere else.
    assert!(!String::from_utf8_lossy(&reqs[0].body).contains("shop.journey-00"));
}

/// Answers every impact question of the request with `noul = n / 100`.
struct EchoImpact;

impl wiremock::Respond for EchoImpact {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let names = question_names(req);
        let answers: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|name| {
                let n: f64 = name.trim_start_matches("impact_").parse().unwrap();
                (
                    name.clone(),
                    serde_json::json!({"type": "noul", "noul": n / 100.0}),
                )
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": MODEL,
            "answers": answers,
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }))
    }
}

#[tokio::test]
async fn impact_batches_25_questions_per_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(EchoImpact)
        .expect(3)
        .mount(&server)
        .await;
    let js = journeys(60);
    let map = engine(&server).impact(&diff(), &js).await.unwrap();
    assert_eq!(map.len(), 60);
    let reqs = requests(&server).await;
    let sizes: Vec<usize> = reqs.iter().map(|r| question_names(r).len()).collect();
    assert_eq!(sizes, [25, 25, 10]);
    // Names restart at 0 in every batch and map back to the right journey.
    assert_eq!(question_names(&reqs[1])[0], "impact_0");
    assert_eq!(map["shop.journey-00"], 0.0);
    assert_eq!(map["shop.journey-26"], 0.01);
    assert_eq!(map["shop.journey-59"], 0.09);
}

#[tokio::test]
async fn impact_fails_closed_when_any_batch_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(EchoImpact)
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(status_with(422, "error_422.json"))
        .mount(&server)
        .await;
    let err = engine(&server)
        .impact(&diff(), &journeys(30))
        .await
        .unwrap_err();
    assert!(matches!(err, EngineError::Rejected { status: 422, .. }));
}

#[tokio::test]
async fn impact_drift_is_rejected() {
    let server = MockServer::start().await;
    let mut body = common::fixture_json("impact_response.json");
    body["model"] = "jev-latest".into();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    assert!(matches!(
        engine(&server).impact(&diff(), &journeys(2)).await,
        Err(EngineError::ModelDrift { .. })
    ));
}
