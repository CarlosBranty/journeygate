//! `S3EvidenceStore`: S3-compatible object storage (MinIO in `deploy/`) with
//! path-style addressing, SigV4-signed `PUT`/`GET` and presigned `GET` URLs.
//!
//! Configured by a URL (`docs/contracts/finalize-phase2.md` § Storage):
//! `s3+https://<access_key>:<secret_key>@<host>[:<port>]/<bucket>[?region=<region>]`
//! (`s3+http` for a local MinIO, region default `us-east-1`, credentials
//! percent-encoded).

use crate::sigv4::{
    Credentials, HeaderRequest, amz_date, authorization, canonical_path, presigned_query,
    sha256_hex,
};
use guard_core::ports::evidence_store::{validate_expiry, validate_key};
use guard_core::ports::{EvidenceError, EvidenceStore, ObjectRef};
use percent_encoding::percent_decode_str;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;

pub const DEFAULT_REGION: &str = "us-east-1";
/// Error bodies are cut to this many characters.
const MAX_ERROR_BODY: usize = 2000;

/// Parsed storage URL. `Debug` never prints the secret key.
#[derive(Debug, Clone)]
pub struct S3Config {
    /// `http(s)://host[:port]`, no trailing slash.
    pub endpoint: String,
    /// The `Host` header value: `host` or `host:port` (non-default port).
    pub host: String,
    pub bucket: String,
    pub region: String,
    credentials: Credentials,
}

fn unconfigured(msg: impl Into<String>) -> EvidenceError {
    EvidenceError::Unconfigured(msg.into())
}

impl S3Config {
    pub fn from_url(raw: &str) -> Result<Self, EvidenceError> {
        let url = url::Url::parse(raw)
            .map_err(|e| unconfigured(format!("storage URL is not a URL: {e}")))?;
        let scheme = match url.scheme() {
            "s3+http" => "http",
            "s3+https" => "https",
            other => {
                return Err(unconfigured(format!(
                    "storage URL scheme must be s3+http or s3+https, got {other:?}"
                )));
            }
        };
        let host = url
            .host_str()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| unconfigured("storage URL has no host"))?;
        let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
        let access_key = decode(url.username());
        let secret_key = url.password().map(decode).unwrap_or_default();
        if access_key.is_empty() || secret_key.is_empty() {
            return Err(unconfigured(
                "storage URL needs <access_key>:<secret_key>@ credentials",
            ));
        }
        let segments: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
        let [bucket] = segments.as_slice() else {
            return Err(unconfigured("storage URL path must be exactly /<bucket>"));
        };
        let bucket = decode(bucket);
        let region = url
            .query_pairs()
            .find(|(k, _)| k == "region")
            .map(|(_, v)| v.into_owned())
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| DEFAULT_REGION.to_string());
        // Default ports are dropped so the signed Host matches what the client sends.
        let port = url
            .port()
            .filter(|p| !((scheme == "http" && *p == 80) || (scheme == "https" && *p == 443)));
        // `host_str` already brackets IPv6 literals.
        let host_header = match port {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        };
        Ok(Self {
            endpoint: format!("{scheme}://{host_header}"),
            host: host_header,
            bucket,
            region,
            credentials: Credentials {
                access_key,
                secret_key,
            },
        })
    }
}

type Clock = Arc<dyn Fn() -> OffsetDateTime + Send + Sync>;

/// Wait before the single retry of a transient failure.
const RETRY_BACKOFF: Duration = Duration::from_millis(500);

#[derive(Clone)]
pub struct S3EvidenceStore {
    config: S3Config,
    http: reqwest::Client,
    clock: Clock,
}

impl std::fmt::Debug for S3EvidenceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3EvidenceStore")
            .field("config", &self.config)
            .finish()
    }
}

impl S3EvidenceStore {
    pub fn new(config: S3Config) -> Result<Self, EvidenceError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| EvidenceError::Transport(e.to_string()))?;
        Ok(Self {
            config,
            http,
            clock: Arc::new(OffsetDateTime::now_utc),
        })
    }

    pub fn from_url(url: &str) -> Result<Self, EvidenceError> {
        Self::new(S3Config::from_url(url)?)
    }

    /// Replaces the clock used for signing (tests).
    pub fn with_clock(
        mut self,
        clock: impl Fn() -> OffsetDateTime + Send + Sync + 'static,
    ) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub fn config(&self) -> &S3Config {
        &self.config
    }

    fn object_path(&self, key: &str) -> String {
        canonical_path(&[&self.config.bucket, key])
    }

    /// One request, retried once after [`RETRY_BACKOFF`] on a transport error
    /// or a 500/502/503/504 (S3 `SlowDown` is a 503).
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<reqwest::Response, EvidenceError> {
        let first = self
            .send_once(method.clone(), path, body.clone(), content_type)
            .await;
        let transient = match &first {
            Err(EvidenceError::Transport(_)) => true,
            Ok(resp) => matches!(resp.status().as_u16(), 500 | 502 | 503 | 504),
            Err(_) => false,
        };
        if !transient {
            return first;
        }
        tokio::time::sleep(RETRY_BACKOFF).await;
        self.send_once(method, path, body, content_type).await
    }

    async fn send_once(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
    ) -> Result<reqwest::Response, EvidenceError> {
        let date = amz_date((self.clock)());
        let payload_hash = sha256_hex(&body);
        let mut headers: Vec<(&str, String)> = vec![
            ("host", self.config.host.clone()),
            ("x-amz-content-sha256", payload_hash.clone()),
            ("x-amz-date", date.clone()),
        ];
        if let Some(ct) = content_type {
            headers.push(("content-type", ct.to_string()));
        }
        let auth = authorization(
            &self.config.credentials,
            &HeaderRequest {
                method: method.as_str(),
                path,
                query: &[],
                headers: &headers,
                payload_hash: &payload_hash,
                amz_date: &date,
                region: &self.config.region,
            },
        );
        let url = format!("{}{path}", self.config.endpoint);
        let mut req = self
            .http
            .request(method, &url)
            .header("authorization", auth)
            .header("x-amz-content-sha256", payload_hash)
            .header("x-amz-date", date);
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        req.body(body)
            .send()
            .await
            .map_err(|e| EvidenceError::Transport(e.without_url().to_string()))
    }

    /// Creates the bucket unless it already exists (`HEAD`, then `PUT`).
    pub async fn ensure_bucket(&self) -> Result<(), EvidenceError> {
        let path = canonical_path(&[&self.config.bucket]);
        let head = self
            .send(reqwest::Method::HEAD, &path, vec![], None)
            .await?;
        if head.status().is_success() {
            return Ok(());
        }
        let put = self.send(reqwest::Method::PUT, &path, vec![], None).await?;
        let status = put.status();
        if status.is_success() {
            return Ok(());
        }
        let body = error_body(put).await;
        if status.as_u16() == 409 && body.contains("BucketAlreadyOwnedByYou") {
            return Ok(());
        }
        // A concurrent creator can win the race between our HEAD and PUT; some
        // S3 servers (SeaweedFS) then answer BucketAlreadyExists even to the
        // owner. Trust a second HEAD over the PUT error.
        if status.as_u16() == 409 {
            let again = self
                .send(reqwest::Method::HEAD, &path, vec![], None)
                .await?;
            if again.status().is_success() {
                return Ok(());
            }
            return Err(EvidenceError::Rejected {
                status: status.as_u16(),
                body: format!("{body} (bucket HEAD then answered {})", again.status()),
            });
        }
        Err(EvidenceError::Rejected {
            status: status.as_u16(),
            body,
        })
    }
}

async fn error_body(resp: reqwest::Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    text.chars().take(MAX_ERROR_BODY).collect()
}

#[async_trait::async_trait]
impl EvidenceStore for S3EvidenceStore {
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<ObjectRef, EvidenceError> {
        validate_key(key)?;
        let size = bytes.len() as u64;
        let resp = self
            .send(
                reqwest::Method::PUT,
                &self.object_path(key),
                bytes,
                Some(content_type),
            )
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(EvidenceError::Rejected {
                status: status.as_u16(),
                body: error_body(resp).await,
            });
        }
        Ok(ObjectRef {
            key: key.to_string(),
            size,
        })
    }

    async fn get_object(&self, key: &str) -> Result<Vec<u8>, EvidenceError> {
        validate_key(key)?;
        let resp = self
            .send(reqwest::Method::GET, &self.object_path(key), vec![], None)
            .await?;
        let status = resp.status();
        if status.as_u16() == 404 {
            return Err(EvidenceError::NotFound(key.to_string()));
        }
        if !status.is_success() {
            return Err(EvidenceError::Rejected {
                status: status.as_u16(),
                body: error_body(resp).await,
            });
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| EvidenceError::Transport(e.without_url().to_string()))
    }

    fn presign_get(&self, key: &str, expires_in: Duration) -> Result<String, EvidenceError> {
        validate_key(key)?;
        validate_expiry(expires_in)?;
        let path = self.object_path(key);
        let query = presigned_query(
            &self.config.credentials,
            &self.config.host,
            &path,
            &amz_date((self.clock)()),
            &self.config.region,
            expires_in.as_secs().max(1),
        );
        Ok(format!("{}{path}?{query}", self.config.endpoint))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minio_url() {
        let c = S3Config::from_url("s3+http://minio%2Badmin:p%40ss@localhost:9000/guard-evidence")
            .unwrap();
        assert_eq!(c.endpoint, "http://localhost:9000");
        assert_eq!(c.host, "localhost:9000");
        assert_eq!(c.bucket, "guard-evidence");
        assert_eq!(c.region, "us-east-1");
        assert_eq!(c.credentials.access_key, "minio+admin");
        assert_eq!(c.credentials.secret_key, "p@ss");
    }

    #[test]
    fn parses_https_with_region_and_default_port() {
        let c = S3Config::from_url(
            "s3+https://AK:SK@s3.eu-west-1.amazonaws.com:443/b?region=eu-west-1",
        )
        .unwrap();
        assert_eq!(c.endpoint, "https://s3.eu-west-1.amazonaws.com");
        assert_eq!(c.host, "s3.eu-west-1.amazonaws.com");
        assert_eq!(c.region, "eu-west-1");
    }

    #[test]
    fn rejects_bad_urls_without_leaking_the_secret() {
        for bad in [
            "https://AK:SK@host/bucket",
            "s3+http://host/bucket",
            "s3+http://AK@host/bucket",
            "s3+http://AK:SK@host/",
            "s3+http://AK:SK@host/a/b",
            "not a url",
        ] {
            match S3Config::from_url(bad) {
                Err(EvidenceError::Unconfigured(msg)) => assert!(!msg.contains("SK"), "{msg}"),
                other => panic!("{bad}: {other:?}"),
            }
        }
    }

    #[test]
    fn debug_hides_the_secret() {
        let s = S3EvidenceStore::from_url("s3+http://AK:topsecret@localhost:9000/b").unwrap();
        assert!(!format!("{s:?}").contains("topsecret"));
    }

    #[test]
    fn presign_is_deterministic_for_a_fixed_clock() {
        let s = S3EvidenceStore::from_url("s3+http://AK:SK@localhost:9000/b")
            .unwrap()
            .with_clock(|| OffsetDateTime::from_unix_timestamp(1_369_353_600).unwrap());
        let a = s.presign_get("k/x.png", Duration::from_secs(60)).unwrap();
        let b = s.presign_get("k/x.png", Duration::from_secs(60)).unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("http://localhost:9000/b/k/x.png?X-Amz-Algorithm=AWS4-HMAC-SHA256&"));
        assert!(a.contains("X-Amz-Date=20130524T000000Z"));
        assert!(a.contains("X-Amz-Expires=60"));
    }
}
