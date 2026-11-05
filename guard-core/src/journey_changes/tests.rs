use super::*;

const NOT_FLOW: &str = "journeys modified, but the ticket is not labeled flow-change";

fn change(id: &str, before: u32, after: u32, skips: u32) -> JourneyChange {
    JourneyChange {
        id: id.into(),
        file: format!("e2e-playwright/tests/journeys/{id}.spec.ts"),
        assertions_before: before,
        assertions_after: after,
        skips_added: skips,
    }
}

fn flagged(flags: &[&str]) -> ChangeReview {
    ChangeReview::Flagged(flags.iter().map(|f| f.to_string()).collect())
}

#[test]
fn review_table() {
    let cases: Vec<(&str, Vec<JourneyChange>, bool, ChangeReview)> = vec![
        ("no changes, flow-change", vec![], true, ChangeReview::Clean),
        (
            "no changes, no flow-change",
            vec![],
            false,
            ChangeReview::Clean,
        ),
        (
            "same assertions, flow-change",
            vec![change("checkout.card", 5, 5, 0)],
            true,
            ChangeReview::Clean,
        ),
        (
            "more assertions, flow-change",
            vec![change("checkout.card", 5, 7, 0)],
            true,
            ChangeReview::Clean,
        ),
        (
            "new file (0 -> n), flow-change",
            vec![change("returns.request", 0, 3, 0)],
            true,
            ChangeReview::Clean,
        ),
        (
            "modified without flow-change label",
            vec![change("checkout.card", 5, 5, 0)],
            false,
            flagged(&[NOT_FLOW]),
        ),
        (
            "assertions decreased by one",
            vec![change("checkout.card", 5, 4, 0)],
            true,
            flagged(&["checkout.card: assertions 5 -> 4"]),
        ),
        (
            "assertions dropped to zero",
            vec![change("checkout.card", 3, 0, 0)],
            true,
            flagged(&["checkout.card: assertions 3 -> 0"]),
        ),
        (
            "one skip added",
            vec![change("checkout.card", 5, 5, 1)],
            true,
            flagged(&["checkout.card: 1 skip/fixme added"]),
        ),
        (
            "everything at once, flags in order",
            vec![change("checkout.card", 5, 2, 3)],
            false,
            flagged(&[
                NOT_FLOW,
                "checkout.card: assertions 5 -> 2",
                "checkout.card: 3 skip/fixme added",
            ]),
        ),
        (
            "per change in input order, label flag only once",
            vec![
                change("b.second", 2, 1, 0),
                change("a.clean", 1, 1, 0),
                change("c.third", 4, 4, 2),
            ],
            false,
            flagged(&[
                NOT_FLOW,
                "b.second: assertions 2 -> 1",
                "c.third: 2 skip/fixme added",
            ]),
        ),
        (
            "u32 extremes",
            vec![change("x.y", u32::MAX, 0, u32::MAX)],
            true,
            flagged(&[
                "x.y: assertions 4294967295 -> 0",
                "x.y: 4294967295 skip/fixme added",
            ]),
        ),
    ];
    for (name, changes, flow_change, expected) in cases {
        assert_eq!(review(&changes, flow_change), expected, "{name}");
    }
}

#[test]
fn json_shape() {
    assert_eq!(
        serde_json::to_value(ChangeReview::Clean).unwrap(),
        serde_json::json!({"review": "clean"})
    );
    assert_eq!(
        serde_json::to_value(flagged(&["a"])).unwrap(),
        serde_json::json!({"review": "flagged", "flags": ["a"]})
    );
}
