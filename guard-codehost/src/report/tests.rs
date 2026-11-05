use super::*;
use guard_core::ports::{
    BaselineOutcome, BugAction, CheckState, EvidenceBundle, Fingerprint, GateCounts, GateReport,
    GateTest, GateVerdict, JourneyChangeReport, ReportOutcome, ReportedBug, TestStatus,
};

fn test(journey: &str, file: &str, line: u32, status: TestStatus) -> GateTest {
    GateTest {
        test_id: format!("chromium::{file}::{journey}"),
        title: format!("{journey} title"),
        file: file.into(),
        line,
        project: "chromium".into(),
        tags: vec![],
        journey: Some(journey.into()),
        status,
        blocking: false,
        attempts: 3,
        duration_ms: 1000,
        error: None,
        baseline: None,
        fingerprint: None,
        evidence: None,
    }
}

fn regression() -> GateTest {
    let mut t = test(
        "checkout.customer-card",
        "tests/journeys/checkout/customer-card.spec.ts",
        10,
        TestStatus::Failed,
    );
    t.blocking = true;
    t.baseline = Some(BaselineOutcome::Regression);
    t.fingerprint = Some(Fingerprint("1a2b3c4d".into()));
    t.evidence = Some(EvidenceBundle {
        test_id: t.test_id.clone(),
        fingerprint: Fingerprint("1a2b3c4d".into()),
        title: t.title.clone(),
        journey: t.journey.clone(),
        location: Some("tests/journeys/checkout/customer-card.spec.ts:14".into()),
        error: "Timeout".into(),
        step: None,
        seed: None,
        commit: "a".repeat(40),
        attempts: vec![],
        baseline: Some(BaselineOutcome::Regression),
        files: vec![],
    });
    t
}

fn smoke_flaky() -> GateTest {
    let mut t = test(
        "catalog.stock-badge",
        "tests/journeys/catalog/stock.spec.ts",
        9,
        TestStatus::Flaky,
    );
    t.blocking = true;
    t.tags = vec!["@smoke".into()];
    t
}

fn report(verdict: GateVerdict, tests: Vec<GateTest>, counts: GateCounts) -> GateReport {
    GateReport {
        version: 2,
        run_id: "12345-1".into(),
        generated_at: "2026-11-01T00:00:00Z".into(),
        provider: None,
        repo: Some("acme/shop".into()),
        pr: None,
        ticket_key: Some("SHOP-123".into()),
        ticket_is_flow_change: false,
        head_sha: "a".repeat(40),
        base_sha: Some("b".repeat(40)),
        verdict,
        counts,
        tests,
        journey_changes: vec![],
        change_flags: vec![],
        report_url: Some("https://s3.example/report.html".into()),
        ci_url: Some("https://ci.example/run/1".into()),
    }
}

fn red_report() -> GateReport {
    let mut r = report(
        GateVerdict::Red,
        vec![
            test(
                "cart.add",
                "tests/journeys/cart/add.spec.ts",
                3,
                TestStatus::Passed,
            ),
            regression(),
            smoke_flaky(),
        ],
        GateCounts {
            passed: 40,
            flaky: 2,
            failed: 1,
            skipped: 0,
            regressions: 1,
            pre_existing: 0,
        },
    );
    r.change_flags = vec!["journeys modified, but the ticket is not labeled flow-change".into()];
    r.journey_changes = vec![JourneyChangeReport {
        journey: "checkout.customer-card".into(),
        file: "tests/journeys/checkout/customer-card.spec.ts".into(),
        flags: vec!["assertions 5 -> 3".into()],
    }];
    r
}

fn outcome() -> ReportOutcome {
    ReportOutcome {
        bugs: vec![ReportedBug {
            fingerprint: Fingerprint("1a2b3c4d".into()),
            key: "SHOP-210".into(),
            url: "https://acme.atlassian.net/browse/SHOP-210".into(),
            action: BugAction::Created,
        }],
        ..ReportOutcome::default()
    }
}

#[test]
fn red_comment_matches_the_contract_example() {
    let expected = "<!-- guard:gate-summary -->
### guard: red

passed 40 · flaky 2 · failed 1 (regressions 1, pre-existing 0) · skipped 0

| | Test | Location | Jira |
| --- | --- | --- | --- |
| ❌ regression | `checkout.customer-card` | `tests/journeys/checkout/customer-card.spec.ts:14` | [SHOP-210](https://acme.atlassian.net/browse/SHOP-210) |
| ⚠️ flaky (blocking: smoke) | `catalog.stock-badge` | `tests/journeys/catalog/stock.spec.ts:9` | — |

**Modified journeys** (QA review via CODEOWNERS)
- journeys modified, but the ticket is not labeled flow-change
- checkout.customer-card: assertions 5 -> 3

If the flow change is intentional, update the failing journey in this PR.

[Report](https://s3.example/report.html) · [Pipeline](https://ci.example/run/1) · run `12345-1`
";
    assert_eq!(render_pr_comment(&red_report(), &outcome()), expected);
}

#[test]
fn green_comment_omits_empty_sections() {
    let r = GateReport {
        report_url: None,
        ..report(
            GateVerdict::Green,
            vec![test("cart.add", "a.spec.ts", 1, TestStatus::Passed)],
            GateCounts {
                passed: 43,
                ..GateCounts::default()
            },
        )
    };
    let expected = "<!-- guard:gate-summary -->
### guard: green

passed 43 · flaky 0 · failed 0 (regressions 0, pre-existing 0) · skipped 0

[Pipeline](https://ci.example/run/1) · run `12345-1`
";
    assert_eq!(render_pr_comment(&r, &ReportOutcome::default()), expected);
}

#[test]
fn rows_are_ordered_blocking_then_pre_existing_then_non_blocking_flaky() {
    let mut pre = test("orders.list", "o.spec.ts", 4, TestStatus::Failed);
    pre.baseline = Some(BaselineOutcome::PreExisting);
    let mut unconfirmed = test("cart.remove", "c.spec.ts", 5, TestStatus::Failed);
    unconfirmed.blocking = true;
    unconfirmed.baseline = Some(BaselineOutcome::Unavailable);
    let mut no_baseline = test("cart.empty", "e.spec.ts", 6, TestStatus::Failed);
    no_baseline.blocking = true;
    let flaky = test("search.typo", "s.spec.ts", 7, TestStatus::Flaky);
    let mut affected_flaky = test("search.go", "g.spec.ts", 8, TestStatus::Failed);
    affected_flaky.blocking = true;
    affected_flaky.baseline = Some(BaselineOutcome::Flaky);
    let mut title_only = test("x", "t.spec.ts", 9, TestStatus::Flaky);
    title_only.journey = None;
    title_only.title = "a | b".into();

    let r = report(
        GateVerdict::Red,
        vec![
            flaky,
            pre,
            unconfirmed,
            title_only,
            no_baseline,
            affected_flaky,
        ],
        GateCounts::default(),
    );
    let comment = render_pr_comment(&r, &ReportOutcome::default());
    let rows: Vec<&str> = comment
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| |") && !l.starts_with("| ---"))
        .collect();
    assert_eq!(
        rows,
        vec![
            "| ❌ unconfirmed | `cart.remove` | `c.spec.ts:5` | — |",
            "| ❌ unconfirmed | `cart.empty` | `e.spec.ts:6` | — |",
            "| ⚠️ flaky (blocking: affected) | `search.go` | `g.spec.ts:8` | — |",
            "| 🟡 pre-existing | `orders.list` | `o.spec.ts:4` | — |",
            "| ⚠️ flaky | `search.typo` | `s.spec.ts:7` | — |",
            "| ⚠️ flaky | `a \\| b` | `t.spec.ts:9` | — |",
        ]
    );
    // No regression: no flow-change sentence.
    assert!(!comment.contains("flow change"));
}

#[test]
fn at_most_30_rows_then_a_count() {
    let tests: Vec<GateTest> = (0..35)
        .map(|i| test(&format!("j.{i}"), "f.spec.ts", i, TestStatus::Flaky))
        .collect();
    let comment = render_pr_comment(
        &report(GateVerdict::Green, tests, GateCounts::default()),
        &ReportOutcome::default(),
    );
    let rows = comment.lines().filter(|l| l.starts_with("| ⚠️")).count();
    assert_eq!(rows, 30);
    assert!(comment.contains("`j.29`"));
    assert!(!comment.contains("`j.30`"));
    assert!(comment.contains("\n\n… and 5 more\n"), "{comment}");
}

#[test]
fn gate_status_for_red_green_and_infrastructure_errors() {
    let red = gate_status(&red_report(), None);
    assert_eq!(red.name, "guard");
    assert_eq!(red.head_sha, "a".repeat(40));
    assert_eq!(red.state, CheckState::Failure);
    assert_eq!(red.title, "guard: red — 1 regression, 1 blocking flaky");
    assert_eq!(red.details_url.as_deref(), Some("https://ci.example/run/1"));
    let comment = render_pr_comment(&red_report(), &ReportOutcome::default());
    assert_eq!(
        red.summary,
        comment
            .strip_prefix("<!-- guard:gate-summary -->\n")
            .unwrap()
    );
    assert!(!red.summary.contains(PR_COMMENT_MARKER));

    let mut green_report = report(
        GateVerdict::Green,
        vec![],
        GateCounts {
            passed: 43,
            flaky: 2,
            ..GateCounts::default()
        },
    );
    green_report.ci_url = None;
    let green = gate_status(&green_report, None);
    assert_eq!(green.state, CheckState::Success);
    assert_eq!(green.title, "guard: green — 43 passed, 2 flaky");
    assert_eq!(
        green.details_url.as_deref(),
        Some("https://s3.example/report.html")
    );

    green_report.counts.flaky = 0;
    assert_eq!(
        gate_status(&green_report, None).title,
        "guard: green — 43 passed"
    );

    let infra = gate_status(&green_report, Some("baseline worktree failed"));
    assert_eq!(infra.state, CheckState::Error);
    assert_eq!(infra.title, "guard: infrastructure error");
    assert!(infra.summary.contains("baseline worktree failed"));
}

#[test]
fn red_title_counts_every_blocking_kind_and_is_capped() {
    let mut a = regression();
    a.fingerprint = None;
    let mut b = test("cart.empty", "e.spec.ts", 6, TestStatus::Failed);
    b.blocking = true;
    let r = report(
        GateVerdict::Red,
        vec![a.clone(), a, b, smoke_flaky(), smoke_flaky()],
        GateCounts::default(),
    );
    assert_eq!(
        gate_status(&r, None).title,
        "guard: red — 2 regressions, 1 unconfirmed, 2 blocking flaky"
    );

    // Red without a blocking test (e.g. a journey-change rule) still says why it is red.
    let empty = report(GateVerdict::Red, vec![], GateCounts::default());
    assert_eq!(gate_status(&empty, None).title, "guard: red");

    let mut long = report(GateVerdict::Red, vec![], GateCounts::default());
    long.change_flags = vec!["x".repeat(500)];
    assert!(gate_status(&long, None).title.chars().count() <= 140);
}
