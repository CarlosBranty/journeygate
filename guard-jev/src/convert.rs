//! Validated conversion of Jev responses into domain values.
//!
//! Rules (`docs/contracts/jev-questions.md`, "Conversion rules"): pinned model,
//! every expected name present with the expected type, every number finite,
//! Noul and confidences within `0.0..=1.0`, Score within `0.0..=4.0`.

use crate::MODEL;
use crate::questions::{
    AGENT_SUITABLE, AREA, CHANGES_EXISTING_FLOW, CLEAR_E2E_CRITERIA, KIND, RISK, RISK_LEVELS,
    TOUCHES_MONEY_OR_SECURITY, impact_question_name,
};
use crate::wire::{Answer, SystemOneResponse};
use guard_core::ports::{
    Area, AreaOption, Classified, EngineError, JourneyQuestion, Kind, TriageAnswers,
};
use std::collections::BTreeMap;

/// A decoded `200` body plus its `answers` object verbatim (for calibration).
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedResponse {
    pub response: SystemOneResponse,
    pub raw_answers: serde_json::Value,
}

impl ParsedResponse {
    /// Decodes a response body. Malformed JSON or shapes are `Transport` errors.
    pub fn from_slice(body: &[u8]) -> Result<Self, EngineError> {
        let value: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| EngineError::Transport(format!("invalid JSON response: {e}")))?;
        let raw_answers = value
            .get("answers")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let response: SystemOneResponse = serde_json::from_value(value)
            .map_err(|e| EngineError::Transport(format!("unexpected response shape: {e}")))?;
        Ok(Self {
            response,
            raw_answers,
        })
    }
}

/// `ModelDrift` unless the response model is exactly [`MODEL`].
pub fn check_model(response: &SystemOneResponse) -> Result<(), EngineError> {
    if response.model == MODEL {
        Ok(())
    } else {
        Err(EngineError::ModelDrift {
            expected: MODEL.to_string(),
            actual: response.model.clone(),
        })
    }
}

fn unexpected(question: &str, detail: impl Into<String>) -> EngineError {
    EngineError::UnexpectedAnswer {
        question: question.to_string(),
        detail: detail.into(),
    }
}

fn answer<'a>(response: &'a SystemOneResponse, name: &str) -> Result<&'a Answer, EngineError> {
    response
        .answers
        .get(name)
        .ok_or_else(|| EngineError::MissingAnswer(name.to_string()))
}

fn in_range(question: &str, what: &str, value: f64, max: f64) -> Result<f64, EngineError> {
    if value.is_finite() && (0.0..=max).contains(&value) {
        Ok(value)
    } else {
        Err(unexpected(
            question,
            format!("{what} {value} is outside 0.0..={max}"),
        ))
    }
}

fn noul(response: &SystemOneResponse, name: &str) -> Result<f64, EngineError> {
    match answer(response, name)? {
        Answer::Noul { noul } => in_range(name, "noul", *noul, 1.0),
        other => Err(unexpected(
            name,
            format!("expected noul, got {}", other.type_name()),
        )),
    }
}

fn choice<'a>(response: &'a SystemOneResponse, name: &str) -> Result<(&'a str, f64), EngineError> {
    match answer(response, name)? {
        Answer::Choice {
            choice, confidence, ..
        } => Ok((
            choice.as_str(),
            in_range(name, "confidence", *confidence, 1.0)?,
        )),
        other => Err(unexpected(
            name,
            format!("expected choice, got {}", other.type_name()),
        )),
    }
}

fn score(response: &SystemOneResponse, name: &str) -> Result<Classified<f64>, EngineError> {
    match answer(response, name)? {
        Answer::Score {
            score, confidence, ..
        } => {
            let max = (RISK_LEVELS.len() - 1) as f64;
            Ok(Classified {
                value: in_range(name, "score", *score, max)?,
                confidence: in_range(name, "confidence", *confidence, 1.0)?,
            })
        }
        other => Err(unexpected(
            name,
            format!("expected score, got {}", other.type_name()),
        )),
    }
}

/// Converts a triage response. `areas` are the options that were offered.
pub fn triage_answers(
    parsed: &ParsedResponse,
    areas: &[AreaOption],
) -> Result<TriageAnswers, EngineError> {
    let response = &parsed.response;
    check_model(response)?;

    let (area_name, area_confidence) = choice(response, AREA)?;
    if !areas.iter().any(|a| a.key.as_str() == area_name) {
        return Err(unexpected(
            AREA,
            format!("{area_name:?} is not one of the offered areas"),
        ));
    }
    let (kind_name, kind_confidence) = choice(response, KIND)?;
    let kind = Kind::from_name(kind_name)
        .ok_or_else(|| unexpected(KIND, format!("{kind_name:?} is not a known kind")))?;

    Ok(TriageAnswers {
        model: response.model.clone(),
        area: Classified {
            value: Area::new(area_name),
            confidence: area_confidence,
        },
        kind: Classified {
            value: kind,
            confidence: kind_confidence,
        },
        risk: score(response, RISK)?,
        clear_e2e_criteria: noul(response, CLEAR_E2E_CRITERIA)?,
        touches_money_or_security: noul(response, TOUCHES_MONEY_OR_SECURITY)?,
        agent_suitable: noul(response, AGENT_SUITABLE)?,
        changes_existing_flow: noul(response, CHANGES_EXISTING_FLOW)?,
        raw: parsed.raw_answers.clone(),
    })
}

/// Converts one impact batch: `impact_<n>` -> `journeys[n].id`.
pub fn impact_answers(
    parsed: &ParsedResponse,
    journeys: &[JourneyQuestion],
) -> Result<BTreeMap<String, f64>, EngineError> {
    check_model(&parsed.response)?;
    journeys
        .iter()
        .enumerate()
        .map(|(n, j)| {
            Ok((
                j.id.clone(),
                noul(&parsed.response, &impact_question_name(n))?,
            ))
        })
        .collect()
}
