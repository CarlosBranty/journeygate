use super::*;
use crate::catalog::parse_catalog;

const CATALOG: &str = r#"{
  "version": 1,
  "journeys": [
    {"id": "checkout.customer-card", "area": "checkout", "title": "t", "impact_question": "q",
     "paths": ["web/src/checkout/**", "services/orders/**"], "smoke": true, "quarantined": false},
    {"id": "billing.invoice-pdf", "area": "billing", "title": "t", "impact_question": "q",
     "paths": ["services/billing/*"], "smoke": false, "quarantined": false},
    {"id": "auth.login", "area": "auth", "title": "t", "impact_question": "q",
     "paths": [], "smoke": true, "quarantined": false},
    {"id": "inventory.restock", "area": "inventory", "title": "t", "impact_question": "q",
     "paths": ["services/inventory/**"], "smoke": false, "quarantined": true}
  ]
}"#;

fn catalog() -> Catalog {
    parse_catalog(CATALOG).unwrap()
}

fn files(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn tags(selection: &Selection) -> Vec<&str> {
    selection.tags.iter().map(String::as_str).collect()
}

#[test]
fn smoke_is_always_selected() {
    let selection = select_affected(&catalog(), &[], None).unwrap();
    assert_eq!(tags(&selection), vec!["@smoke"]);
}

#[test]
fn smoke_journeys_are_listed_with_a_smoke_reason() {
    let selection = select_affected(&catalog(), &[], None).unwrap();
    let ids: Vec<&str> = selection.journeys.iter().map(|j| j.id.as_str()).collect();
    assert_eq!(ids, vec!["auth.login", "checkout.customer-card"]);
    assert_eq!(selection.journeys[0].reasons, vec![Reason::Smoke]);
    assert_eq!(selection.journeys[0].tag, "@journey:auth.login");
}

#[test]
fn ticket_adds_a_ticket_tag() {
    let selection = select_affected(&catalog(), &[], Some("SHOP-123")).unwrap();
    assert_eq!(tags(&selection), vec!["@smoke", "@ticket:SHOP-123"]);
}

#[test]
fn changed_files_select_journeys_by_path() {
    let changed = files(&["README.md", "services/billing/invoice.go"]);
    let selection = select_affected(&catalog(), &changed, None).unwrap();
    assert_eq!(
        tags(&selection),
        vec!["@journey:billing.invoice-pdf", "@smoke"]
    );
    let billing = selection
        .journeys
        .iter()
        .find(|j| j.id == "billing.invoice-pdf")
        .unwrap();
    assert_eq!(
        billing.reasons,
        vec![Reason::Path {
            file: "services/billing/invoice.go".into(),
            glob: "services/billing/*".into()
        }]
    );
}

#[test]
fn single_star_does_not_cross_directories() {
    let changed = files(&["services/billing/pdf/render.go"]);
    let selection = select_affected(&catalog(), &changed, None).unwrap();
    assert!(!selection.tags.contains("@journey:billing.invoice-pdf"));
}

#[test]
fn one_reason_per_matching_file_with_the_first_matching_glob_then_smoke() {
    let changed = files(&[
        "web/src/checkout/cart.ts",
        "README.md",
        "services/orders/a.go",
        "web/src/checkout/pay.ts",
    ]);
    let selection = select_affected(&catalog(), &changed, None).unwrap();
    let checkout = selection
        .journeys
        .iter()
        .find(|j| j.id == "checkout.customer-card")
        .unwrap();
    assert_eq!(
        checkout.reasons,
        vec![
            Reason::Path {
                file: "web/src/checkout/cart.ts".into(),
                glob: "web/src/checkout/**".into()
            },
            Reason::Path {
                file: "services/orders/a.go".into(),
                glob: "services/orders/**".into()
            },
            Reason::Path {
                file: "web/src/checkout/pay.ts".into(),
                glob: "web/src/checkout/**".into()
            },
            Reason::Smoke,
        ]
    );
    assert!(selection.tags.contains("@journey:checkout.customer-card"));
}

#[test]
fn quarantined_journeys_are_never_selected_by_affected() {
    let changed = files(&["services/inventory/stock.go"]);
    let selection = select_affected(&catalog(), &changed, None).unwrap();
    assert!(!selection.tags.contains("@journey:inventory.restock"));
    assert!(
        selection
            .journeys
            .iter()
            .all(|j| j.id != "inventory.restock")
    );
}

/// `@smoke` still reaches a quarantined smoke journey's tests; Playwright drops
/// them with `grepInvert` (e2e-playwright/src/config/env.ts). guard's part is to
/// never list it nor add its `@journey:` tag, so `--journey <id>` is the only way in.
#[test]
fn quarantined_smoke_journeys_are_neither_listed_nor_tagged() {
    let catalog = parse_catalog(
        r#"{"version": 1, "journeys": [
          {"id": "checkout.card", "area": "checkout", "title": "t", "impact_question": "q",
           "paths": ["web/**"], "smoke": true, "quarantined": true}
        ]}"#,
    )
    .unwrap();
    let changed = files(&["web/a.ts"]);
    for selection in [
        select_affected(&catalog, &changed, None).unwrap(),
        select_all(&catalog),
    ] {
        assert_eq!(tags(&selection), vec!["@smoke"]);
        assert!(selection.journeys.is_empty());
    }
    let requested = select_journeys(&catalog, &["checkout.card".into()]).unwrap();
    assert_eq!(tags(&requested), vec!["@journey:checkout.card"]);
}

#[test]
fn e2e_tags_are_sorted_and_comma_separated() {
    let changed = files(&["services/billing/x.go"]);
    let selection = select_affected(&catalog(), &changed, Some("SHOP-9")).unwrap();
    assert_eq!(
        selection.e2e_tags(),
        "@journey:billing.invoice-pdf,@smoke,@ticket:SHOP-9"
    );
}

#[test]
fn reasons_serialize_with_a_kind_field() {
    let path = Reason::Path {
        file: "a".into(),
        glob: "b".into(),
    };
    assert_eq!(
        serde_json::to_string(&path).unwrap(),
        r#"{"kind":"path","file":"a","glob":"b"}"#
    );
    assert_eq!(
        serde_json::to_string(&Reason::Smoke).unwrap(),
        r#"{"kind":"smoke"}"#
    );
}

#[test]
fn select_all_takes_smoke_and_every_active_journey() {
    let selection = select_all(&catalog());
    assert_eq!(
        tags(&selection),
        vec![
            "@journey:auth.login",
            "@journey:billing.invoice-pdf",
            "@journey:checkout.customer-card",
            "@smoke"
        ]
    );
    assert_eq!(selection.journeys.len(), 3);
    assert!(
        selection
            .journeys
            .iter()
            .all(|j| j.reasons == vec![Reason::All])
    );
}

#[test]
fn select_journeys_allows_quarantined_and_rejects_unknown_ids() {
    let ids = files(&["inventory.restock", "auth.login", "auth.login"]);
    let selection = select_journeys(&catalog(), &ids).unwrap();
    assert_eq!(
        tags(&selection),
        vec!["@journey:auth.login", "@journey:inventory.restock"]
    );
    assert_eq!(selection.journeys[0].reasons, vec![Reason::Requested]);

    let error = select_journeys(&catalog(), &files(&["nope.x"])).unwrap_err();
    assert_eq!(error, RoutingError::UnknownJourney("nope.x".into()));
    assert_eq!(error.to_string(), "unknown journey id: nope.x");
}

// ------------------------------------------------ Phase 2: impact (ports.md)

fn impact(pairs: &[(&str, f64)]) -> std::collections::BTreeMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn impact_above_the_threshold_selects_a_journey() {
    let map = impact(&[("billing.invoice-pdf", 0.81), ("auth.login", 0.25)]);
    let selection = select_affected_with_impact(&catalog(), &[], Some(&map), 0.25, None).unwrap();
    assert_eq!(
        tags(&selection),
        vec!["@journey:billing.invoice-pdf", "@smoke"]
    );
    let billing = selection
        .journeys
        .iter()
        .find(|j| j.id == "billing.invoice-pdf")
        .unwrap();
    assert_eq!(billing.reasons, vec![Reason::Impact { probability: 0.81 }]);
    // Exactly at the threshold is not selected (strict comparison).
    let auth = selection
        .journeys
        .iter()
        .find(|j| j.id == "auth.login")
        .unwrap();
    assert_eq!(auth.reasons, vec![Reason::Smoke]);
}

#[test]
fn impact_reasons_come_after_path_reasons_and_before_smoke() {
    let map = impact(&[("checkout.customer-card", 0.9)]);
    let changed = files(&["web/src/checkout/a.ts"]);
    let selection =
        select_affected_with_impact(&catalog(), &changed, Some(&map), 0.25, None).unwrap();
    let checkout = selection
        .journeys
        .iter()
        .find(|j| j.id == "checkout.customer-card")
        .unwrap();
    assert_eq!(
        checkout.reasons,
        vec![
            Reason::Path {
                file: "web/src/checkout/a.ts".into(),
                glob: "web/src/checkout/**".into()
            },
            Reason::Impact { probability: 0.9 },
            Reason::Smoke,
        ]
    );
}

#[test]
fn impact_never_selects_quarantined_or_unknown_journeys_and_nan_fails_open_to_nothing() {
    let map = impact(&[
        ("inventory.restock", 0.99),
        ("ghost.journey", 0.99),
        ("billing.invoice-pdf", f64::NAN),
    ]);
    let selection = select_affected_with_impact(&catalog(), &[], Some(&map), 0.25, None).unwrap();
    assert_eq!(tags(&selection), vec!["@smoke"]);
}

#[test]
fn engine_unavailable_selects_every_active_journey() {
    let selection = select_affected_with_impact(&catalog(), &[], None, 0.25, Some("X-1")).unwrap();
    assert_eq!(
        tags(&selection),
        vec![
            "@journey:auth.login",
            "@journey:billing.invoice-pdf",
            "@journey:checkout.customer-card",
            "@smoke",
            "@ticket:X-1"
        ]
    );
    let billing = selection
        .journeys
        .iter()
        .find(|j| j.id == "billing.invoice-pdf")
        .unwrap();
    assert_eq!(billing.reasons, vec![Reason::EngineUnavailable]);
}

#[test]
fn new_reasons_serialize_with_snake_case_kinds() {
    assert_eq!(
        serde_json::to_value(Reason::Impact { probability: 0.5 }).unwrap(),
        serde_json::json!({"kind": "impact", "probability": 0.5})
    );
    assert_eq!(
        serde_json::to_value(Reason::EngineUnavailable).unwrap(),
        serde_json::json!({"kind": "engine_unavailable"})
    );
}

#[test]
fn a_subtree_entry_selects_every_journey_with_globs_under_it() {
    let changed = files(&["services/", "web/src/checkout/cart.ts"]);
    let selection = select_affected(&catalog(), &changed, None).unwrap();
    assert_eq!(
        tags(&selection),
        vec![
            "@journey:billing.invoice-pdf",
            "@journey:checkout.customer-card",
            "@smoke"
        ]
    );
    let billing = selection
        .journeys
        .iter()
        .find(|j| j.id == "billing.invoice-pdf")
        .unwrap();
    assert_eq!(
        billing.reasons,
        vec![Reason::Path {
            file: "services/".into(),
            glob: "services/billing/*".into()
        }]
    );
    let unrelated = select_affected(&catalog(), &files(&["docs/"]), None).unwrap();
    assert_eq!(tags(&unrelated), vec!["@smoke"]);
}
