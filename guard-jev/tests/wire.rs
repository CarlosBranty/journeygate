//! Serde round-trips of the Jev wire types against the recorded fixtures.

mod common;

use common::{areas, fixture, fixture_json, triage_state};
use guard_jev::MODEL;
use guard_jev::questions::triage_questions;
use guard_jev::wire::{Answer, Question, SystemOneRequest, SystemOneResponse};
use indexmap::IndexMap;

#[test]
fn triage_request_serializes_exactly_like_the_fixture() {
    let questions = triage_questions(&areas(), None).unwrap();
    let request = SystemOneRequest::new(triage_state(), questions);
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value, fixture_json("triage_request.json"));
}

#[test]
fn triage_request_keeps_question_and_option_order() {
    let questions = triage_questions(&areas(), None).unwrap();
    let request = SystemOneRequest::new(triage_state(), questions);
    let text = serde_json::to_string(&request).unwrap();
    // Re-read with order-preserving maps.
    let parsed: SystemOneRequest<serde_json::Value> = serde_json::from_str(&text).unwrap();
    let names: Vec<&str> = parsed.questions.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "area",
            "kind",
            "risk",
            "clear_e2e_criteria",
            "touches_money_or_security",
            "agent_suitable",
            "changes_existing_flow"
        ]
    );
    let Question::Choice { criteria, .. } = &parsed.questions["kind"] else {
        panic!("kind is a choice");
    };
    let options: Vec<&str> = criteria.keys().map(String::as_str).collect();
    assert_eq!(
        options,
        ["bugfix", "minor_feature", "refactor", "architecture"]
    );
}

#[test]
fn fixture_request_round_trips_and_keeps_order() {
    let text = fixture("triage_request.json");
    let parsed: SystemOneRequest<serde_json::Value> = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.model, MODEL);
    let again = serde_json::to_string(&parsed).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&again).unwrap(),
        fixture_json("triage_request.json")
    );
    let reparsed: SystemOneRequest<serde_json::Value> = serde_json::from_str(&again).unwrap();
    let a: Vec<_> = parsed.questions.keys().collect();
    let b: Vec<_> = reparsed.questions.keys().collect();
    assert_eq!(a.len(), 7);
    assert_eq!(a, b);
}

#[test]
fn triage_response_fixture_round_trips() {
    let response: SystemOneResponse =
        serde_json::from_str(&fixture("triage_response.json")).unwrap();
    assert_eq!(response.model, MODEL);
    assert_eq!(response.usage.input_tokens, 1834);
    assert_eq!(response.usage.output_tokens, 61);
    assert_eq!(response.answers.len(), 7);
    match &response.answers["area"] {
        Answer::Choice {
            choice,
            confidence,
            probabilities,
        } => {
            assert_eq!(choice, "catalog");
            assert_eq!(*confidence, 0.91);
            let order: Vec<&str> = probabilities.keys().map(String::as_str).collect();
            assert_eq!(order, ["catalog", "checkout", "account"]);
        }
        other => panic!("area: {other:?}"),
    }
    match &response.answers["risk"] {
        Answer::Score {
            score,
            confidence,
            legend,
            probabilities,
        } => {
            assert_eq!(*score, 0.4);
            assert_eq!(*confidence, 0.88);
            assert_eq!(legend.len(), 5);
            assert_eq!(probabilities["0"], 0.7);
        }
        other => panic!("risk: {other:?}"),
    }
    assert_eq!(
        response.answers["agent_suitable"],
        Answer::Noul { noul: 0.9 }
    );
    let again = serde_json::to_value(&response).unwrap();
    assert_eq!(again, fixture_json("triage_response.json"));
}

#[test]
fn impact_response_fixture_parses() {
    let response: SystemOneResponse =
        serde_json::from_str(&fixture("impact_response.json")).unwrap();
    assert_eq!(response.answers["impact_0"], Answer::Noul { noul: 0.93 });
    assert_eq!(response.answers["impact_1"], Answer::Noul { noul: 0.02 });
}

#[test]
fn noul_answer_has_no_confidence_field() {
    let value = serde_json::to_value(Answer::Noul { noul: 0.12 }).unwrap();
    assert_eq!(value, serde_json::json!({"type": "noul", "noul": 0.12}));
}

#[test]
fn unknown_answer_type_is_unsupported() {
    let answer: Answer =
        serde_json::from_value(serde_json::json!({"type": "box", "box": [1, 2, 3, 4]})).unwrap();
    assert_eq!(answer, Answer::Unsupported);
}

#[test]
fn unknown_question_type_is_unsupported() {
    let q: Question =
        serde_json::from_value(serde_json::json!({"type": "box", "instructions": "x"})).unwrap();
    assert_eq!(q, Question::Unsupported);
}

#[test]
fn noul_question_criteria_are_optional() {
    let q = Question::Noul {
        instructions: "It rains.".into(),
        criteria: None,
    };
    let value = serde_json::to_value(&q).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"type": "noul", "instructions": "It rains."})
    );
    let back: Question = serde_json::from_value(value).unwrap();
    assert_eq!(back, q);
}

#[test]
fn choice_option_without_description_serializes_as_null() {
    let mut criteria = IndexMap::new();
    criteria.insert("yes".to_string(), None);
    criteria.insert("no".to_string(), Some("Not at all.".to_string()));
    let q = Question::Choice {
        instructions: "Pick.".into(),
        criteria,
    };
    let value = serde_json::to_value(&q).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"type": "choice", "instructions": "Pick.", "criteria": {"yes": null, "no": "Not at all."}})
    );
}

#[test]
fn response_without_usage_still_parses() {
    let response: SystemOneResponse = serde_json::from_value(serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {"impact_0": {"type": "noul", "noul": 0.5}}
    }))
    .unwrap();
    assert_eq!(response.usage.input_tokens, 0);
}
