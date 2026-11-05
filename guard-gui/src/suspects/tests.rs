use super::*;

const DOC: &str = r#"{
  "bases": [{"repo": null, "reference": "main", "merge_base": "abc"}],
  "journeys": [
    {"journey": "orders.checkout", "title": "Checkout", "status": "failed", "tests": 2,
     "commits": [
       {"repo": "api", "sha": "a1b2c3d4", "short": "a1b2c3d", "author": "Bob",
        "date": "2023-11-14T22:13:20Z", "timestamp": 1700000000,
        "subject": "api: rewrite totals", "files": ["api/a.cs", "api/b.cs"], "recent": false}
     ],
     "uncommitted": ["web/checkout/d.ts"],
     "blame": [
       {"file": "e2e/tests/c.spec.ts", "line": 3, "sha": "ffff", "short": "fffffff",
        "author": "Quinn", "date": "2023-11-01T00:00:00Z", "timestamp": 1698796800,
        "subject": "test: total", "content": "expect(total).toBe(10);", "uncommitted": false},
       {"file": "e2e/tests/c.spec.ts", "line": 9, "sha": null, "short": null,
        "author": null, "date": null, "timestamp": null, "subject": null,
        "content": "x", "uncommitted": true}
     ]},
    {"journey": null, "title": "untagged", "status": "flaky", "tests": 1,
     "commits": [], "uncommitted": [], "blame": []}
  ],
  "warnings": ["backoffice-web: no base branch found"]
}"#;

#[test]
fn the_json_of_guard_suspects_parses() {
    let doc = parse(DOC).unwrap();
    assert_eq!(doc.journeys.len(), 2);
    assert_eq!(doc.warnings, ["backoffice-web: no base branch found"]);
    let checkout = doc.for_journey("orders.checkout").unwrap();
    assert_eq!(checkout.commits[0].short, "a1b2c3d");
    assert_eq!(checkout.commits[0].repo.as_deref(), Some("api"));
    assert_eq!(checkout.uncommitted, ["web/checkout/d.ts"]);
    assert_eq!(checkout.blame[0].author.as_deref(), Some("Quinn"));
    assert!(checkout.blame[1].uncommitted);
    assert!(doc.for_journey("nope").is_none());
    assert!(checkout.has_any());
    assert!(!doc.journeys[1].has_any());
}

#[test]
fn unknown_fields_are_ignored_and_bad_json_is_an_error() {
    let doc = parse(r#"{"journeys": [], "extra": 1}"#).unwrap();
    assert!(doc.journeys.is_empty());
    assert!(parse("not json").is_err());
    assert!(parse("").is_err());
}

#[test]
fn ages_read_like_a_commit_list() {
    let now = 1_700_000_000;
    assert_eq!(relative_age(now, now + 50), "just now");
    assert_eq!(relative_age(now, now - 30), "just now");
    assert_eq!(relative_age(now, now - 5 * 60), "5 min ago");
    assert_eq!(relative_age(now, now - 3 * 3600), "3 h ago");
    assert_eq!(relative_age(now, now - 86_400), "1 day ago");
    assert_eq!(relative_age(now, now - 4 * 86_400), "4 days ago");
    assert_eq!(relative_age(now, now - 15 * 86_400), "2 weeks ago");
    assert_eq!(relative_age(now, now - 70 * 86_400), "2 months ago");
    assert_eq!(relative_age(now, now - 800 * 86_400), "2 years ago");
}

#[test]
fn a_subject_is_shortened_on_a_char_boundary() {
    assert_eq!(shorten("short", 10), "short");
    assert_eq!(
        shorten("edición masiva de pedidos", 10),
        "edición m\u{2026}"
    );
}

#[test]
fn suspects_of_another_report_are_not_current() {
    let cached = Some((Some(1u32), "doc"));
    assert_eq!(current(&cached, &Some(1)), Some(&"doc"));
    // A newer report: the cached rows belong to the old one.
    assert_eq!(current(&cached, &Some(2)), None);
    assert_eq!(current(&cached, &None), None);
    assert_eq!(current::<Option<u32>, &str>(&None, &Some(1)), None);
}
