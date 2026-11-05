use super::*;
use crate::config::{ValidationErrorKind, parse_target};

pub(crate) const EXAMPLE: &str = r#"{
  "version": 1,
  "journeys": [
    {
      "id": "checkout.customer-card",
      "area": "checkout",
      "title": "Customer pays by card and gets one confirmed order",
      "impact_question": "This diff can change checkout.",
      "paths": ["web/src/checkout/**", "services/orders/**"],
      "smoke": true,
      "quarantined": false
    },
    {
      "id": "billing.invoice-pdf",
      "area": "billing",
      "title": "Invoice PDF is issued after a paid order",
      "impact_question": "This diff can change invoices.",
      "paths": ["services/billing/**"],
      "smoke": false,
      "quarantined": false
    },
    {
      "id": "inventory.restock",
      "area": "inventory",
      "title": "Restock",
      "impact_question": "This diff can change restock.",
      "paths": ["services/inventory/**"],
      "smoke": false,
      "quarantined": true
    }
  ]
}"#;

const TARGET: &str = r#"
version = 1
[app]
name = "shop"
base_url = "http://localhost:3000"
[areas.checkout]
description = "Checkout"
paths = ["web/src/checkout/**"]
[areas.billing]
description = "Billing"
paths = ["services/billing/**"]
"#;

fn journey_json(id: &str, extra: &str) -> String {
    format!(
        r#"{{"version": 1, "journeys": [{{"id": "{id}", "area": "checkout", "title": "t", "impact_question": "q", "paths": [], "smoke": false, "quarantined": false{extra}}}]}}"#
    )
}

fn invalid(text: &str) -> Vec<crate::config::ValidationError> {
    match parse_catalog(text) {
        Err(CatalogError::Invalid(errors)) => errors,
        other => panic!("expected validation errors, got {other:?}"),
    }
}

#[test]
fn parses_the_example_catalog() {
    let catalog = parse_catalog(EXAMPLE).unwrap();
    assert_eq!(catalog.journeys.len(), 3);
    assert_eq!(catalog.quarantined_count(), 1);
    let active: Vec<&str> = catalog.active().map(|j| j.id.as_str()).collect();
    assert_eq!(
        active,
        vec!["checkout.customer-card", "billing.invoice-pdf"]
    );
    assert!(catalog.find("inventory.restock").is_some());
    assert!(catalog.find("nope.nope").is_none());
}

#[test]
fn builds_journey_and_area_tags() {
    let catalog = parse_catalog(EXAMPLE).unwrap();
    let journey = catalog.find("checkout.customer-card").unwrap();
    assert_eq!(journey.tag(), "@journey:checkout.customer-card");
    assert_eq!(journey.area_tag(), "@area:checkout");
    assert_eq!(journey_tag("a.b"), "@journey:a.b");
}

#[test]
fn accepts_an_optional_schema_key_and_an_empty_list() {
    let catalog =
        parse_catalog(r#"{"$schema": "../x.json", "version": 1, "journeys": []}"#).unwrap();
    assert!(catalog.journeys.is_empty());
    assert_eq!(catalog.schema.as_deref(), Some("../x.json"));
}

#[test]
fn rejects_unknown_keys_and_bad_json() {
    assert!(matches!(
        parse_catalog(r#"{"version": 1, "journeys": [], "extra": 1}"#),
        Err(CatalogError::Parse(_))
    ));
    assert!(matches!(
        parse_catalog(&journey_json("a.b", r#", "owner": "x""#)),
        Err(CatalogError::Parse(_))
    ));
    assert!(matches!(parse_catalog("{"), Err(CatalogError::Parse(_))));
}

#[test]
fn rejects_unsupported_versions() {
    let errors = invalid(r#"{"version": 2, "journeys": []}"#);
    assert_eq!(errors[0].kind, ValidationErrorKind::UnsupportedVersion(2));
}

#[test]
fn journey_ids_need_two_lowercase_segments() {
    for bad in [
        "checkout",
        "Checkout.card",
        "check_out.card",
        "checkout.card_x",
    ] {
        let errors = invalid(&journey_json(bad, ""));
        assert_eq!(errors[0].field, "journeys[0].id", "{bad}");
    }
    assert!(parse_catalog(&journey_json("checkout.customer-card", "")).is_ok());
    assert!(parse_catalog(&journey_json("a1.b-2.c", "")).is_ok());
}

#[test]
fn title_and_question_must_not_be_empty() {
    let text = journey_json("a.b", "").replace(r#""title": "t""#, r#""title": """#);
    let errors = invalid(&text);
    assert_eq!(errors[0].field, "journeys[a.b].title");
    assert_eq!(errors[0].kind, ValidationErrorKind::Empty);
}

#[test]
fn paths_must_be_unique_valid_globs() {
    let text = journey_json("a.b", "").replace(r#""paths": []"#, r#""paths": ["x/**", "x/**"]"#);
    assert_eq!(
        invalid(&text)[0].kind,
        ValidationErrorKind::Duplicate("x/**".into())
    );
    let text = journey_json("a.b", "").replace(r#""paths": []"#, r#""paths": ["x/[a"]"#);
    assert!(matches!(
        invalid(&text)[0].kind,
        ValidationErrorKind::InvalidGlob { .. }
    ));
}

#[test]
fn check_against_config_reports_unknown_areas_and_duplicate_ids() {
    let config = parse_target(TARGET).unwrap();
    let mut catalog = parse_catalog(EXAMPLE).unwrap();
    catalog.journeys.push(catalog.journeys[0].clone());
    let issues = check_against_config(&catalog, &config);
    assert_eq!(
        issues,
        vec![
            CatalogIssue::UnknownArea {
                journey: "inventory.restock".into(),
                area: "inventory".into()
            },
            CatalogIssue::DuplicateId("checkout.customer-card".into()),
        ]
    );
    assert_eq!(
        issues[0].to_string(),
        "journey inventory.restock: area inventory is not a key of [areas] in target.toml"
    );
    assert_eq!(
        issues[1].to_string(),
        "duplicate journey id checkout.customer-card"
    );
}

#[test]
fn lint_reports_dead_catalog_globs() {
    let catalog = parse_catalog(EXAMPLE).unwrap();
    let tracked = vec![
        "web/src/checkout/cart.ts".to_string(),
        "services/billing/a.go".to_string(),
    ];
    let warnings: Vec<String> = lint(&catalog, &tracked)
        .iter()
        .map(|w| w.to_string())
        .collect();
    assert_eq!(
        warnings,
        vec![
            "dead glob in journeys[checkout.customer-card].paths: services/orders/**",
            "dead glob in journeys[inventory.restock].paths: services/inventory/**",
        ]
    );
}
