//! Jev `POST /v1/systemone` wire types.
//!
//! Shapes follow `docs/contracts/jev-questions.md`. Question and answer enums
//! are internally tagged by `type` (lowercase). Maps whose order matters to the
//! model (questions, Choice options, Choice probabilities) are `IndexMap`s.

use crate::MODEL;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Request body: `{"model", "state", "questions"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemOneRequest<S> {
    pub model: String,
    pub state: S,
    /// Question name -> question, sent in insertion order.
    pub questions: IndexMap<String, Question>,
}

impl<S> SystemOneRequest<S> {
    /// A request for the pinned [`MODEL`].
    pub fn new(state: S, questions: IndexMap<String, Question>) -> Self {
        Self {
            model: MODEL.to_string(),
            state,
            questions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    /// A statement; the answer is the probability that it is true.
    Noul {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    /// Pick one option. Option name -> description (`None` = judged by its name).
    Choice {
        instructions: String,
        criteria: IndexMap<String, Option<String>>,
    },
    /// Ordered levels, low -> high; level 0 is the lowest.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Any other question type (e.g. `box`). This platform never sends one.
    #[serde(other)]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub when_true: String,
    #[serde(rename = "false")]
    pub when_false: String,
}

/// Response body of a `200`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemOneResponse {
    /// The resolved model version that actually answered.
    pub model: String,
    /// Keyed by question name.
    pub answers: IndexMap<String, Answer>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    /// Probability that the statement is true. There is no confidence field.
    Noul { noul: f64 },
    Choice {
        choice: String,
        confidence: f64,
        probabilities: IndexMap<String, f64>,
    },
    /// Probability-weighted mean of 0-based levels (`0.0..=levels-1`).
    Score {
        score: f64,
        confidence: f64,
        legend: BTreeMap<String, String>,
        probabilities: BTreeMap<String, f64>,
    },
    /// Any other answer type (e.g. `box`): always an `UnexpectedAnswer`.
    #[serde(other)]
    Unsupported,
}

impl Answer {
    /// The wire `type` tag, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Answer::Noul { .. } => "noul",
            Answer::Choice { .. } => "choice",
            Answer::Score { .. } => "score",
            Answer::Unsupported => "unsupported",
        }
    }
}
