//! The seven triage questions and the per-journey impact Noul questions.
//!
//! Every name, text and option below is part of the calibration dataset
//! (`docs/contracts/jev-questions.md`): changing one invalidates calibration.

use crate::permutation::{KIND_SEED_XOR, permute};
use crate::wire::{NoulCriteria, Question};
use guard_core::config::MAX_AREAS;
use guard_core::ports::{AreaOption, EngineError, JourneyQuestion, Kind};
use indexmap::IndexMap;
use std::collections::BTreeSet;

pub const AREA: &str = "area";
pub const KIND: &str = "kind";
pub const RISK: &str = "risk";
pub const CLEAR_E2E_CRITERIA: &str = "clear_e2e_criteria";
pub const TOUCHES_MONEY_OR_SECURITY: &str = "touches_money_or_security";
pub const AGENT_SUITABLE: &str = "agent_suitable";
pub const CHANGES_EXISTING_FLOW: &str = "changes_existing_flow";

/// The triage question names, in request order.
pub const TRIAGE_QUESTION_NAMES: [&str; 7] = [
    AREA,
    KIND,
    RISK,
    CLEAR_E2E_CRITERIA,
    TOUCHES_MONEY_OR_SECURITY,
    AGENT_SUITABLE,
    CHANGES_EXISTING_FLOW,
];

const AREA_INSTRUCTIONS: &str = "Which product area of the application does this ticket primarily change? Choose the single area whose description best matches where the requested change lives.";

const KIND_INSTRUCTIONS: &str = "What kind of change does this ticket ask for?";

fn kind_description(kind: Kind) -> &'static str {
    match kind {
        Kind::Bugfix => "Fixes behavior that is wrong today, without adding new capabilities.",
        Kind::MinorFeature => {
            "Adds a small, self-contained capability or option to an existing flow."
        }
        Kind::Refactor => "Restructures code without changing user-visible behavior.",
        Kind::Architecture => {
            "Changes system structure: new services, data models, cross-cutting infrastructure, or how components communicate."
        }
    }
}

const RISK_INSTRUCTIONS: &str =
    "How risky is it to implement this ticket incorrectly, for users and for the business?";

/// Risk levels, low -> high. Never permuted: the order is the scale.
pub const RISK_LEVELS: [&str; 5] = [
    "Trivial: copy, styling or configuration with no behavioral impact.",
    "Low: a localized behavior change in one module, easy to verify and revert.",
    "Medium: a behavior change across several modules, or one that touches persisted data.",
    "High: changes a critical flow (payments, authentication, orders) or data shared by many features.",
    "Critical: could cause data loss, a security exposure or a full outage if wrong.",
];

/// `(name, instructions, criteria.true, criteria.false)` of the four triage Nouls.
const TRIAGE_NOULS: [(&str, &str, &str, &str); 4] = [
    (
        CLEAR_E2E_CRITERIA,
        "The ticket states acceptance criteria concrete enough that an end-to-end browser test can verify each of them through observable outcomes.",
        "Every criterion names an observable result in the UI or the API that a test can assert.",
        "Criteria are missing, vague, or only describe implementation details.",
    ),
    (
        TOUCHES_MONEY_OR_SECURITY,
        "Implementing this ticket changes how money is charged, refunded or displayed, or changes authentication, authorization, secrets or the handling of personal data.",
        "The change affects payments, prices, refunds, login, permissions, tokens, secrets or personal data.",
        "The change does not touch money, access control, secrets or personal data.",
    ),
    (
        AGENT_SUITABLE,
        "A competent autonomous coding agent could implement this ticket correctly using only the ticket text and the repository, without asking a human any clarifying question.",
        "The goal, scope and expected behavior are unambiguous and self-contained.",
        "The ticket needs product decisions, design input, access to external systems or clarification.",
    ),
    (
        CHANGES_EXISTING_FLOW,
        "This ticket intentionally changes how an existing user-facing process works (its steps, rules or outcome), rather than fixing it or adding a separate new process.",
        "An existing process will behave differently on purpose after this change.",
        "Existing processes keep working as they do today; the ticket fixes a defect or adds something new alongside them.",
    ),
];

/// Impact Noul `criteria.true`.
pub const IMPACT_TRUE: &str =
    "The changed code runs, or its output is used, while a user goes through this process.";
/// Impact Noul `criteria.false`.
pub const IMPACT_FALSE: &str =
    "The changed code is unrelated to this process or only affects other processes.";
/// Most impact questions per request; more journeys are sent in sequential batches.
pub const IMPACT_BATCH_SIZE: usize = 25;

fn noul(instructions: &str, when_true: &str, when_false: &str) -> Question {
    Question::Noul {
        instructions: instructions.to_string(),
        criteria: Some(NoulCriteria {
            when_true: when_true.to_string(),
            when_false: when_false.to_string(),
        }),
    }
}

/// The seven triage questions in contract order.
///
/// `areas` must be in canonical order (sorted by key, as `TargetConfig.areas`)
/// with 1..=`MAX_AREAS` unique keys. With `permutation_seed`, the `area` options
/// are shuffled with the seed and the `kind` options with
/// `seed ^ KIND_SEED_XOR`; `risk` is never permuted.
pub fn triage_questions(
    areas: &[AreaOption],
    permutation_seed: Option<u64>,
) -> Result<IndexMap<String, Question>, EngineError> {
    if areas.is_empty() || areas.len() > MAX_AREAS {
        return Err(EngineError::Unconfigured(format!(
            "the area Choice needs between 1 and {MAX_AREAS} options, got {}",
            areas.len()
        )));
    }
    let mut seen = BTreeSet::new();
    if let Some(dup) = areas.iter().find(|a| !seen.insert(a.key.as_str())) {
        return Err(EngineError::Unconfigured(format!(
            "duplicate area option {:?}",
            dup.key.as_str()
        )));
    }

    let mut area_options: Vec<&AreaOption> = areas.iter().collect();
    let mut kinds: Vec<Kind> = Kind::ALL.to_vec();
    if let Some(seed) = permutation_seed {
        permute(&mut area_options, seed);
        permute(&mut kinds, seed ^ KIND_SEED_XOR);
    }

    let mut questions = IndexMap::with_capacity(TRIAGE_QUESTION_NAMES.len());
    questions.insert(
        AREA.to_string(),
        Question::Choice {
            instructions: AREA_INSTRUCTIONS.to_string(),
            criteria: area_options
                .into_iter()
                .map(|a| (a.key.as_str().to_string(), Some(a.description.clone())))
                .collect(),
        },
    );
    questions.insert(
        KIND.to_string(),
        Question::Choice {
            instructions: KIND_INSTRUCTIONS.to_string(),
            criteria: kinds
                .into_iter()
                .map(|k| {
                    (
                        k.as_str().to_string(),
                        Some(kind_description(k).to_string()),
                    )
                })
                .collect(),
        },
    );
    questions.insert(
        RISK.to_string(),
        Question::Score {
            instructions: RISK_INSTRUCTIONS.to_string(),
            criteria: RISK_LEVELS.iter().map(|s| s.to_string()).collect(),
        },
    );
    for (name, instructions, when_true, when_false) in TRIAGE_NOULS {
        questions.insert(name.to_string(), noul(instructions, when_true, when_false));
    }
    Ok(questions)
}

/// `impact_<n>`: `n` is the 0-based index of the journey within one request.
pub fn impact_question_name(n: usize) -> String {
    format!("impact_{n}")
}

/// One Noul per journey of a single batch (call it with at most
/// [`IMPACT_BATCH_SIZE`] journeys). Instructions = `impact_question` verbatim.
pub fn impact_questions(journeys: &[JourneyQuestion]) -> IndexMap<String, Question> {
    journeys
        .iter()
        .enumerate()
        .map(|(n, j)| {
            (
                impact_question_name(n),
                noul(&j.impact_question, IMPACT_TRUE, IMPACT_FALSE),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triage_question_names_match_built_questions() {
        let areas = vec![AreaOption {
            key: guard_core::ports::Area::new("app"),
            description: "Everything.".into(),
        }];
        let q = triage_questions(&areas, None).unwrap();
        let names: Vec<&str> = q.keys().map(String::as_str).collect();
        assert_eq!(names, TRIAGE_QUESTION_NAMES);
    }

    #[test]
    fn max_areas_is_accepted() {
        let areas: Vec<AreaOption> = (0..MAX_AREAS)
            .map(|i| AreaOption {
                key: guard_core::ports::Area::new(format!("a{i:03}")),
                description: "x".into(),
            })
            .collect();
        assert!(triage_questions(&areas, Some(1)).is_ok());
    }
}
