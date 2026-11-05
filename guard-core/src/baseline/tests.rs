use super::*;

fn facts(status: TestStatus, baseline: Option<BaselineOutcome>) -> BlockingFacts {
    BlockingFacts {
        status,
        baseline,
        smoke: false,
        affected_by_path: false,
    }
}

#[test]
fn classifies_every_base_run() {
    assert_eq!(classify(BaseRun::Passed), BaselineOutcome::Regression);
    assert_eq!(classify(BaseRun::Missing), BaselineOutcome::Regression);
    assert_eq!(classify(BaseRun::Failed), BaselineOutcome::PreExisting);
    assert_eq!(classify(BaseRun::Flaky), BaselineOutcome::Flaky);
    assert_eq!(
        classify(BaseRun::Infrastructure),
        BaselineOutcome::Unavailable
    );
}

#[test]
fn passed_and_skipped_never_block() {
    assert!(!is_blocking(facts(TestStatus::Passed, None)));
    assert!(!is_blocking(BlockingFacts {
        smoke: true,
        ..facts(TestStatus::Skipped, None)
    }));
}

#[test]
fn a_failure_without_baseline_blocks_fail_closed() {
    assert!(is_blocking(facts(TestStatus::Failed, None)));
}

#[test]
fn regression_and_unavailable_block() {
    assert!(is_blocking(facts(
        TestStatus::Failed,
        Some(BaselineOutcome::Regression)
    )));
    assert!(is_blocking(facts(
        TestStatus::Failed,
        Some(BaselineOutcome::Unavailable)
    )));
}

#[test]
fn pre_existing_does_not_block_even_for_smoke() {
    assert!(!is_blocking(BlockingFacts {
        smoke: true,
        affected_by_path: true,
        ..facts(TestStatus::Failed, Some(BaselineOutcome::PreExisting))
    }));
}

#[test]
fn flaky_blocks_only_for_smoke_or_affected_journeys() {
    for baseline in [None, Some(BaselineOutcome::Flaky)] {
        let status = if baseline.is_none() {
            TestStatus::Flaky
        } else {
            TestStatus::Failed
        };
        assert!(!is_blocking(facts(status, baseline)));
        assert!(is_blocking(BlockingFacts {
            smoke: true,
            ..facts(status, baseline)
        }));
        assert!(is_blocking(BlockingFacts {
            affected_by_path: true,
            ..facts(status, baseline)
        }));
    }
}
