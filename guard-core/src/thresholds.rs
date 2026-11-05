//! `thresholds.toml`: the calibrated Gatekeeper thresholds and the triage mode.
//!
//! Contract: `docs/contracts/thresholds-toml.md` and
//! `schemas/thresholds.schema.json`. The thresholds come out of calibration and
//! are owned by the platform team, not by the app team, which is why they do
//! not live in `target.toml`.
//!
//! The data types and their defaults are part of the contract and are complete;
//! parsing and validation are filled in by the core-domain builder.

use serde::{Deserialize, Serialize};

/// Shadow Mode records every decision but always applies the human lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Shadow,
    Active,
}

impl Mode {
    pub fn is_shadow(self) -> bool {
        self == Mode::Shadow
    }
}

/// The parsed `thresholds.toml`, with every default applied.
///
/// Comparisons in the Gatekeeper are strict and negated, so `NaN` fails closed
/// into the human lane (see `gatekeeper::decide`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    /// File format version. Must be `1`.
    pub version: i64,
    pub mode: Mode,
    /// Agent lane requires `agent_suitable > agent_suitable_min`.
    pub agent_suitable_min: f64,
    /// Agent lane requires `risk.score < risk_score_max` (0-based Score, 5 levels).
    pub risk_score_max: f64,
    /// Agent lane requires `touches_money_or_security < money_or_security_max`.
    pub money_or_security_max: f64,
    /// Agent lane requires `clear_e2e_criteria > clear_criteria_min`.
    pub clear_criteria_min: f64,
    /// Agent lane requires area, kind and risk confidence `> confidence_min`.
    pub confidence_min: f64,
    /// Agent lane requires `changes_existing_flow < existing_flow_max`.
    pub existing_flow_max: f64,
    /// A journey is selected by impact when its Noul is `> impact_threshold`.
    pub impact_threshold: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            version: 1,
            mode: Mode::Shadow,
            agent_suitable_min: 0.80,
            risk_score_max: 1.0,
            money_or_security_max: 0.15,
            clear_criteria_min: 0.70,
            confidence_min: 0.75,
            existing_flow_max: 0.20,
            impact_threshold: 0.25,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ThresholdsError {
    /// TOML syntax error, unknown key or wrong type.
    #[error("thresholds.toml: {0}")]
    Parse(String),
    /// A value outside its range (see `docs/contracts/thresholds-toml.md`).
    #[error("thresholds.toml: {field}: {message}")]
    Invalid { field: String, message: String },
}

/// Parses `thresholds.toml` text and validates every range rule.
///
/// An empty document is valid and yields [`Thresholds::default`].
pub fn parse_thresholds(text: &str) -> Result<Thresholds, ThresholdsError> {
    let thresholds: Thresholds = toml::from_str(text)
        .map_err(|e| ThresholdsError::Parse(e.to_string().trim_end().to_string()))?;
    validate(&thresholds)?;
    Ok(thresholds)
}

/// Range rules of the contract: probabilities in `0.0..=1.0`, `risk_score_max`
/// in `0.0..=4.0`, `version == 1`, every value finite.
pub fn validate(thresholds: &Thresholds) -> Result<(), ThresholdsError> {
    if thresholds.version != 1 {
        return Err(invalid("version", "must be 1"));
    }
    let t = thresholds;
    const PROBABILITY: (f64, &str) = (1.0, "must be between 0.0 and 1.0");
    const SCORE: (f64, &str) = (4.0, "must be between 0.0 and 4.0");
    // Declaration order, so the first violation reported is deterministic.
    let ranges = [
        ("agent_suitable_min", t.agent_suitable_min, PROBABILITY),
        ("risk_score_max", t.risk_score_max, SCORE),
        (
            "money_or_security_max",
            t.money_or_security_max,
            PROBABILITY,
        ),
        ("clear_criteria_min", t.clear_criteria_min, PROBABILITY),
        ("confidence_min", t.confidence_min, PROBABILITY),
        ("existing_flow_max", t.existing_flow_max, PROBABILITY),
        ("impact_threshold", t.impact_threshold, PROBABILITY),
    ];
    for (field, value, (max, message)) in ranges {
        if !value.is_finite() {
            return Err(invalid(field, "must be finite"));
        }
        if !(0.0..=max).contains(&value) {
            return Err(invalid(field, message));
        }
    }
    Ok(())
}

fn invalid(field: &str, message: &str) -> ThresholdsError {
    ThresholdsError::Invalid {
        field: field.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests;
