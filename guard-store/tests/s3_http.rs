//! HTTP behavior of the S3 `EvidenceStore` against wiremock: path-style
//! addressing, SigV4 headers, error mapping and presigned URLs.

use guard_core::ports::{EvidenceError, EvidenceStore};
use guard_store::S3EvidenceStore;
use sha2::{Digest, Sha256};
use std::time::Duration;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn store(server: &MockServer) -> S3EvidenceStore {
    let addr = server.address();
    let url = format!(
        "s3+http://AKID%2Fx:se%2Bcret@{}:{}/evidence?region=eu-west-1",
        addr.ip(),
        addr.port()
    );
    S3EvidenceStore::from_url(&url).unwrap()
}

fn authorization(req: &Request) -> String {
    req.headers["authorization"].to_str().unwrap().to_string()
}

#[tokio::test]
async fn put_object_is_a_signed_path_style_put() {
    let server = MockServer::start().await;
    let body = b"png bytes".to_vec();
    let body_hash = hex::encode(Sha256::digest(&body));
    Mock::given(method("PUT"))
        .and(path("/evidence/acme/run%201/screenshot.png"))
        .and(header("content-type", "image/png"))
        .and(header("x-amz-content-sha256", body_hash.as_str()))
        .and(header_exists("x-amz-date"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;

    let r = store(&server)
        .put_object("acme/run 1/screenshot.png", body.clone(), "image/png")
        .await
        .unwrap();
    assert_eq!(r.key, "acme/run 1/screenshot.png");
    assert_eq!(r.size, body.len() as u64);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].body, body);
    let auth = authorization(&requests[0]);
    let date = requests[0].headers["x-amz-date"].to_str().unwrap();
    assert!(
        auth.starts_with(&format!(
            "AWS4-HMAC-SHA256 Credential=AKID/x/{}/eu-west-1/s3/aws4_request, ",
            &date[..8]
        )),
        "{auth}"
    );
    assert!(
        auth.contains(
            "SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, Signature="
        ),
        "{auth}"
    );
    let signature = auth.rsplit("Signature=").next().unwrap();
    assert_eq!(signature.len(), 64);
}

#[tokio::test]
async fn get_object_returns_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/evidence/acme/trace.zip"))
        .and(header(
            "x-amz-content-sha256",
            hex::encode(Sha256::digest(b"")).as_str(),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PK zip".to_vec()))
        .mount(&server)
        .await;
    let bytes = store(&server).get_object("acme/trace.zip").await.unwrap();
    assert_eq!(bytes, b"PK zip");
    let auth = authorization(&server.received_requests().await.unwrap()[0]);
    assert!(
        auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date,"),
        "{auth}"
    );
}

#[tokio::test]
async fn get_object_404_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(404).set_body_string("<Error><Code>NoSuchKey</Code></Error>"),
        )
        .mount(&server)
        .await;
    let err = store(&server)
        .get_object("acme/missing.png")
        .await
        .unwrap_err();
    assert_eq!(err, EvidenceError::NotFound("acme/missing.png".into()));
}

#[tokio::test]
async fn other_errors_are_rejected_with_status_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_string("<Error><Code>SignatureDoesNotMatch</Code></Error>"),
        )
        .mount(&server)
        .await;
    let err = store(&server)
        .put_object("acme/x.png", vec![1], "image/png")
        .await
        .unwrap_err();
    match err {
        EvidenceError::Rejected { status, body } => {
            assert_eq!(status, 403);
            assert!(body.contains("SignatureDoesNotMatch"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn unreachable_endpoint_is_a_transport_error() {
    // Port 9 (discard) on loopback: nothing listens there.
    let s = S3EvidenceStore::from_url("s3+http://a:b@127.0.0.1:9/bucket").unwrap();
    let err = s.get_object("k").await.unwrap_err();
    assert!(matches!(err, EvidenceError::Transport(_)), "{err:?}");
}

#[tokio::test]
async fn presigned_url_is_path_style_with_sigv4_query() {
    let server = MockServer::start().await;
    let url = store(&server)
        .presign_get("acme/run 1/trace.zip", Duration::from_secs(3600))
        .unwrap();
    let parsed = url::Url::parse(&url).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert_eq!(parsed.path(), "/evidence/acme/run%201/trace.zip");
    let query: std::collections::BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(query["X-Amz-Algorithm"], "AWS4-HMAC-SHA256");
    assert_eq!(query["X-Amz-Expires"], "3600");
    assert_eq!(query["X-Amz-SignedHeaders"], "host");
    assert!(query["X-Amz-Credential"].starts_with("AKID/x/"));
    assert!(query["X-Amz-Credential"].ends_with("/eu-west-1/s3/aws4_request"));
    assert_eq!(query["X-Amz-Signature"].len(), 64);
}

#[tokio::test]
async fn ensure_bucket_accepts_created_and_already_owned() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/evidence"))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_string("<Error><Code>BucketAlreadyOwnedByYou</Code></Error>"),
        )
        .mount(&server)
        .await;
    store(&server).ensure_bucket().await.unwrap();

    let other = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/evidence"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&other)
        .await;
    store(&other).ensure_bucket().await.unwrap();
}

#[tokio::test]
async fn a_transient_5xx_is_retried_once() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(503).set_body_string("<Code>SlowDown</Code>"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    store(&server)
        .put_object("acme/x.png", vec![1, 2], "image/png")
        .await
        .unwrap();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].body, vec![1, 2]);
}

#[tokio::test]
async fn a_persistent_5xx_is_retried_only_once_and_4xx_never() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let err = store(&server)
        .put_object("acme/x.png", vec![1], "image/png")
        .await
        .unwrap_err();
    assert!(
        matches!(err, EvidenceError::Rejected { status: 500, .. }),
        "{err:?}"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    let other = MockServer::start().await;
    Mock::given(method("PUT"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&other)
        .await;
    let _ = store(&other)
        .put_object("acme/x.png", vec![1], "image/png")
        .await
        .unwrap_err();
    assert_eq!(other.received_requests().await.unwrap().len(), 1);
}
