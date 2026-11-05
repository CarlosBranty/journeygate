use super::*;
use std::path::Path;

const FIXTURE: &str = include_str!("../../tests/fixtures/report.json");

#[test]
fn maps_tests_to_journeys_through_the_journey_tag() {
    let results = parse_report(FIXTURE).unwrap();
    assert_eq!(results.len(), 2, "the setup test has no journey tag");
    assert!(results.contains_key("checkout.customer-card"));
    assert!(results.contains_key("billing.invoice-pdf"));
}

#[test]
fn the_worst_test_status_wins_and_durations_add_up() {
    let results = parse_report(FIXTURE).unwrap();
    let card = &results["checkout.customer-card"];
    assert_eq!(card.outcome, Outcome::Failed);
    assert_eq!(card.duration_ms, 1164 + 5751);
    assert_eq!(card.tests, 2);
    let invoice = &results["billing.invoice-pdf"];
    assert_eq!(invoice.outcome, Outcome::Flaky);
    assert_eq!(invoice.duration_ms, 900 + 700, "every attempt counts");
}

#[test]
fn the_first_error_is_kept_without_terminal_colors() {
    let results = parse_report(FIXTURE).unwrap();
    assert_eq!(
        results["checkout.customer-card"].error.as_deref(),
        Some("Error: expect(locator).toHaveText() failed")
    );
}

#[test]
fn attachments_with_a_path_are_listed_and_inline_bodies_skipped() {
    let results = parse_report(FIXTURE).unwrap();
    let card = &results["checkout.customer-card"];
    let names: Vec<&str> = card.attachments.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, vec!["screenshot", "trace"]);
    assert!(card.attachments[0].is_image());
    assert!(!card.attachments[1].is_image());
    let invoice = &results["billing.invoice-pdf"];
    assert_eq!(invoice.attachments[0].name, "video");
}

#[test]
fn spec_files_are_recorded_once() {
    let results = parse_report(FIXTURE).unwrap();
    assert_eq!(
        results["checkout.customer-card"].files,
        vec!["tests/journeys/checkout/customer-card.spec.ts"]
    );
}

#[test]
fn relative_attachment_paths_resolve_against_the_playwright_dir() {
    let e2e = Path::new("repo").join("e2e-playwright");
    let results = parse_report(FIXTURE).unwrap();
    let trace = &results["checkout.customer-card"].attachments[1];
    assert_eq!(
        trace.resolved(&e2e),
        e2e.join("test-results/declined/trace.zip")
    );
}

#[test]
fn a_malformed_report_is_an_error() {
    assert!(parse_report("{not json").is_err());
}

#[test]
fn a_test_level_skip_is_skipped() {
    let text = r#"{"suites":[{"title":"s","specs":[{"title":"t","file":"f","line":1,
        "tags":["journey:a.b"],"tests":[{"projectName":"p","status":"skipped","results":[]}]}]}]}"#;
    let results = parse_report(text).unwrap();
    assert_eq!(results["a.b"].outcome, Outcome::Skipped);
}

#[test]
fn history_duration_sums_the_known_spec_files() {
    let history = guard_core::durations::parse_history(
        r#"{"version":1,"files":{
            "tests/journeys/checkout/customer-card.spec.ts":{"duration_ms":4000,"samples":2},
            "tests/x.spec.ts":{"duration_ms":1000,"samples":1}}}"#,
    )
    .unwrap();
    let files = [
        "tests/journeys/checkout/customer-card.spec.ts".to_string(),
        "tests/x.spec.ts".to_string(),
    ];
    assert_eq!(history_ms(&history, &files), Some(5000));
    assert_eq!(history_ms(&history, &["missing".to_string()]), None);
}

#[test]
fn durations_format_as_seconds_or_minutes() {
    assert_eq!(format_duration(950), "0.9s");
    assert_eq!(format_duration(12_450), "12.4s");
    assert_eq!(format_duration(125_000), "2m05s");
}

fn stamp(secs: u64, len: u64) -> ReportStamp {
    ReportStamp {
        modified: Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)),
        len,
    }
}

#[test]
fn a_report_is_fresh_only_when_the_run_rewrote_it() {
    assert!(report_is_fresh(None, Some(stamp(10, 5))));
    assert!(report_is_fresh(Some(stamp(10, 5)), Some(stamp(11, 5))));
    assert!(report_is_fresh(Some(stamp(10, 5)), Some(stamp(10, 6))));
    assert!(!report_is_fresh(Some(stamp(10, 5)), Some(stamp(10, 5))));
    // The run deleted it, or never wrote one.
    assert!(!report_is_fresh(Some(stamp(10, 5)), None));
    assert!(!report_is_fresh(None, None));
}

#[test]
fn a_stale_report_counts_no_failure_for_this_run() {
    let results = parse_report(FIXTURE).unwrap();
    let (failed, flaky) = run_counts(true, &results);
    assert!(
        failed + flaky > 0,
        "the fixture has a failure or a flaky test"
    );
    assert_eq!(run_counts(false, &results), (0, 0));
}

#[test]
fn ages_read_as_relative_time() {
    use std::time::Duration;
    assert_eq!(format_age(Duration::from_secs(20)), "just now");
    assert_eq!(format_age(Duration::from_secs(60)), "1 min ago");
    assert_eq!(format_age(Duration::from_secs(59 * 60)), "59 min ago");
    assert_eq!(format_age(Duration::from_secs(3 * 3600 + 5)), "3 h ago");
    assert_eq!(format_age(Duration::from_secs(86_400)), "1 day ago");
    assert_eq!(format_age(Duration::from_secs(5 * 86_400)), "5 days ago");
}

#[test]
fn the_last_report_label_names_its_outcome_and_age() {
    use std::time::Duration;
    assert_eq!(
        last_report_label(Outcome::Failed, Some(Duration::from_secs(7200))),
        "Last report: failed (2 h ago)"
    );
    assert_eq!(
        last_report_label(Outcome::Passed, None),
        "Last report: passed"
    );
}

#[test]
fn the_report_outcome_is_its_worst_journey() {
    let results = parse_report(FIXTURE).unwrap();
    let worst = results.values().map(|r| r.outcome).max();
    assert_eq!(report_outcome(&results), worst);
    assert_eq!(report_outcome(&BTreeMap::new()), None);
}
