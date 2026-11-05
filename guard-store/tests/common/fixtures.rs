//! Builders of port values for the shared suites.

use guard_core::ports::*;

pub fn state() -> TriageState {
    TriageState {
        summary: "Fix coupon rounding".into(),
        description: "Coupons round down instead of half-up.".into(),
        acceptance_criteria: vec!["10.005 rounds to 10.01".into()],
        mentioned_modules: vec!["checkout".into()],
        blast_radius: BlastRadius::Isolated,
    }
}

pub fn answers() -> TriageAnswers {
    TriageAnswers {
        model: "jev-1.13.0".into(),
        area: Classified {
            value: Area::new("checkout"),
            confidence: 0.91,
        },
        kind: Classified {
            value: Kind::Bugfix,
            confidence: 0.88,
        },
        risk: Classified {
            value: 1.25,
            confidence: 0.7,
        },
        clear_e2e_criteria: 0.93,
        touches_money_or_security: 0.4,
        agent_suitable: 0.85,
        changes_existing_flow: 0.1,
        raw: serde_json::json!({"area": {"choice": "checkout", "confidence": 0.91}}),
    }
}

/// A successful decision of `ticket` at `decided_at`.
pub fn decision(ticket: &str, decided_at: &str, mode: Mode) -> TriageDecision {
    TriageDecision {
        ticket_key: ticket.into(),
        decided_at: decided_at.into(),
        mode,
        state: state(),
        answers: Some(answers()),
        engine_error: None,
        facts: ImpactFacts::default(),
        thresholds: Thresholds::default(),
        permutation_seed: Some(u64::MAX - 7),
        decided: Lane::Agent,
        effective: if mode == Mode::Shadow {
            Lane::Human(vec![HumanReason::ShadowMode])
        } else {
            Lane::Agent
        },
    }
}

/// A decision recorded when the engine failed.
pub fn failed_decision(ticket: &str, decided_at: &str) -> TriageDecision {
    TriageDecision {
        answers: None,
        engine_error: Some("engine: HTTP 529 overloaded".into()),
        permutation_seed: None,
        decided: Lane::Human(vec![HumanReason::EngineUnavailable]),
        effective: Lane::Human(vec![HumanReason::EngineUnavailable]),
        ..decision(ticket, decided_at, Mode::Active)
    }
}

pub fn label(ticket: &str, labeled_at: &str, labels: &[&str]) -> HumanLabel {
    HumanLabel {
        ticket_key: ticket.into(),
        labeled_at: labeled_at.into(),
        area: Some(Area::new("checkout")),
        kind: Some(Kind::Refactor),
        lane: Some(LaneKind::Human),
        flow_change: Some(true),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        source: LabelSource::JiraWebhook,
    }
}

pub fn pr(repo: &str, number: u64) -> PrContext {
    PrContext {
        provider: CodeHostProvider::Github,
        repo: repo.into(),
        number,
        title: "Fix coupon rounding".into(),
        url: format!("https://github.com/{repo}/pull/{number}"),
        author: "dev".into(),
        ticket_key: Some("SHOP-1".into()),
        branch: "fix/SHOP-1".into(),
        base_branch: "main".into(),
        base_sha: "b".repeat(40),
        head_sha: "a".repeat(40),
        changed_files: vec!["src/checkout.ts".into()],
    }
}

pub fn test(test_id: &str, status: TestStatus) -> GateTest {
    GateTest {
        test_id: test_id.into(),
        title: test_id.rsplit("::").next().unwrap_or(test_id).into(),
        file: "tests/journeys/checkout/pay.spec.ts".into(),
        line: 9,
        project: "chromium".into(),
        tags: vec!["@journey:checkout.pay".into()],
        journey: Some("checkout.pay".into()),
        status,
        blocking: status != TestStatus::Passed,
        attempts: if status == TestStatus::Passed { 1 } else { 3 },
        duration_ms: 1200,
        error: None,
        baseline: None,
        fingerprint: None,
        evidence: None,
    }
}

/// A gate report of `repo` (or none) with the given tests.
pub fn report(
    repo: Option<&str>,
    run_id: &str,
    pr_number: Option<u64>,
    tests: Vec<GateTest>,
) -> GateReport {
    let verdict = if tests.iter().any(|t| t.status == TestStatus::Failed) {
        GateVerdict::Red
    } else {
        GateVerdict::Green
    };
    GateReport {
        version: 2,
        run_id: run_id.into(),
        generated_at: "2026-11-01T10:00:00Z".into(),
        provider: repo.map(|_| CodeHostProvider::Github),
        repo: repo.map(str::to_string),
        pr: match (repo, pr_number) {
            (Some(r), Some(n)) => Some(pr(r, n)),
            _ => None,
        },
        ticket_key: Some("SHOP-1".into()),
        ticket_is_flow_change: false,
        head_sha: "a".repeat(40),
        base_sha: Some("b".repeat(40)),
        verdict,
        counts: GateCounts {
            passed: tests
                .iter()
                .filter(|t| t.status == TestStatus::Passed)
                .count(),
            flaky: tests
                .iter()
                .filter(|t| t.status == TestStatus::Flaky)
                .count(),
            failed: tests
                .iter()
                .filter(|t| t.status == TestStatus::Failed)
                .count(),
            skipped: tests
                .iter()
                .filter(|t| t.status == TestStatus::Skipped)
                .count(),
            regressions: tests
                .iter()
                .filter(|t| t.status == TestStatus::Failed)
                .count(),
            pre_existing: 0,
        },
        tests,
        journey_changes: vec![],
        change_flags: vec![],
        report_url: None,
        ci_url: Some("https://ci.example/run/1".into()),
    }
}

pub fn outcome(warning: &str) -> ReportOutcome {
    ReportOutcome {
        warnings: if warning.is_empty() {
            vec![]
        } else {
            vec![warning.into()]
        },
        ..ReportOutcome::default()
    }
}
