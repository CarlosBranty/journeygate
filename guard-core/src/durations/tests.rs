use super::*;
use crate::verdict::{Classification, TestOutcome};

const HISTORY: &str = r#"{
  "version": 1,
  "files": {
    "tests/journeys/checkout/customer-card.spec.ts": { "duration_ms": 12450, "samples": 7 },
    "tests/smoke/healthz.spec.ts": { "duration_ms": 100, "samples": 1 }
  }
}"#;

fn outcome(file: &str, duration_ms: u64) -> TestOutcome {
    TestOutcome {
        title: "t".into(),
        file: file.into(),
        line: 1,
        project: "chromium".into(),
        tags: vec![],
        journey: None,
        status: Classification::Passed,
        blocking: false,
        attempts: 1,
        duration_ms,
        error: None,
    }
}

#[test]
fn parses_the_history_file() {
    let history = parse_history(HISTORY).unwrap();
    assert_eq!(history.files.len(), 2);
    let entry = &history.files["tests/journeys/checkout/customer-card.spec.ts"];
    assert_eq!(entry.duration_ms, 12450);
    assert_eq!(entry.samples, 7);
    assert_eq!(
        history.known_durations()["tests/smoke/healthz.spec.ts"],
        100
    );
}

#[test]
fn malformed_history_is_an_error() {
    assert!(parse_history("{").is_err());
    assert!(
        parse_history(r#"{"version": 1, "files": {"a": {"duration_ms": -1, "samples": 1}}}"#)
            .is_err()
    );
    let error = parse_history(r#"{"version": 2, "files": {}}"#).unwrap_err();
    assert_eq!(error, DurationsError::UnsupportedVersion(2));
}

#[test]
fn merge_averages_known_files_and_adds_new_ones() {
    let mut history = parse_history(HISTORY).unwrap();
    let run = BTreeMap::from([
        (
            "tests/journeys/checkout/customer-card.spec.ts".to_string(),
            12451,
        ),
        ("tests/journeys/new.spec.ts".to_string(), 900),
    ]);
    history.merge(&run);
    let checkout = &history.files["tests/journeys/checkout/customer-card.spec.ts"];
    assert_eq!(checkout.duration_ms, 12451); // round((12450 + 12451) / 2) = 12450.5 -> 12451
    assert_eq!(checkout.samples, 8);
    let new = &history.files["tests/journeys/new.spec.ts"];
    assert_eq!((new.duration_ms, new.samples), (900, 1));
    let untouched = &history.files["tests/smoke/healthz.spec.ts"];
    assert_eq!((untouched.duration_ms, untouched.samples), (100, 1));
}

#[test]
fn run_durations_sum_tests_per_file() {
    let tests = vec![
        outcome("a.spec.ts", 100),
        outcome("a.spec.ts", 50),
        outcome("b.spec.ts", 7),
    ];
    let run = run_durations(&tests);
    assert_eq!(
        run,
        BTreeMap::from([("a.spec.ts".to_string(), 150), ("b.spec.ts".to_string(), 7)])
    );
}

/// The setup project runs once per shard, so the merged report carries it N
/// times; it is never planned, and keeping it would skew the median fallback.
#[test]
fn run_durations_skip_the_setup_project_and_non_spec_files() {
    let tests = vec![
        outcome("tests/setup/auth.setup.ts", 900),
        outcome("tests/setup/auth.setup.ts", 900),
        outcome("tests/setup/other.spec.ts", 5),
        outcome("tests/smoke/healthz.spec.ts", 30),
    ];
    assert_eq!(
        run_durations(&tests),
        BTreeMap::from([("tests/smoke/healthz.spec.ts".to_string(), 30)])
    );
}

#[test]
fn history_round_trips_through_json() {
    let mut history = DurationHistory::default();
    history.merge(&BTreeMap::from([("x.spec.ts".to_string(), 10)]));
    let json = history.to_json();
    assert_eq!(parse_history(&json).unwrap(), history);
    assert!(json.contains("\"version\": 1"));
}

/// Histories written before the setup project was excluded may still carry it;
/// the planner must not see it (it would skew the median fallback weight).
#[test]
fn planned_durations_ignore_setup_entries_left_in_old_histories() {
    let mut history = parse_history(HISTORY).unwrap();
    history.merge(&BTreeMap::from([(
        "tests/setup/auth.setup.ts".to_string(),
        90_000,
    )]));
    let planned = history.planned_durations();
    assert_eq!(
        planned.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![
            "tests/journeys/checkout/customer-card.spec.ts",
            "tests/smoke/healthz.spec.ts"
        ]
    );
}
