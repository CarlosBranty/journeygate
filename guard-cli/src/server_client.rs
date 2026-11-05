//! Client of the `guard serve` API (server-api.md) used by `guard test
//! --affected`, `guard gate finalize` and `guard repro`.

use guard_core::config::TargetConfig;
use guard_core::ports::{DiffSummary, GateReport, JourneyQuestion};
use guard_server::api::{GateResultResponse, ImpactRequest, ImpactResponse, PrContextResponse};
use std::time::Duration;

/// The bearer token variable shared with the server.
pub const TOKEN_ENV: &str = "GUARD_SERVER_TOKEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerError {
    /// `GUARD_SERVER_TOKEN` is not set.
    MissingToken,
    /// A non-2xx answer: `{error, detail}` of the contract when available.
    Status {
        status: u16,
        error: String,
        detail: String,
    },
    Transport(String),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::MissingToken => write!(f, "{TOKEN_ENV} is not set"),
            ServerError::Status {
                status,
                error,
                detail,
            } => write!(f, "HTTP {status} {error}: {detail}"),
            ServerError::Transport(e) => write!(f, "{e}"),
        }
    }
}

pub struct ServerClient {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl ServerClient {
    /// `[server].url` plus `GUARD_SERVER_TOKEN`; `None` without `[server]`.
    pub fn from_config(config: &TargetConfig) -> Option<Self> {
        let server = config.server.as_ref()?;
        Some(Self::new(
            &server.url,
            std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty()),
        ))
    }

    pub fn new(base: &str, token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("guard-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            http,
        }
    }

    fn token(&self) -> Result<&str, ServerError> {
        self.token.as_deref().ok_or(ServerError::MissingToken)
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        response: reqwest::Response,
    ) -> Result<T, ServerError> {
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            return Err(ServerError::Status {
                status,
                error: parsed["error"].as_str().unwrap_or("http_error").to_string(),
                detail: parsed["detail"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| body.chars().take(500).collect()),
            });
        }
        serde_json::from_str(&body)
            .map_err(|e| ServerError::Transport(format!("invalid response body: {e}")))
    }

    /// `POST /api/impact`.
    pub async fn impact(
        &self,
        diff: &DiffSummary,
        journeys: &[JourneyQuestion],
    ) -> Result<ImpactResponse, ServerError> {
        let body = ImpactRequest {
            diff: diff.clone(),
            journeys: journeys.to_vec(),
        };
        let response = self
            .http
            .post(format!("{}/api/impact", self.base))
            .bearer_auth(self.token()?)
            .json(&body)
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Self::decode(response).await
    }

    /// `POST /api/gate-result`.
    pub async fn gate_result(
        &self,
        report: &GateReport,
    ) -> Result<GateResultResponse, ServerError> {
        let response = self
            .http
            .post(format!("{}/api/gate-result", self.base))
            .bearer_auth(self.token()?)
            .json(report)
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Self::decode(response).await
    }

    /// `GET /api/pr-context?number=<n>`.
    pub async fn pr_context(&self, number: u64) -> Result<PrContextResponse, ServerError> {
        let response = self
            .http
            .get(format!("{}/api/pr-context?number={number}", self.base))
            .bearer_auth(self.token()?)
            .send()
            .await
            .map_err(|e| ServerError::Transport(e.to_string()))?;
        Self::decode(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn question(id: &str) -> JourneyQuestion {
        JourneyQuestion {
            id: id.into(),
            impact_question: "q".into(),
        }
    }

    #[tokio::test]
    async fn impact_sends_the_bearer_token_and_the_journeys() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/impact"))
            .and(header("authorization", "Bearer t0k"))
            .and(body_partial_json(
                serde_json::json!({"journeys": [{"id": "a.b"}]}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(
                {"model": "jev-1.13.0", "threshold": 0.25, "impact": {"a.b": 0.81}}
            )))
            .expect(1)
            .mount(&server)
            .await;
        let client = ServerClient::new(&format!("{}/", server.uri()), Some("t0k".into()));
        let answer = client
            .impact(&DiffSummary::default(), &[question("a.b")])
            .await
            .unwrap();
        assert_eq!(answer.impact["a.b"], 0.81);
        assert_eq!(answer.threshold, 0.25);
    }

    #[tokio::test]
    async fn contract_errors_are_decoded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/impact"))
            .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!(
                {"error": "engine_unavailable", "detail": "timed out"}
            )))
            .mount(&server)
            .await;
        let client = ServerClient::new(&server.uri(), Some("t".into()));
        let error = client
            .impact(&DiffSummary::default(), &[question("a.b")])
            .await
            .unwrap_err();
        assert_eq!(
            error,
            ServerError::Status {
                status: 503,
                error: "engine_unavailable".into(),
                detail: "timed out".into()
            }
        );
    }

    #[tokio::test]
    async fn a_missing_token_never_sends_a_request() {
        let client = ServerClient::new("http://127.0.0.1:9", None);
        let error = client.pr_context(1).await.unwrap_err();
        assert_eq!(error, ServerError::MissingToken);
    }

    #[tokio::test]
    async fn pr_context_reads_the_latest_gate() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/pr-context"))
            .and(query_param("number", "12"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "pr": {"provider": "github", "repo": "acme/shop", "number": 12, "title": "t",
                       "url": "u", "author": "dev", "ticket_key": null, "branch": "b",
                       "base_branch": "main", "base_sha": "a", "head_sha": "b",
                       "changed_files": []},
                "ticket": null,
                "latest_gate": null
            })))
            .mount(&server)
            .await;
        let client = ServerClient::new(&server.uri(), Some("t".into()));
        let answer = client.pr_context(12).await.unwrap();
        assert_eq!(answer.pr.number, 12);
        assert!(answer.latest_gate.is_none());
    }
}
