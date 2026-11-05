//! Question builders, Choice permutation and response conversion.

mod common;

use common::{areas, fixture, journeys};
use guard_core::ports::{Area, AreaOption, EngineError, Kind};
use guard_jev::convert::{impact_answers, triage_answers};
use guard_jev::permutation::{KIND_SEED_XOR, SplitMix64, permute};
use guard_jev::questions::{
    IMPACT_BATCH_SIZE, IMPACT_FALSE, IMPACT_TRUE, impact_question_name, impact_questions,
    triage_questions,
};
use guard_jev::wire::{Answer, NoulCriteria, Question, SystemOneResponse};
use guard_jev::{MODEL, ParsedResponse};

fn option_names(q: &Question) -> Vec<String> {
    match q {
        Question::Choice { criteria, .. } => criteria.keys().cloned().collect(),
        other => panic!("not a choice: {other:?}"),
    }
}

fn parsed(name: &str) -> ParsedResponse {
    ParsedResponse::from_slice(fixture(name).as_bytes()).unwrap()
}

// ------------------------------------------------------------ permutation

#[test]
fn splitmix64_matches_reference_vectors() {
    // Reference outputs of SplitMix64 seeded with 0.
    let mut rng = SplitMix64::new(0);
    assert_eq!(rng.next_u64(), 0xE220_A839_7B1D_CDAF);
    assert_eq!(rng.next_u64(), 0x6E78_9E6A_A1B9_65F4);
    assert_eq!(rng.next_u64(), 0x06C4_5D18_8009_454F);
}

#[test]
fn permute_is_deterministic_and_a_permutation() {
    let canonical: Vec<u32> = (0..20).collect();
    let mut a = canonical.clone();
    let mut b = canonical.clone();
    permute(&mut a, 12345);
    permute(&mut b, 12345);
    assert_eq!(a, b);
    assert_ne!(a, canonical, "20 items with this seed must move");
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(sorted, canonical);
}

#[test]
fn permute_follows_the_fisher_yates_recipe() {
    let mut items = vec!['a', 'b', 'c', 'd'];
    permute(&mut items, 7);
    // Re-derive by hand from the contract text.
    let mut expected = vec!['a', 'b', 'c', 'd'];
    let mut rng = SplitMix64::new(7);
    for i in (1..expected.len()).rev() {
        let j = (rng.next_u64() % (i as u64 + 1)) as usize;
        expected.swap(i, j);
    }
    assert_eq!(items, expected);
}

#[test]
fn permute_handles_empty_and_single() {
    let mut empty: Vec<u8> = vec![];
    permute(&mut empty, 1);
    let mut one = vec![9];
    permute(&mut one, 1);
    assert_eq!(one, [9]);
}

#[test]
fn seeded_triage_questions_permute_area_and_kind_independently() {
    let many: Vec<AreaOption> = (0..10)
        .map(|i| AreaOption {
            key: Area::new(format!("area{i}")),
            description: format!("Area {i}."),
        })
        .collect();
    let seed = 0xDEAD_BEEF;
    let q = triage_questions(&many, Some(seed)).unwrap();

    let mut expected_areas: Vec<String> = many.iter().map(|a| a.key.0.clone()).collect();
    permute(&mut expected_areas, seed);
    assert_eq!(option_names(&q["area"]), expected_areas);

    let mut expected_kinds: Vec<String> =
        Kind::ALL.iter().map(|k| k.as_str().to_string()).collect();
    permute(&mut expected_kinds, seed ^ KIND_SEED_XOR);
    assert_eq!(option_names(&q["kind"]), expected_kinds);

    // Descriptions travel with their option.
    let Question::Choice { criteria, .. } = &q["area"] else {
        unreachable!()
    };
    assert_eq!(criteria["area3"].as_deref(), Some("Area 3."));

    // Risk levels are never permuted.
    let canonical = triage_questions(&many, None).unwrap();
    assert_eq!(q["risk"], canonical["risk"]);
    // Same seed, same order (replayable).
    assert_eq!(triage_questions(&many, Some(seed)).unwrap(), q);
}

#[test]
fn unseeded_triage_questions_keep_canonical_order() {
    let q = triage_questions(&areas(), None).unwrap();
    assert_eq!(option_names(&q["area"]), ["account", "catalog", "checkout"]);
    assert_eq!(
        option_names(&q["kind"]),
        ["bugfix", "minor_feature", "refactor", "architecture"]
    );
}

#[test]
fn triage_questions_reject_bad_area_lists() {
    assert!(matches!(
        triage_questions(&[], None),
        Err(EngineError::Unconfigured(_))
    ));
    let too_many: Vec<AreaOption> = (0..256)
        .map(|i| AreaOption {
            key: Area::new(format!("a{i}")),
            description: "x".into(),
        })
        .collect();
    assert!(matches!(
        triage_questions(&too_many, None),
        Err(EngineError::Unconfigured(_))
    ));
    let dup = vec![areas()[0].clone(), areas()[0].clone()];
    assert!(matches!(
        triage_questions(&dup, None),
        Err(EngineError::Unconfigured(_))
    ));
}

// ------------------------------------------------------------ impact questions

#[test]
fn impact_questions_are_named_by_index_with_fixed_criteria() {
    let js = journeys(3);
    let q = impact_questions(&js);
    let names: Vec<&str> = q.keys().map(String::as_str).collect();
    assert_eq!(names, ["impact_0", "impact_1", "impact_2"]);
    assert_eq!(impact_question_name(2), "impact_2");
    assert_eq!(
        q["impact_1"],
        Question::Noul {
            instructions: "The diff affects journey number 1.".into(),
            criteria: Some(NoulCriteria {
                when_true: IMPACT_TRUE.into(),
                when_false: IMPACT_FALSE.into(),
            }),
        }
    );
    assert_eq!(
        IMPACT_TRUE,
        "The changed code runs, or its output is used, while a user goes through this process."
    );
    assert_eq!(
        IMPACT_FALSE,
        "The changed code is unrelated to this process or only affects other processes."
    );
    assert_eq!(IMPACT_BATCH_SIZE, 25);
}

// ------------------------------------------------------------ triage conversion

#[test]
fn triage_fixture_converts_to_domain_answers() {
    let p = parsed("triage_response.json");
    let answers = triage_answers(&p, &areas()).unwrap();
    assert_eq!(answers.model, MODEL);
    assert_eq!(answers.area.value, Area::new("catalog"));
    assert_eq!(answers.area.confidence, 0.91);
    assert_eq!(answers.kind.value, Kind::Bugfix);
    assert_eq!(answers.kind.confidence, 0.94);
    assert_eq!(answers.risk.value, 0.4);
    assert_eq!(answers.risk.confidence, 0.88);
    assert_eq!(answers.clear_e2e_criteria, 0.86);
    assert_eq!(answers.touches_money_or_security, 0.04);
    assert_eq!(answers.agent_suitable, 0.9);
    assert_eq!(answers.changes_existing_flow, 0.08);
    let raw = &common::fixture_json("triage_response.json")["answers"];
    assert_eq!(&answers.raw, raw);
}

#[test]
fn drifted_model_is_rejected_on_conversion() {
    let p = parsed("triage_response_drift.json");
    assert_eq!(
        triage_answers(&p, &areas()).unwrap_err(),
        EngineError::ModelDrift {
            expected: MODEL.into(),
            actual: "jev-1.14.0".into()
        }
    );
}

fn with_answer(name: &str, answer: serde_json::Value) -> ParsedResponse {
    let mut v = common::fixture_json("triage_response.json");
    if answer.is_null() {
        v["answers"].as_object_mut().unwrap().remove(name);
    } else {
        v["answers"][name] = answer;
    }
    ParsedResponse::from_slice(&serde_json::to_vec(&v).unwrap()).unwrap()
}

fn unexpected_for(p: &ParsedResponse) -> String {
    match triage_answers(p, &areas()).unwrap_err() {
        EngineError::UnexpectedAnswer { question, .. } => question,
        other => panic!("expected UnexpectedAnswer, got {other:?}"),
    }
}

#[test]
fn missing_answer_is_reported_by_name() {
    for name in [
        "area",
        "kind",
        "risk",
        "clear_e2e_criteria",
        "touches_money_or_security",
        "agent_suitable",
        "changes_existing_flow",
    ] {
        let p = with_answer(name, serde_json::Value::Null);
        assert_eq!(
            triage_answers(&p, &areas()).unwrap_err(),
            EngineError::MissingAnswer(name.into())
        );
    }
}

#[test]
fn wrong_answer_type_is_unexpected() {
    let p = with_answer("area", serde_json::json!({"type": "noul", "noul": 0.5}));
    assert_eq!(unexpected_for(&p), "area");
    let p = with_answer(
        "agent_suitable",
        serde_json::json!({"type": "box", "box": []}),
    );
    assert_eq!(unexpected_for(&p), "agent_suitable");
    let p = with_answer(
        "risk",
        serde_json::json!({"type": "choice", "choice": "x", "confidence": 0.9, "probabilities": {}}),
    );
    assert_eq!(unexpected_for(&p), "risk");
}

#[test]
fn unknown_choice_options_are_unexpected() {
    let p = with_answer(
        "area",
        serde_json::json!({"type": "choice", "choice": "billing", "confidence": 0.9, "probabilities": {}}),
    );
    assert_eq!(unexpected_for(&p), "area");
    let p = with_answer(
        "kind",
        serde_json::json!({"type": "choice", "choice": "feature", "confidence": 0.9, "probabilities": {}}),
    );
    assert_eq!(unexpected_for(&p), "kind");
}

#[test]
fn out_of_range_numbers_are_unexpected() {
    let p = with_answer(
        "agent_suitable",
        serde_json::json!({"type": "noul", "noul": 1.2}),
    );
    assert_eq!(unexpected_for(&p), "agent_suitable");
    let p = with_answer(
        "clear_e2e_criteria",
        serde_json::json!({"type": "noul", "noul": -0.1}),
    );
    assert_eq!(unexpected_for(&p), "clear_e2e_criteria");
    let p = with_answer(
        "area",
        serde_json::json!({"type": "choice", "choice": "catalog", "confidence": 1.5, "probabilities": {}}),
    );
    assert_eq!(unexpected_for(&p), "area");
    let p = with_answer(
        "risk",
        serde_json::json!({"type": "score", "score": 4.5, "confidence": 0.9, "legend": {}, "probabilities": {}}),
    );
    assert_eq!(unexpected_for(&p), "risk");
    let p = with_answer(
        "risk",
        serde_json::json!({"type": "score", "score": 2.0, "confidence": -0.2, "legend": {}, "probabilities": {}}),
    );
    assert_eq!(unexpected_for(&p), "risk");
}

#[test]
fn score_bounds_are_inclusive() {
    for score in [0.0, 4.0] {
        let p = with_answer(
            "risk",
            serde_json::json!({"type": "score", "score": score, "confidence": 0.9, "legend": {}, "probabilities": {}}),
        );
        assert_eq!(triage_answers(&p, &areas()).unwrap().risk.value, score);
    }
}

#[test]
fn non_finite_numbers_are_unexpected() {
    // JSON cannot carry NaN, so build the typed response by hand.
    let mut p = parsed("triage_response.json");
    p.response
        .answers
        .insert("agent_suitable".into(), Answer::Noul { noul: f64::NAN });
    match triage_answers(&p, &areas()).unwrap_err() {
        EngineError::UnexpectedAnswer { question, .. } => assert_eq!(question, "agent_suitable"),
        other => panic!("{other:?}"),
    }
}

// ------------------------------------------------------------ impact conversion

#[test]
fn impact_fixture_maps_names_back_to_journey_ids() {
    let p = parsed("impact_response.json");
    let js = journeys(2);
    let map = impact_answers(&p, &js).unwrap();
    assert_eq!(map.len(), 2);
    assert_eq!(map["shop.journey-00"], 0.93);
    assert_eq!(map["shop.journey-01"], 0.02);
}

#[test]
fn impact_conversion_checks_drift_presence_and_type() {
    let js = journeys(3);
    let p = parsed("impact_response.json");
    assert_eq!(
        impact_answers(&p, &js).unwrap_err(),
        EngineError::MissingAnswer("impact_2".into())
    );

    let mut p = parsed("impact_response.json");
    p.response.model = "jev-1.12.0".into();
    assert!(matches!(
        impact_answers(&p, &journeys(2)),
        Err(EngineError::ModelDrift { .. })
    ));

    let mut p = parsed("impact_response.json");
    p.response
        .answers
        .insert("impact_1".into(), Answer::Unsupported);
    assert!(matches!(
        impact_answers(&p, &journeys(2)),
        Err(EngineError::UnexpectedAnswer { question, .. }) if question == "impact_1"
    ));

    let mut p = parsed("impact_response.json");
    p.response
        .answers
        .insert("impact_0".into(), Answer::Noul { noul: 2.0 });
    assert!(matches!(
        impact_answers(&p, &journeys(2)),
        Err(EngineError::UnexpectedAnswer { .. })
    ));
}

#[test]
fn parsed_response_keeps_raw_answers() {
    let p = parsed("impact_response.json");
    assert_eq!(
        p.raw_answers,
        common::fixture_json("impact_response.json")["answers"]
    );
    assert!(matches!(
        ParsedResponse::from_slice(b"not json"),
        Err(EngineError::Transport(_))
    ));
    let _: &SystemOneResponse = &p.response;
}
