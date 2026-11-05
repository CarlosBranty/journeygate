//! `DecisionEngine`: the classifier behind triage and impact (Jev in production).

use super::types::{DiffSummary, JourneyQuestion, TriageAnswers, TriageRequest};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// No API key / endpoint configured.
    #[error("decision engine not configured: {0}")]
    Unconfigured(String),
    /// The request exceeded the 800 ms timeout (after the single retry).
    #[error("decision engine timed out")]
    Timeout,
    /// 429 or 529 after the single backoff retry.
    #[error("decision engine overloaded (HTTP {0})")]
    Retryable(u16),
    /// 400, 422 or any other non-200 status, with the response body.
    #[error("decision engine rejected the request (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
    /// The response `model` differs from the pinned model.
    #[error("model drift: expected {expected}, got {actual}")]
    ModelDrift { expected: String, actual: String },
    /// A question has no answer in the response.
    #[error("missing answer for question {0}")]
    MissingAnswer(String),
    /// An answer of the wrong type, an unknown Choice option or a non-finite number.
    #[error("unexpected answer for question {question}: {detail}")]
    UnexpectedAnswer { question: String, detail: String },
    /// Network, TLS or JSON decoding failure.
    #[error("decision engine transport error: {0}")]
    Transport(String),
}

/// The classifier. Any error routes to the human lane (triage) or to running
/// every journey (impact): callers never retry beyond what the adapter does.
#[async_trait::async_trait]
pub trait DecisionEngine: Send + Sync {
    /// The pinned model id (`jev-1.13.0`), recorded in the decision log.
    fn model(&self) -> &str;

    /// Asks the seven triage questions in one request and converts the answers.
    async fn triage(&self, request: &TriageRequest) -> Result<TriageAnswers, EngineError>;

    /// One Noul per journey: probability that the diff affects it.
    ///
    /// Returns exactly one entry per input journey id (adapters batch requests
    /// to respect the engine's size limits). An empty `journeys` returns an
    /// empty map without calling the engine.
    async fn impact(
        &self,
        diff: &DiffSummary,
        journeys: &[JourneyQuestion],
    ) -> Result<BTreeMap<String, f64>, EngineError>;
}
