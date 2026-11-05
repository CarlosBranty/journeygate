use super::*;
use crate::config::parse_target;
use crate::ports::types::{Classified, HumanReason, Kind};
use HumanReason::*;

/// Answers that clear every calibrated check with the default thresholds.
fn good() -> TriageAnswers {
    TriageAnswers {
        model: "jev-1.13.0".into(),
        area: Classified {
            value: Area::new("catalog"),
            confidence: 0.9,
        },
        kind: Classified {
            value: Kind::Bugfix,
            confidence: 0.9,
        },
        risk: Classified {
            value: 0.5,
            confidence: 0.9,
        },
        clear_e2e_criteria: 0.9,
        touches_money_or_security: 0.05,
        agent_suitable: 0.9,
        changes_existing_flow: 0.05,
        raw: serde_json::json!({}),
    }
}

fn rules() -> GateRules {
    GateRules {
        max_fan_out_for_agent: 10,
        sensitive_areas: [Area::new("checkout"), Area::new("auth")]
            .into_iter()
            .collect(),
    }
}

fn human(reasons: &[HumanReason]) -> Lane {
    Lane::Human(reasons.to_vec())
}

fn decide_default(answers: &TriageAnswers, facts: &ImpactFacts) -> Lane {
    decide(answers, facts, &Thresholds::default(), &rules())
}

#[test]
fn clean_ticket_goes_to_the_agent() {
    assert_eq!(
        decide_default(&good(), &ImpactFacts::default()),
        Lane::Agent
    );
}

type Tweak = fn(&mut TriageAnswers);

#[test]
fn calibrated_checks_table_with_boundaries_and_nan() {
    let cases: Vec<(&str, Tweak, Lane)> = vec![
        // agent_suitable > 0.80
        (
            "agent_suitable exactly 0.80",
            |a| a.agent_suitable = 0.80,
            human(&[NotAgentSuitable]),
        ),
        (
            "agent_suitable just above",
            |a| a.agent_suitable = 0.800_001,
            Lane::Agent,
        ),
        (
            "agent_suitable 1.0",
            |a| a.agent_suitable = 1.0,
            Lane::Agent,
        ),
        (
            "agent_suitable 0.0",
            |a| a.agent_suitable = 0.0,
            human(&[NotAgentSuitable]),
        ),
        (
            "agent_suitable NaN",
            |a| a.agent_suitable = f64::NAN,
            human(&[NotAgentSuitable]),
        ),
        // risk.score < 1.0
        (
            "risk exactly 1.0",
            |a| a.risk.value = 1.0,
            human(&[RiskTooHigh]),
        ),
        (
            "risk just below 1.0",
            |a| a.risk.value = 0.999_999,
            Lane::Agent,
        ),
        ("risk 0.0", |a| a.risk.value = 0.0, Lane::Agent),
        ("risk 4.0", |a| a.risk.value = 4.0, human(&[RiskTooHigh])),
        (
            "risk NaN",
            |a| a.risk.value = f64::NAN,
            human(&[RiskTooHigh]),
        ),
        // touches_money_or_security < 0.15
        (
            "money exactly 0.15",
            |a| a.touches_money_or_security = 0.15,
            human(&[MoneyOrSecurity]),
        ),
        (
            "money just below",
            |a| a.touches_money_or_security = 0.149_999,
            Lane::Agent,
        ),
        (
            "money 0.0",
            |a| a.touches_money_or_security = 0.0,
            Lane::Agent,
        ),
        (
            "money 1.0",
            |a| a.touches_money_or_security = 1.0,
            human(&[MoneyOrSecurity]),
        ),
        (
            "money NaN",
            |a| a.touches_money_or_security = f64::NAN,
            human(&[MoneyOrSecurity]),
        ),
        // clear_e2e_criteria > 0.70
        (
            "clear exactly 0.70",
            |a| a.clear_e2e_criteria = 0.70,
            human(&[UnclearCriteria]),
        ),
        (
            "clear just above",
            |a| a.clear_e2e_criteria = 0.700_001,
            Lane::Agent,
        ),
        (
            "clear NaN",
            |a| a.clear_e2e_criteria = f64::NAN,
            human(&[UnclearCriteria]),
        ),
        // changes_existing_flow < 0.20
        (
            "flow exactly 0.20",
            |a| a.changes_existing_flow = 0.20,
            human(&[ChangesExistingFlow]),
        ),
        (
            "flow just below",
            |a| a.changes_existing_flow = 0.199_999,
            Lane::Agent,
        ),
        (
            "flow 1.0",
            |a| a.changes_existing_flow = 1.0,
            human(&[ChangesExistingFlow]),
        ),
        (
            "flow NaN",
            |a| a.changes_existing_flow = f64::NAN,
            human(&[ChangesExistingFlow]),
        ),
        // confidences > 0.75 (Choice and Score only)
        (
            "area confidence exactly 0.75",
            |a| a.area.confidence = 0.75,
            human(&[LowConfidence]),
        ),
        (
            "kind confidence exactly 0.75",
            |a| a.kind.confidence = 0.75,
            human(&[LowConfidence]),
        ),
        (
            "risk confidence exactly 0.75",
            |a| a.risk.confidence = 0.75,
            human(&[LowConfidence]),
        ),
        (
            "area confidence just above",
            |a| a.area.confidence = 0.750_001,
            Lane::Agent,
        ),
        (
            "all confidences 1.0",
            |a| {
                a.area.confidence = 1.0;
                a.kind.confidence = 1.0;
                a.risk.confidence = 1.0;
            },
            Lane::Agent,
        ),
        (
            "area confidence NaN",
            |a| a.area.confidence = f64::NAN,
            human(&[LowConfidence]),
        ),
        (
            "kind confidence NaN",
            |a| a.kind.confidence = f64::NAN,
            human(&[LowConfidence]),
        ),
        (
            "risk confidence NaN",
            |a| a.risk.confidence = f64::NAN,
            human(&[LowConfidence]),
        ),
        (
            "two low confidences give one reason",
            |a| {
                a.area.confidence = 0.1;
                a.risk.confidence = 0.1;
            },
            human(&[LowConfidence]),
        ),
        // kind and sensitive areas
        (
            "architecture kind",
            |a| a.kind.value = Kind::Architecture,
            human(&[ArchitecturalKind]),
        ),
        (
            "minor feature",
            |a| a.kind.value = Kind::MinorFeature,
            Lane::Agent,
        ),
        ("refactor", |a| a.kind.value = Kind::Refactor, Lane::Agent),
        (
            "sensitive area",
            |a| a.area.value = Area::new("checkout"),
            human(&[SensitiveArea]),
        ),
        (
            "other sensitive area",
            |a| a.area.value = Area::new("auth"),
            human(&[SensitiveArea]),
        ),
        (
            "area match is exact",
            |a| a.area.value = Area::new("Checkout"),
            Lane::Agent,
        ),
        (
            "empty area key",
            |a| a.area.value = Area::new(""),
            Lane::Agent,
        ),
        (
            "everything NaN",
            |a| {
                a.area.confidence = f64::NAN;
                a.kind.confidence = f64::NAN;
                a.risk.confidence = f64::NAN;
                a.risk.value = f64::NAN;
                a.touches_money_or_security = f64::NAN;
                a.agent_suitable = f64::NAN;
                a.clear_e2e_criteria = f64::NAN;
                a.changes_existing_flow = f64::NAN;
            },
            human(&[
                LowConfidence,
                RiskTooHigh,
                MoneyOrSecurity,
                NotAgentSuitable,
                UnclearCriteria,
                ChangesExistingFlow,
            ]),
        ),
    ];
    for (name, tweak, expected) in cases {
        let mut answers = good();
        tweak(&mut answers);
        assert_eq!(
            decide_default(&answers, &ImpactFacts::default()),
            expected,
            "{name}"
        );
    }
}

#[test]
fn hard_rules_table() {
    let cases: Vec<(&str, ImpactFacts, Lane)> = vec![
        ("no facts", ImpactFacts::default(), Lane::Agent),
        (
            "human-only path",
            ImpactFacts {
                human_only_path_hit: true,
                human_only_paths: vec!["infra/main.tf".into()],
                ..ImpactFacts::default()
            },
            human(&[HardRulePath]),
        ),
        (
            "schema migration",
            ImpactFacts {
                schema_migration: true,
                ..ImpactFacts::default()
            },
            human(&[SchemaMigration]),
        ),
        (
            "fan-out exactly at the limit",
            ImpactFacts {
                max_fan_out: 10,
                ..ImpactFacts::default()
            },
            Lane::Agent,
        ),
        (
            "fan-out one over the limit",
            ImpactFacts {
                max_fan_out: 11,
                ..ImpactFacts::default()
            },
            human(&[FanOutTooWide]),
        ),
        (
            "fan-out u32::MAX",
            ImpactFacts {
                max_fan_out: u32::MAX,
                ..ImpactFacts::default()
            },
            human(&[FanOutTooWide]),
        ),
    ];
    for (name, facts, expected) in cases {
        assert_eq!(decide_default(&good(), &facts), expected, "{name}");
    }
}

#[test]
fn hard_rules_cannot_be_overridden_by_perfect_answers() {
    let mut perfect = good();
    perfect.agent_suitable = 1.0;
    perfect.touches_money_or_security = 0.0;
    perfect.changes_existing_flow = 0.0;
    perfect.risk.value = 0.0;
    let facts = ImpactFacts {
        human_only_path_hit: true,
        ..ImpactFacts::default()
    };
    assert_eq!(decide_default(&perfect, &facts), human(&[HardRulePath]));
}

#[test]
fn every_reason_in_table_order() {
    let answers = TriageAnswers {
        area: Classified {
            value: Area::new("checkout"),
            confidence: 0.1,
        },
        kind: Classified {
            value: Kind::Architecture,
            confidence: 0.9,
        },
        risk: Classified {
            value: 3.0,
            confidence: 0.9,
        },
        clear_e2e_criteria: 0.1,
        touches_money_or_security: 0.9,
        agent_suitable: 0.1,
        changes_existing_flow: 0.9,
        ..good()
    };
    let facts = ImpactFacts {
        max_fan_out: 50,
        human_only_path_hit: true,
        schema_migration: true,
        ..ImpactFacts::default()
    };
    assert_eq!(
        decide_default(&answers, &facts),
        human(&[
            HardRulePath,
            SchemaMigration,
            FanOutTooWide,
            SensitiveArea,
            ArchitecturalKind,
            LowConfidence,
            RiskTooHigh,
            MoneyOrSecurity,
            NotAgentSuitable,
            UnclearCriteria,
            ChangesExistingFlow,
        ])
    );
}

#[test]
fn custom_thresholds_are_honored() {
    let strict = Thresholds {
        agent_suitable_min: 0.95,
        ..Thresholds::default()
    };
    assert_eq!(
        decide(&good(), &ImpactFacts::default(), &strict, &rules()),
        human(&[NotAgentSuitable])
    );

    // Degenerate thresholds: nothing can pass a `> 1.0` or `< 0.0` check.
    let impossible = Thresholds {
        agent_suitable_min: 1.0,
        money_or_security_max: 0.0,
        ..Thresholds::default()
    };
    let mut answers = good();
    answers.agent_suitable = 1.0;
    answers.touches_money_or_security = 0.0;
    assert_eq!(
        decide(&answers, &ImpactFacts::default(), &impossible, &rules()),
        human(&[MoneyOrSecurity, NotAgentSuitable])
    );

    // NaN thresholds fail closed too.
    let nan = Thresholds {
        confidence_min: f64::NAN,
        risk_score_max: f64::NAN,
        ..Thresholds::default()
    };
    assert_eq!(
        decide(&good(), &ImpactFacts::default(), &nan, &rules()),
        human(&[LowConfidence, RiskTooHigh])
    );
}

#[test]
fn zero_fan_out_limit_and_no_sensitive_areas() {
    let rules = GateRules {
        max_fan_out_for_agent: 0,
        sensitive_areas: BTreeSet::new(),
    };
    let mut answers = good();
    answers.area.value = Area::new("checkout");
    let t = Thresholds::default();
    assert_eq!(
        decide(&answers, &ImpactFacts::default(), &t, &rules),
        Lane::Agent
    );
    let facts = ImpactFacts {
        max_fan_out: 1,
        ..ImpactFacts::default()
    };
    assert_eq!(
        decide(&answers, &facts, &t, &rules),
        human(&[FanOutTooWide])
    );
}

#[test]
fn decide_without_engine_table() {
    let cases: Vec<(&str, ImpactFacts, Lane)> = vec![
        (
            "no facts",
            ImpactFacts::default(),
            human(&[EngineUnavailable]),
        ),
        (
            "fan-out at the limit",
            ImpactFacts {
                max_fan_out: 10,
                ..ImpactFacts::default()
            },
            human(&[EngineUnavailable]),
        ),
        (
            "every hard rule",
            ImpactFacts {
                max_fan_out: 11,
                human_only_path_hit: true,
                schema_migration: true,
                ..ImpactFacts::default()
            },
            human(&[
                HardRulePath,
                SchemaMigration,
                FanOutTooWide,
                EngineUnavailable,
            ]),
        ),
        (
            "schema migration only",
            ImpactFacts {
                schema_migration: true,
                ..ImpactFacts::default()
            },
            human(&[SchemaMigration, EngineUnavailable]),
        ),
    ];
    for (name, facts, expected) in cases {
        assert_eq!(decide_without_engine(&facts, &rules()), expected, "{name}");
    }
}

const TARGET: &str = r#"
version = 1
[app]
name = "shop"
base_url = "http://localhost:3000"
[testability]
roles = ["customer"]
[hard_rules]
human_only_paths = ["infra/**"]
max_fan_out_for_agent = 7
[areas.checkout]
description = "Checkout and payments"
paths = ["web/checkout/**"]
sensitive = true
[areas.auth]
description = "Login"
paths = ["web/auth/**"]
sensitive = true
[areas.catalog]
description = "Product catalog"
paths = ["web/catalog/**"]
"#;

#[test]
fn rules_from_config() {
    let config = parse_target(TARGET).unwrap();
    let rules = GateRules::from_config(&config);
    assert_eq!(rules.max_fan_out_for_agent, 7);
    assert_eq!(
        rules.sensitive_areas,
        [Area::new("auth"), Area::new("checkout")]
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn rules_from_config_defaults_and_out_of_range_limits() {
    let minimal = r#"
version = 1
[app]
name = "shop"
base_url = "http://localhost:3000"
[testability]
roles = ["customer"]
[areas.catalog]
description = "Catalog"
paths = ["web/**"]
"#;
    let mut config = parse_target(minimal).unwrap();
    let rules = GateRules::from_config(&config);
    assert_eq!(rules.max_fan_out_for_agent, 10);
    assert!(rules.sensitive_areas.is_empty());

    // Negative limits fail closed (any fan-out above zero is too wide).
    config.hard_rules.max_fan_out_for_agent = -1;
    assert_eq!(GateRules::from_config(&config).max_fan_out_for_agent, 0);
    // Limits beyond u32 saturate.
    config.hard_rules.max_fan_out_for_agent = i64::MAX;
    assert_eq!(
        GateRules::from_config(&config).max_fan_out_for_agent,
        u32::MAX
    );
}
