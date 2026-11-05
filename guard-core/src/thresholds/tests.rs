use super::*;

#[test]
fn defaults_match_the_contract() {
    let t = Thresholds::default();
    assert_eq!(t.version, 1);
    assert_eq!(t.mode, Mode::Shadow);
    assert_eq!(t.agent_suitable_min, 0.80);
    assert_eq!(t.risk_score_max, 1.0);
    assert_eq!(t.money_or_security_max, 0.15);
    assert_eq!(t.clear_criteria_min, 0.70);
    assert_eq!(t.confidence_min, 0.75);
    assert_eq!(t.existing_flow_max, 0.20);
    assert_eq!(t.impact_threshold, 0.25);
    assert!(validate(&t).is_ok());
}

#[test]
fn empty_and_comment_only_documents_yield_the_defaults() {
    assert_eq!(parse_thresholds("").unwrap(), Thresholds::default());
    assert_eq!(
        parse_thresholds("\n# only a comment\n").unwrap(),
        Thresholds::default()
    );
}

#[test]
fn contract_example_parses() {
    let text = r#"
version = 1
mode = "shadow"

agent_suitable_min = 0.80
risk_score_max = 1.0
money_or_security_max = 0.15
clear_criteria_min = 0.70
confidence_min = 0.75
existing_flow_max = 0.20
impact_threshold = 0.25
"#;
    assert_eq!(parse_thresholds(text).unwrap(), Thresholds::default());
}

#[test]
fn every_key_overrides_its_default() {
    let text = r#"
mode = "active"
agent_suitable_min = 0.9
risk_score_max = 2.5
money_or_security_max = 0.05
clear_criteria_min = 0.6
confidence_min = 0.8
existing_flow_max = 0.1
impact_threshold = 0.4
"#;
    let t = parse_thresholds(text).unwrap();
    assert_eq!(
        t,
        Thresholds {
            version: 1,
            mode: Mode::Active,
            agent_suitable_min: 0.9,
            risk_score_max: 2.5,
            money_or_security_max: 0.05,
            clear_criteria_min: 0.6,
            confidence_min: 0.8,
            existing_flow_max: 0.1,
            impact_threshold: 0.4,
        }
    );
    assert!(!t.mode.is_shadow());
}

#[test]
fn partial_document_keeps_the_other_defaults() {
    let t = parse_thresholds("confidence_min = 0.9").unwrap();
    assert_eq!(t.confidence_min, 0.9);
    assert_eq!(t.mode, Mode::Shadow);
    assert_eq!(t.agent_suitable_min, 0.80);
}

#[test]
fn integers_are_accepted_where_floats_are_expected() {
    let t = parse_thresholds("risk_score_max = 1\nagent_suitable_min = 1\nimpact_threshold = 0")
        .unwrap();
    assert_eq!(t.risk_score_max, 1.0);
    assert_eq!(t.agent_suitable_min, 1.0);
    assert_eq!(t.impact_threshold, 0.0);
}

#[test]
fn parse_errors() {
    for text in [
        "unknown_key = 1",
        "mode = \"passive\"",
        "mode = 1",
        "version = \"1\"",
        "version = 1.0",
        "confidence_min = \"high\"",
        "confidence_min = ",
        "[section]\nx = 1",
    ] {
        let err = parse_thresholds(text).unwrap_err();
        assert!(
            matches!(err, ThresholdsError::Parse(_)),
            "{text:?} -> {err:?}"
        );
        assert!(err.to_string().starts_with("thresholds.toml: "), "{err}");
    }
}

fn invalid(field: &str, message: &str) -> ThresholdsError {
    ThresholdsError::Invalid {
        field: field.into(),
        message: message.into(),
    }
}

#[test]
fn probability_range_rules_table() {
    let fields = [
        "agent_suitable_min",
        "money_or_security_max",
        "clear_criteria_min",
        "confidence_min",
        "existing_flow_max",
        "impact_threshold",
    ];
    for field in fields {
        for ok in ["0.0", "0", "1.0", "1", "0.5", "0.15", "0.8", "-0.0"] {
            assert!(
                parse_thresholds(&format!("{field} = {ok}")).is_ok(),
                "{field} = {ok}"
            );
        }
        for bad in ["-0.0001", "1.0001", "2", "-1"] {
            assert_eq!(
                parse_thresholds(&format!("{field} = {bad}")).unwrap_err(),
                invalid(field, "must be between 0.0 and 1.0"),
                "{field} = {bad}"
            );
        }
        for non_finite in ["nan", "+nan", "-nan", "inf", "+inf", "-inf"] {
            assert_eq!(
                parse_thresholds(&format!("{field} = {non_finite}")).unwrap_err(),
                invalid(field, "must be finite"),
                "{field} = {non_finite}"
            );
        }
    }
}

#[test]
fn risk_score_range_rules_table() {
    for ok in ["0.0", "1.0", "4.0", "4", "3.999"] {
        assert!(
            parse_thresholds(&format!("risk_score_max = {ok}")).is_ok(),
            "{ok}"
        );
    }
    for bad in ["-0.1", "4.0001", "5"] {
        assert_eq!(
            parse_thresholds(&format!("risk_score_max = {bad}")).unwrap_err(),
            invalid("risk_score_max", "must be between 0.0 and 4.0"),
            "{bad}"
        );
    }
    for non_finite in ["nan", "inf", "-inf"] {
        assert_eq!(
            parse_thresholds(&format!("risk_score_max = {non_finite}")).unwrap_err(),
            invalid("risk_score_max", "must be finite")
        );
    }
}

#[test]
fn version_must_be_one() {
    assert!(parse_thresholds("version = 1").is_ok());
    for bad in ["0", "2", "-1"] {
        assert_eq!(
            parse_thresholds(&format!("version = {bad}")).unwrap_err(),
            invalid("version", "must be 1")
        );
    }
}

#[test]
fn validate_reports_the_first_violation_in_field_order() {
    let t = Thresholds {
        version: 3,
        agent_suitable_min: f64::NAN,
        ..Thresholds::default()
    };
    assert_eq!(validate(&t).unwrap_err(), invalid("version", "must be 1"));

    let t = Thresholds {
        agent_suitable_min: 7.0,
        impact_threshold: f64::INFINITY,
        ..Thresholds::default()
    };
    assert_eq!(
        validate(&t).unwrap_err(),
        invalid("agent_suitable_min", "must be between 0.0 and 1.0")
    );

    let t = Thresholds {
        impact_threshold: f64::NEG_INFINITY,
        ..Thresholds::default()
    };
    assert_eq!(
        validate(&t).unwrap_err(),
        invalid("impact_threshold", "must be finite")
    );
}

#[test]
fn invalid_error_display() {
    assert_eq!(
        invalid("confidence_min", "must be finite").to_string(),
        "thresholds.toml: confidence_min: must be finite"
    );
}

#[test]
fn json_round_trip_uses_lowercase_mode() {
    let t = Thresholds {
        mode: Mode::Active,
        ..Thresholds::default()
    };
    let json = serde_json::to_value(&t).unwrap();
    assert_eq!(json["mode"], "active");
    let back: Thresholds = serde_json::from_value(json).unwrap();
    assert_eq!(back, t);
}
