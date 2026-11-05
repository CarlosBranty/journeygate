use super::*;

pub(crate) const REPORT: &str = r#"{
  "config": {"rootDir": "/work/e2e-playwright"},
  "suites": [
    {
      "title": "tests/journeys/checkout/customer-card.spec.ts",
      "file": "tests/journeys/checkout/customer-card.spec.ts",
      "specs": [],
      "suites": [
        {
          "title": "Checkout: customer pays by card",
          "file": "tests/journeys/checkout/customer-card.spec.ts",
          "specs": [
            {
              "title": "creates exactly one confirmed order",
              "file": "tests/journeys/checkout/customer-card.spec.ts",
              "line": 9,
              "tags": ["journey:checkout.customer-card", "@area:checkout", "smoke"],
              "tests": [
                {
                  "projectName": "chromium",
                  "status": "flaky",
                  "results": [
                    {"duration": 10000, "retry": 0, "status": "failed",
                     "error": {"message": "\u001b[31mexpect(locator).toBeVisible() failed\u001b[39m\n\nLocator: x"}},
                    {"duration": 8230, "retry": 1, "status": "passed"}
                  ]
                }
              ]
            }
          ]
        }
      ]
    },
    {
      "title": "tests/journeys/billing/invoice-pdf.spec.ts",
      "file": "tests/journeys/billing/invoice-pdf.spec.ts",
      "specs": [
        {
          "title": "issues the invoice",
          "file": "tests/journeys/billing/invoice-pdf.spec.ts",
          "line": 7,
          "tags": ["@journey:billing.invoice-pdf", "@area:billing"],
          "tests": [
            {
              "projectName": "chromium",
              "status": "unexpected",
              "results": [
                {"duration": 1000, "retry": 0, "status": "failed", "error": {"message": "first failure"}},
                {"duration": 1000, "retry": 1, "status": "timedOut", "error": {"message": "second failure"}},
                {"duration": 1000.4, "retry": 2, "status": "failed", "error": {"message": "Timeout 30000ms exceeded.\nmore"}}
              ]
            }
          ]
        },
        {
          "title": "renders totals",
          "file": "tests/journeys/billing/invoice-pdf.spec.ts",
          "line": 3,
          "tags": ["@journey:billing.invoice-pdf", "@area:billing"],
          "tests": [
            {"projectName": "chromium", "status": "flaky", "results": [
              {"duration": 50, "retry": 0, "status": "failed", "error": {"message": "boom"}},
              {"duration": 40, "retry": 1, "status": "passed"}
            ]}
          ]
        }
      ]
    },
    {
      "title": "tests/setup/auth.setup.ts",
      "file": "tests/setup/auth.setup.ts",
      "specs": [
        {
          "title": "authenticate customer",
          "file": "tests/setup/auth.setup.ts",
          "line": 4,
          "tags": [],
          "tests": [
            {"projectName": "setup", "status": "expected", "results": [{"duration": 300, "retry": 0, "status": "passed"}]},
            {"projectName": "firefox", "status": "skipped", "results": []}
          ]
        }
      ]
    }
  ],
  "errors": [],
  "stats": {"duration": 38412.7, "expected": 1, "unexpected": 1, "flaky": 2, "skipped": 1}
}"#;

fn outcomes() -> Vec<TestOutcome> {
    classify(&parse_report(REPORT).unwrap())
}

fn by_title<'a>(tests: &'a [TestOutcome], needle: &str) -> &'a TestOutcome {
    tests.iter().find(|t| t.title.contains(needle)).unwrap()
}

#[test]
fn classifies_every_test_of_every_spec() {
    let tests = outcomes();
    assert_eq!(tests.len(), 5);
    assert_eq!(
        by_title(&tests, "confirmed order").status,
        Classification::Flaky
    );
    assert_eq!(
        by_title(&tests, "issues the invoice").status,
        Classification::Failed
    );
    assert_eq!(
        by_title(&tests, "renders totals").status,
        Classification::Flaky
    );
    let setup: Vec<&TestOutcome> = tests
        .iter()
        .filter(|t| t.file == "tests/setup/auth.setup.ts")
        .collect();
    assert_eq!(setup[0].status, Classification::Skipped);
    assert_eq!(setup[0].project, "firefox");
    assert_eq!(setup[1].status, Classification::Passed);
}

#[test]
fn flaky_smoke_blocks_and_flaky_non_smoke_does_not() {
    let tests = outcomes();
    assert!(by_title(&tests, "confirmed order").blocking);
    assert!(!by_title(&tests, "renders totals").blocking);
    assert!(by_title(&tests, "issues the invoice").blocking);
    assert!(
        tests
            .iter()
            .filter(|t| t.file.contains("setup"))
            .all(|t| !t.blocking)
    );
}

#[test]
fn title_joins_describe_blocks_without_the_file_suite() {
    let tests = outcomes();
    assert_eq!(
        by_title(&tests, "confirmed order").title,
        "Checkout: customer pays by card > creates exactly one confirmed order"
    );
    assert_eq!(
        by_title(&tests, "issues the invoice").title,
        "issues the invoice"
    );
}

#[test]
fn tags_are_normalised_sorted_and_give_the_journey() {
    let test = by_title(&outcomes(), "confirmed order").clone();
    assert_eq!(
        test.tags,
        vec![
            "@area:checkout",
            "@journey:checkout.customer-card",
            "@smoke"
        ]
    );
    assert_eq!(test.journey.as_deref(), Some("checkout.customer-card"));
    assert_eq!(by_title(&outcomes(), "authenticate").journey, None);
}

#[test]
fn attempts_duration_and_error_come_from_results() {
    let tests = outcomes();
    let flaky = by_title(&tests, "confirmed order");
    assert_eq!(flaky.attempts, 2);
    assert_eq!(flaky.duration_ms, 18230);
    assert_eq!(
        flaky.error.as_deref(),
        Some("expect(locator).toBeVisible() failed")
    );

    let failed = by_title(&tests, "issues the invoice");
    assert_eq!(failed.attempts, 3);
    assert_eq!(failed.duration_ms, 3000);
    assert_eq!(failed.error.as_deref(), Some("Timeout 30000ms exceeded."));

    let passed = by_title(&tests, "authenticate");
    assert_eq!(passed.error, None);
}

#[test]
fn tests_are_sorted_by_file_line_then_project() {
    let order: Vec<(String, u32, String)> = outcomes()
        .into_iter()
        .map(|t| (t.file, t.line, t.project))
        .collect();
    assert_eq!(
        order,
        vec![
            (
                "tests/journeys/billing/invoice-pdf.spec.ts".into(),
                3,
                "chromium".into()
            ),
            (
                "tests/journeys/billing/invoice-pdf.spec.ts".into(),
                7,
                "chromium".into()
            ),
            (
                "tests/journeys/checkout/customer-card.spec.ts".into(),
                9,
                "chromium".into()
            ),
            ("tests/setup/auth.setup.ts".into(), 4, "firefox".into()),
            ("tests/setup/auth.setup.ts".into(), 4, "setup".into()),
        ]
    );
}

#[test]
fn counts_and_verdict() {
    let tests = outcomes();
    assert_eq!(
        count(&tests),
        Counts {
            passed: 1,
            flaky: 2,
            failed: 1,
            skipped: 1
        }
    );
    assert_eq!(decide(&tests), Verdict::Red);
    assert_eq!(Verdict::Red.exit_code(), 1);
    assert_eq!(Verdict::Green.exit_code(), 0);

    let only_non_smoke_flaky: Vec<TestOutcome> =
        tests.iter().filter(|t| !t.blocking).cloned().collect();
    assert_eq!(decide(&only_non_smoke_flaky), Verdict::Green);
    assert_eq!(decide(&[]), Verdict::Green);
}

#[test]
fn local_exit_code_only_counts_unexpected_failures() {
    let tests = outcomes();
    assert_eq!(local_exit_code(&tests), 1);
    let no_failures: Vec<TestOutcome> = tests
        .into_iter()
        .filter(|t| t.status != Classification::Failed)
        .collect();
    assert_eq!(local_exit_code(&no_failures), 0);
}

#[test]
fn report_duration_comes_from_stats() {
    let report = parse_report(REPORT).unwrap();
    assert_eq!(report.duration_ms(), 38413);
    assert_eq!(report.test_count(), 5);
    assert!(!report.has_errors());
}

#[test]
fn malformed_reports_are_errors() {
    assert!(parse_report("not json").is_err());
    assert!(parse_report(r#"{"suites": [{"specs": [{"title": "x"}]}]}"#).is_err());
}

#[test]
fn playwright_exit_codes_are_interpreted_with_the_report() {
    let report = parse_report(REPORT).unwrap();
    let empty = parse_report(r#"{"suites": [], "errors": []}"#).unwrap();
    let errored = parse_report(r#"{"suites": [], "errors": [{"message": "config"}]}"#).unwrap();
    let errored_with_tests = REPORT.replace(r#""errors": [],"#, r#""errors": [{"message": "x"}],"#);
    let errored_with_tests = parse_report(&errored_with_tests).unwrap();

    assert_eq!(interpret_exit(Some(0), Some(&report)), RunOutcome::Passed);
    assert_eq!(
        interpret_exit(Some(1), Some(&report)),
        RunOutcome::TestsFailed
    );
    assert!(matches!(
        interpret_exit(Some(0), None),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(Some(1), None),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(Some(1), Some(&empty)),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(Some(1), Some(&errored)),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(Some(1), Some(&errored_with_tests)),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(Some(130), Some(&report)),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(None, Some(&report)),
        RunOutcome::Infrastructure(_)
    ));
    assert!(matches!(
        interpret_exit(Some(2), Some(&report)),
        RunOutcome::Infrastructure(_)
    ));
    assert_eq!(RunOutcome::Passed.exit_code(), 0);
    assert_eq!(RunOutcome::TestsFailed.exit_code(), 1);
    assert_eq!(RunOutcome::Infrastructure("x".into()).exit_code(), 3);
}

#[test]
fn verdict_document_has_the_contract_shape() {
    let tests = outcomes();
    let doc = VerdictDocument::new("12345-1".into(), "2026-10-31T12:00:00Z".into(), tests);
    let json = serde_json::to_string_pretty(&doc).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["run_id"], "12345-1");
    assert_eq!(value["verdict"], "red");
    assert_eq!(
        value["counts"],
        serde_json::json!({"passed": 1, "flaky": 2, "failed": 1, "skipped": 1})
    );
    assert_eq!(value["baseline"], serde_json::Value::Null);
    assert_eq!(
        value["not_implemented"],
        serde_json::json!(["baseline", "tracker"])
    );
    let test = &value["tests"][2];
    assert_eq!(test["status"], "flaky");
    assert_eq!(test["blocking"], true);
    assert_eq!(test["journey"], "checkout.customer-card");
    assert_eq!(test["project"], "chromium");
    assert_key_order(
        &json,
        0,
        &[
            "version",
            "run_id",
            "generated_at",
            "verdict",
            "counts",
            "tests",
            "baseline",
            "not_implemented",
        ],
    );
    let tests_at = json.find("\"tests\"").unwrap();
    assert_key_order(
        &json,
        tests_at,
        &[
            "title",
            "file",
            "line",
            "project",
            "tags",
            "journey",
            "status",
            "blocking",
            "attempts",
            "duration_ms",
            "error",
        ],
    );
}

fn assert_key_order(json: &str, from: usize, keys: &[&str]) {
    let mut last = from;
    for key in keys {
        let at = json[last..]
            .find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("{key} missing after {last}"));
        last += at;
    }
}

#[test]
fn failure_locations_take_the_last_failing_attempt_location() {
    let report = parse_report(
        r#"{"suites": [{"title": "a.spec.ts", "suites": [{"title": "d", "specs": [
          {"title": "t", "file": "tests/a.spec.ts", "line": 5, "tags": ["@journey:a.b"],
           "tests": [{"projectName": "chromium", "status": "unexpected", "results": [
             {"retry": 0, "status": "failed", "error": {"message": "x",
               "location": {"file": "/work/e2e/tests/a.spec.ts", "line": 9, "column": 3}}},
             {"retry": 1, "status": "failed", "error": {"message": "y",
               "location": {"file": "/work/e2e/tests/a.spec.ts", "line": 12, "column": 3}}}
           ]}]},
          {"title": "ok", "file": "tests/a.spec.ts", "line": 20,
           "tests": [{"projectName": "chromium", "status": "expected", "results": [
             {"retry": 0, "status": "passed"}]}]},
          {"title": "no location", "file": "tests/a.spec.ts", "line": 30,
           "tests": [{"projectName": "chromium", "status": "unexpected", "results": [
             {"retry": 0, "status": "failed", "error": {"message": "z"}}]}]}
        ]}]}]}"#,
    )
    .unwrap();
    assert_eq!(
        failure_locations(&report),
        vec![FailureLocation {
            title: "d > t".into(),
            spec_file: "tests/a.spec.ts".into(),
            spec_line: 5,
            project: "chromium".into(),
            file: "/work/e2e/tests/a.spec.ts".into(),
            line: 12,
        }]
    );
}
