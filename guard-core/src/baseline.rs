//! Baseline classification and the Phase 2 blocking rule.
//!
//! Contract: `docs/contracts/finalize-phase2.md`. A test that failed 3/3 on the
//! PR head is re-run against the base commit (app and tests at base); the
//! result decides whether this change broke it.

use crate::ports::types::{BaselineOutcome, TestStatus};

#[cfg(test)]
mod tests;

/// Most failed tests re-run against the base commit in one finalize. The rest
/// get `BaselineOutcome::Unavailable` (blocking, fail closed).
pub const MAX_BASELINE_TESTS: usize = 20;

/// What the base-commit run of one test produced (3 attempts, `--retries=2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseRun {
    /// `expected`: passed on the first attempt.
    Passed,
    /// `unexpected`: failed all 3 attempts.
    Failed,
    /// `flaky`: failed, then passed on a retry.
    Flaky,
    /// The test does not exist at the base commit (zero tests matched).
    Missing,
    /// No result: worktree, install, `guard up` or Playwright failed.
    Infrastructure,
}

/// `Passed`/`Missing` = regression, `Failed` = pre-existing, `Flaky` = flaky,
/// `Infrastructure` = unavailable.
pub fn classify(run: BaseRun) -> BaselineOutcome {
    match run {
        BaseRun::Passed | BaseRun::Missing => BaselineOutcome::Regression,
        BaseRun::Failed => BaselineOutcome::PreExisting,
        BaseRun::Flaky => BaselineOutcome::Flaky,
        BaseRun::Infrastructure => BaselineOutcome::Unavailable,
    }
}

/// Facts about one test that decide whether it blocks the PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingFacts {
    pub status: TestStatus,
    /// Only meaningful for `TestStatus::Failed`; `None` = no baseline run.
    pub baseline: Option<BaselineOutcome>,
    /// The test carries `@smoke`.
    pub smoke: bool,
    /// The test's journey is selected by a changed file matching its `paths`.
    pub affected_by_path: bool,
}

/// Phase 2 blocking rule:
///
/// | Status | Baseline | Blocks |
/// | --- | --- | --- |
/// | passed, skipped | — | no |
/// | flaky | — | if smoke or affected by path |
/// | failed | regression, unavailable, none | yes |
/// | failed | pre_existing | no |
/// | failed | flaky | if smoke or affected by path |
pub fn is_blocking(facts: BlockingFacts) -> bool {
    let unstable_blocks = facts.smoke || facts.affected_by_path;
    match facts.status {
        TestStatus::Passed | TestStatus::Skipped => false,
        TestStatus::Flaky => unstable_blocks,
        TestStatus::Failed => match facts.baseline {
            None | Some(BaselineOutcome::Regression) | Some(BaselineOutcome::Unavailable) => true,
            Some(BaselineOutcome::PreExisting) => false,
            Some(BaselineOutcome::Flaky) => unstable_blocks,
        },
    }
}
