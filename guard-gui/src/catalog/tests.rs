use super::*;
use std::path::Path;

const FIXTURE: &str = include_str!("../../tests/fixtures/journeys.json");

#[test]
fn reads_the_journey_rows_of_the_catalog() {
    let rows = parse_rows(FIXTURE).unwrap();
    assert_eq!(rows.len(), 2);
    let card = &rows[0];
    assert_eq!(card.id, "checkout.customer-card");
    assert_eq!(card.area, "checkout");
    assert_eq!(
        card.title,
        "Customer pays by card and gets one confirmed order"
    );
    assert_eq!(
        card.paths,
        vec!["web/src/checkout/**", "services/payments/**"]
    );
    assert!(card.smoke);
    assert!(!card.quarantined);
    assert!(rows[1].quarantined);
}

#[test]
fn every_journey_is_unclassified_because_the_contract_has_no_safety_field() {
    // journeys-catalog.md: "No other keys are allowed", so the catalog
    // cannot carry a safety class and the GUI must not invent one.
    let rows = parse_rows(FIXTURE).unwrap();
    assert!(rows.iter().all(|r| r.safety == Safety::Unclassified));
}

#[test]
fn rejects_a_catalog_with_extra_keys_like_guard_does() {
    let text = FIXTURE.replacen(
        "\"quarantined\": false",
        "\"quarantined\": false, \"safety\": \"safe\"",
        1,
    );
    assert!(matches!(
        parse_rows(&text),
        Err(CatalogLoadError::Invalid(_))
    ));
}

#[test]
fn the_catalog_path_is_under_the_playwright_dir() {
    let path = catalog_path(Path::new("repo"));
    assert!(path.ends_with("e2e-playwright/catalog/journeys.json"));
}

#[test]
fn a_missing_catalog_is_a_load_error() {
    let err = load(Path::new("does/not/exist")).unwrap_err();
    assert!(matches!(err, CatalogLoadError::Read { .. }));
}

#[test]
fn areas_are_sorted_and_unique() {
    let rows = parse_rows(FIXTURE).unwrap();
    assert_eq!(areas(&rows), vec!["billing", "checkout"]);
}

#[test]
fn spec_file_follows_the_layout_convention() {
    let rows = parse_rows(FIXTURE).unwrap();
    assert_eq!(
        rows[0].conventional_spec_file(),
        "tests/journeys/checkout/customer-card.spec.ts"
    );
}

#[test]
fn search_matches_id_title_or_area_case_insensitively() {
    let rows = parse_rows(FIXTURE).unwrap();
    assert!(rows[0].matches("CARD", None));
    assert!(rows[1].matches("invoice", None));
    assert!(!rows[1].matches("card", None));
    assert!(rows[0].matches("", Some("checkout")));
    assert!(!rows[0].matches("", Some("billing")));
}

fn row(id: &str, area: &str) -> JourneyRow {
    JourneyRow {
        id: id.into(),
        area: area.into(),
        title: format!("title of {id}"),
        paths: Vec::new(),
        smoke: false,
        quarantined: false,
        safety: Safety::Unclassified,
    }
}

fn area(name: &str, count: usize) -> TreeItem {
    TreeItem::Area {
        name: name.into(),
        count,
    }
}

#[test]
fn the_tree_groups_journeys_by_sorted_area() {
    let rows = vec![
        row("ship.a", "ship"),
        row("bill.a", "bill"),
        row("ship.b", "ship"),
    ];
    let items = tree_items(&rows, "", &BTreeSet::new());
    assert_eq!(
        items,
        vec![
            area("bill", 1),
            TreeItem::Journey(1),
            area("ship", 2),
            TreeItem::Journey(0),
            TreeItem::Journey(2),
        ]
    );
}

#[test]
fn a_collapsed_area_hides_its_journeys() {
    let rows = vec![row("ship.a", "ship"), row("bill.a", "bill")];
    let collapsed: BTreeSet<String> = ["ship".to_string()].into();
    let items = tree_items(&rows, "", &collapsed);
    assert_eq!(
        items,
        vec![area("bill", 1), TreeItem::Journey(1), area("ship", 1)]
    );
}

#[test]
fn a_search_keeps_matching_areas_only_and_opens_them() {
    let rows = vec![row("ship.a", "ship"), row("bill.a", "bill")];
    let collapsed: BTreeSet<String> = ["ship".to_string()].into();
    let items = tree_items(&rows, "SHIP.A", &collapsed);
    assert_eq!(items, vec![area("ship", 1), TreeItem::Journey(0)]);
}
