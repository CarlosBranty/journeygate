//! reqwest client for `POST /v1/systemone` and the `DecisionEngine` impl.
//!
//! Per attempt: 800 ms timeout (connect + response body). Exactly one retry on
//! `429`, `529` or a timeout, after `max(retry_backoff, Retry-After)`. A
//! `Retry-After` longer than `max_retry_after` is not waited for: the call
//! fails at once (fail closed beats blocking triage). Everything else is final.

use crate::convert::{ParsedResponse, check_model, impact_answers, triage_answers};
use crate::error_detail::error_detail;
use crate::questions::{IMPACT_BATCH_SIZE, impact_questions, triage_questions};
use crate::wire::{Question, SystemOneRequest};
use crate::{DEFAULT_URL, MODEL};
use guard_core::ports::{
    DecisionEngine, DiffSummary, EngineError, JourneyQuestion, TriageAnswers, TriageRequest,
};
use indexmap::IndexMap;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// Env var holding the API key.
pub const API_KEY_ENV: &str = "TYPESAFE_API_KEY";
/// Env var overriding the endpoint (tests point it at wiremock).
pub const URL_ENV: &str = "JEV_URL";

/// Connection settings. `Debug` never prints the key.
#[derive(Debug, Clone)]
pub struct JevConfig {
    /// Full endpoint URL, `https://api.typesafe.ai/v1/systemone` by default.
    pub url: String,
    pub api_key: SecretString,
    /// Per-attempt timeout (contract: 800 ms).
    pub timeout: Duration,
    /// Minimum wait before the single retry (contract: 200 ms).
    pub retry_backoff: Duration,
    /// Longest `Retry-After` the client is willing to wait for.
    pub max_retry_after: Duration,
}

impl JevConfig {
    pub fn new(url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            api_key: SecretString::from(api_key.into()),
            timeout: Duration::from_millis(800),
            retry_backoff: Duration::from_millis(200),
            max_retry_after: Duration::from_secs(2),
        }
    }

    /// `TYPESAFE_API_KEY` (required, non-blank) and `JEV_URL` (optional).
    pub fn from_env() -> Result<Self, EngineError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// [`JevConfig::from_env`] with an injectable variable lookup.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, EngineError> {
        let key = lookup(API_KEY_ENV)
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .ok_or_else(|| EngineError::Unconfigured(format!("{API_KEY_ENV} is not set")))?;
        let url = lookup(URL_ENV)
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| DEFAULT_URL.to_string());
        Ok(Self::new(url, key))
    }
}

/// `DecisionEngine` backed by Jev.
#[derive(Debug, Clone)]
pub struct JevEngine {
    http: reqwest::Client,
    config: JevConfig,
}

enum Attempt {
    Done(Result<ParsedResponse, EngineError>),
    Retry {
        error: EngineError,
        retry_after: Option<Duration>,
    },
}

/// `Retry-After` in (possibly fractional) seconds. HTTP-dates are ignored.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let raw = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    let secs: f64 = raw.trim().parse().ok()?;
    (secs.is_finite() && secs >= 0.0).then(|| Duration::from_secs_f64(secs))
}

fn transport(e: reqwest::Error) -> EngineError {
    EngineError::Transport(e.to_string())
}

impl JevEngine {
    pub fn new(config: JevConfig) -> Result<Self, EngineError> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(transport)?;
        Ok(Self { http, config })
    }

    /// Shorthand for `JevEngine::new(JevConfig::from_env()?)`.
    pub fn from_env() -> Result<Self, EngineError> {
        Self::new(JevConfig::from_env()?)
    }

    pub fn config(&self) -> &JevConfig {
        &self.config
    }

    /// Sends one request (with the single retry) and returns the decoded body.
    /// The model is checked here: a drifted model is never returned.
    pub async fn ask<S: Serialize + Sync>(
        &self,
        state: &S,
        questions: IndexMap<String, Question>,
    ) -> Result<ParsedResponse, EngineError> {
        let body = serde_json::to_vec(&SystemOneRequest::new(state, questions))
            .map_err(|e| EngineError::Transport(format!("cannot encode request: {e}")))?;

        let parsed = match self.attempt(&body).await {
            Attempt::Done(result) => result,
            Attempt::Retry { error, retry_after } => {
                let wait = retry_after.map_or(self.config.retry_backoff, |r| {
                    r.max(self.config.retry_backoff)
                });
                if retry_after.is_some_and(|r| r > self.config.max_retry_after) {
                    return Err(error);
                }
                tokio::time::sleep(wait).await;
                match self.attempt(&body).await {
                    Attempt::Done(result) => result,
                    Attempt::Retry { error, .. } => Err(error),
                }
            }
        }?;
        check_model(&parsed.response)?;
        tracing::debug!(
            model = %parsed.response.model,
            input_tokens = parsed.response.usage.input_tokens,
            output_tokens = parsed.response.usage.output_tokens,
            "jev usage"
        );
        Ok(parsed)
    }

    async fn attempt(&self, body: &[u8]) -> Attempt {
        let sent = self
            .http
            .post(&self.config.url)
            .bearer_auth(self.config.api_key.expose_secret())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .await;
        let response = match sent {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Attempt::Retry {
                    error: EngineError::Timeout,
                    retry_after: None,
                };
            }
            Err(e) => return Attempt::Done(Err(transport(e))),
        };

        let status = response.status().as_u16();
        let retry_after = parse_retry_after(response.headers());
        let bytes = match response.bytes().await {
            Ok(b) => b,
            Err(e) if e.is_timeout() => {
                return Attempt::Retry {
                    error: EngineError::Timeout,
                    retry_after: None,
                };
            }
            Err(e) => return Attempt::Done(Err(transport(e))),
        };
        match status {
            200 => Attempt::Done(ParsedResponse::from_slice(&bytes)),
            429 | 529 => Attempt::Retry {
                error: EngineError::Retryable(status),
                retry_after,
            },
            _ => Attempt::Done(Err(EngineError::Rejected {
                status,
                body: error_detail(&String::from_utf8_lossy(&bytes)),
            })),
        }
    }
}

#[async_trait::async_trait]
impl DecisionEngine for JevEngine {
    fn model(&self) -> &str {
        MODEL
    }

    async fn triage(&self, request: &TriageRequest) -> Result<TriageAnswers, EngineError> {
        let questions = triage_questions(&request.areas, request.permutation_seed)?;
        let parsed = self.ask(&request.state, questions).await?;
        triage_answers(&parsed, &request.areas)
    }

    async fn impact(
        &self,
        diff: &DiffSummary,
        journeys: &[JourneyQuestion],
    ) -> Result<BTreeMap<String, f64>, EngineError> {
        let mut out = BTreeMap::new();
        for batch in journeys.chunks(IMPACT_BATCH_SIZE) {
            let parsed = self.ask(diff, impact_questions(batch)).await?;
            out.extend(impact_answers(&parsed, batch)?);
        }
        Ok(out)
    }
}
