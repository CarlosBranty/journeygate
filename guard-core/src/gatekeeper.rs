//! The deterministic Gatekeeper (section 2 of the architecture doc).
//!
//! Hard rules on facts first, calibrated signals second. Every calibrated check
//! is a negated strict comparison, so a `NaN` ends in the human lane.
//!
//! Check order (and therefore the order of `HumanReason`s in the result):
//!
//! | # | Hit when | Reason |
//! | --- | --- | --- |
//! | 1 | `facts.human_only_path_hit` | `HardRulePath` |
//! | 2 | `facts.schema_migration` | `SchemaMigration` |
//! | 3 | `facts.max_fan_out > rules.max_fan_out_for_agent` | `FanOutTooWide` |
//! | 4 | `rules.sensitive_areas` contains `answers.area.value` | `SensitiveArea` |
//! | 5 | `answers.kind.value == Architecture` | `ArchitecturalKind` |
//! | 6 | `!(area.confidence > c && kind.confidence > c && risk.confidence > c)` | `LowConfidence` |
//! | 7 | `!(risk.value < risk_score_max)` | `RiskTooHigh` |
//! | 8 | `!(touches_money_or_security < money_or_security_max)` | `MoneyOrSecurity` |
//! | 9 | `!(agent_suitable > agent_suitable_min)` | `NotAgentSuitable` |
//! | 10 | `!(clear_e2e_criteria > clear_criteria_min)` | `UnclearCriteria` |
//! | 11 | `!(changes_existing_flow < existing_flow_max)` | `ChangesExistingFlow` |
//!
//! No hit = `Lane::Agent`. `EngineUnavailable` and `ShadowMode` are never
//! produced by [`decide`]: see [`decide_without_engine`] and
//! `triage::effective_lane`.

use crate::config::TargetConfig;
use crate::ports::types::{Area, HumanReason, ImpactFacts, Kind, Lane, TriageAnswers};
use crate::thresholds::Thresholds;
use std::collections::BTreeSet;

/// Rules that come from `target.toml`, not from calibration.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GateRules {
    /// `[hard_rules].max_fan_out_for_agent`.
    pub max_fan_out_for_agent: u32,
    /// Keys of `[areas]` with `sensitive = true`.
    pub sensitive_areas: BTreeSet<Area>,
}

impl GateRules {
    /// Reads `[hard_rules]` and `[areas]` of a validated config.
    pub fn from_config(config: &TargetConfig) -> Self {
        let limit = config.hard_rules.max_fan_out_for_agent;
        // A negative limit fails closed (every fan-out above zero is too wide).
        let max_fan_out_for_agent = u32::try_from(limit.max(0)).unwrap_or(u32::MAX);
        let sensitive_areas = config
            .areas
            .iter()
            .filter(|(_, area)| area.sensitive)
            .map(|(key, _)| Area::new(key.clone()))
            .collect();
        Self {
            max_fan_out_for_agent,
            sensitive_areas,
        }
    }
}

/// Routes a triaged ticket. See the module docs for the exact check table.
// The negated comparisons are the contract: `!(x > min)` is true for NaN, so a
// NaN on either side fails closed into the human lane. `x <= min` would not.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub fn decide(
    answers: &TriageAnswers,
    facts: &ImpactFacts,
    thresholds: &Thresholds,
    rules: &GateRules,
) -> Lane {
    let a = answers;
    let th = thresholds;
    let c = th.confidence_min;
    // Every calibrated check is a negated strict comparison: NaN on either side
    // makes the inner comparison false, so the check hits (fail closed).
    let calibrated = [
        (
            rules.sensitive_areas.contains(&a.area.value),
            HumanReason::SensitiveArea,
        ),
        (
            a.kind.value == Kind::Architecture,
            HumanReason::ArchitecturalKind,
        ),
        (
            !(a.area.confidence > c && a.kind.confidence > c && a.risk.confidence > c),
            HumanReason::LowConfidence,
        ),
        (
            !(a.risk.value < th.risk_score_max),
            HumanReason::RiskTooHigh,
        ),
        (
            !(a.touches_money_or_security < th.money_or_security_max),
            HumanReason::MoneyOrSecurity,
        ),
        (
            !(a.agent_suitable > th.agent_suitable_min),
            HumanReason::NotAgentSuitable,
        ),
        (
            !(a.clear_e2e_criteria > th.clear_criteria_min),
            HumanReason::UnclearCriteria,
        ),
        (
            !(a.changes_existing_flow < th.existing_flow_max),
            HumanReason::ChangesExistingFlow,
        ),
    ];
    let mut reasons = hard_rule_reasons(facts, rules);
    reasons.extend(
        calibrated
            .into_iter()
            .filter(|(hit, _)| *hit)
            .map(|(_, r)| r),
    );
    if reasons.is_empty() {
        Lane::Agent
    } else {
        Lane::Human(reasons)
    }
}

/// The decision when the engine failed: the hard-rule reasons that apply to
/// `facts` (checks 1-3), followed by `EngineUnavailable`. Always human.
pub fn decide_without_engine(facts: &ImpactFacts, rules: &GateRules) -> Lane {
    let mut reasons = hard_rule_reasons(facts, rules);
    reasons.push(HumanReason::EngineUnavailable);
    Lane::Human(reasons)
}

/// Checks 1-3: facts computed in Rust that the engine can never override.
fn hard_rule_reasons(facts: &ImpactFacts, rules: &GateRules) -> Vec<HumanReason> {
    [
        (facts.human_only_path_hit, HumanReason::HardRulePath),
        (facts.schema_migration, HumanReason::SchemaMigration),
        (
            facts.max_fan_out > rules.max_fan_out_for_agent,
            HumanReason::FanOutTooWide,
        ),
    ]
    .into_iter()
    .filter(|(hit, _)| *hit)
    .map(|(_, reason)| reason)
    .collect()
}

#[cfg(test)]
mod tests;
