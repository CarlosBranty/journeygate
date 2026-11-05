use super::*;
use guard_core::catalog::{Catalog, Journey};

fn journey(
    id: &str,
    area: &str,
    title: &str,
    question: &str,
    smoke: bool,
    quarantined: bool,
) -> Journey {
    Journey {
        id: id.to_string(),
        area: area.to_string(),
        title: title.to_string(),
        impact_question: question.to_string(),
        paths: vec![format!("src/{area}/**")],
        smoke,
        quarantined,
    }
}

fn catalog() -> Catalog {
    Catalog {
        schema: None,
        version: 1,
        journeys: vec![
            journey(
                "checkout.customer-card",
                "checkout",
                "Pay by card",
                "Can a customer pay by card?",
                true,
                false,
            ),
            journey(
                "auth.login",
                "auth",
                "Log in",
                "Can a user log in with a password?",
                true,
                false,
            ),
            journey(
                "checkout.coupon",
                "checkout",
                "Apply coupon",
                "Does a valid COUPON reduce the total?",
                false,
                true,
            ),
        ],
    }
}

fn ids(output: &CatalogOutput) -> Vec<&str> {
    output.journeys.iter().map(|j| j.id.as_str()).collect()
}

#[test]
fn no_filter_returns_every_journey_sorted_by_id() {
    let output = search(&catalog(), &CatalogQuery::default());
    assert_eq!(
        ids(&output),
        vec!["auth.login", "checkout.coupon", "checkout.customer-card"]
    );
}

#[test]
fn entries_carry_journey_area_and_smoke_tags() {
    let output = search(&catalog(), &CatalogQuery::default());
    let card = output
        .journeys
        .iter()
        .find(|j| j.id == "checkout.customer-card")
        .unwrap();
    assert_eq!(
        card.tags,
        vec![
            "@journey:checkout.customer-card",
            "@area:checkout",
            "@smoke"
        ]
    );
    assert_eq!(card.paths, vec!["src/checkout/**"]);
    assert!(card.smoke);
    assert!(!card.quarantined);
    let coupon = output
        .journeys
        .iter()
        .find(|j| j.id == "checkout.coupon")
        .unwrap();
    assert_eq!(
        coupon.tags,
        vec!["@journey:checkout.coupon", "@area:checkout"]
    );
}

#[test]
fn filters_by_area() {
    let query = CatalogQuery {
        area: Some("checkout".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(
        ids(&search(&catalog(), &query)),
        vec!["checkout.coupon", "checkout.customer-card"]
    );
}

#[test]
fn filters_by_tag() {
    let smoke = CatalogQuery {
        tag: Some("@smoke".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(
        ids(&search(&catalog(), &smoke)),
        vec!["auth.login", "checkout.customer-card"]
    );
    let one = CatalogQuery {
        tag: Some("@journey:auth.login".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(ids(&search(&catalog(), &one)), vec!["auth.login"]);
    let area = CatalogQuery {
        tag: Some("@area:auth".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(ids(&search(&catalog(), &area)), vec!["auth.login"]);
}

#[test]
fn text_matches_id_title_and_impact_question_case_insensitively() {
    let by_question = CatalogQuery {
        text: Some("coupon reduce".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(
        ids(&search(&catalog(), &by_question)),
        vec!["checkout.coupon"]
    );
    let by_title = CatalogQuery {
        text: Some("PAY BY".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(
        ids(&search(&catalog(), &by_title)),
        vec!["checkout.customer-card"]
    );
    let by_id = CatalogQuery {
        text: Some("auth.LOG".into()),
        ..CatalogQuery::default()
    };
    assert_eq!(ids(&search(&catalog(), &by_id)), vec!["auth.login"]);
}

#[test]
fn can_exclude_quarantined_journeys() {
    let query = CatalogQuery {
        include_quarantined: false,
        ..CatalogQuery::default()
    };
    assert_eq!(
        ids(&search(&catalog(), &query)),
        vec!["auth.login", "checkout.customer-card"]
    );
}

#[test]
fn filters_combine_with_and() {
    let query = CatalogQuery {
        area: Some("checkout".into()),
        tag: Some("@smoke".into()),
        text: Some("card".into()),
        include_quarantined: true,
    };
    assert_eq!(
        ids(&search(&catalog(), &query)),
        vec!["checkout.customer-card"]
    );
    let none = CatalogQuery {
        area: Some("auth".into()),
        text: Some("coupon".into()),
        ..CatalogQuery::default()
    };
    assert!(search(&catalog(), &none).journeys.is_empty());
}
