//! Flaky history for the dashboard (`GET /flaky`, architecture section 15):
//! the per-test outcome of every stored gate result, aggregated over the
//! latest `window` gate results.
//!
//! `record_gate_result` writes one row per `GateReport.tests[]` entry
//! (replaced on upsert). `flaky_stats` counts, per `test_id`, the runs whose
//! status is not `skipped`, how many were `flaky` and how many `failed`, and
//! returns only tests with at least one flaky run, ordered by `flaky_rate`
//! descending, then `test_id`. Quarantine decisions (2 % rule, never `@smoke`)
//! stay with the caller.

use guard_core::ports::{StoreError, TestStatus};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default window: the last 50 gate results (architecture section 4).
pub const DEFAULT_FLAKY_WINDOW: u32 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FlakyQuery {
    /// Only gate results of this repo (`""` = results without a repo).
    pub repo: Option<String>,
    /// Number of latest gate results to look at. `0` means 50.
    pub window: u32,
}

impl FlakyQuery {
    pub(crate) fn effective_window(&self) -> u32 {
        if self.window == 0 {
            DEFAULT_FLAKY_WINDOW
        } else {
            self.window
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlakyStat {
    pub test_id: String,
    /// Journey of the newest run of the test in the window.
    pub journey: Option<String>,
    /// Runs in the window, excluding `skipped`.
    pub runs: u32,
    pub flaky: u32,
    pub failed: u32,
    /// `flaky / runs`.
    pub flaky_rate: f64,
}

#[async_trait::async_trait]
pub trait FlakyHistory: Send + Sync {
    async fn flaky_stats(&self, query: &FlakyQuery) -> Result<Vec<FlakyStat>, StoreError>;
}

pub(crate) fn status_name(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Passed => "passed",
        TestStatus::Flaky => "flaky",
        TestStatus::Failed => "failed",
        TestStatus::Skipped => "skipped",
    }
}

/// Aggregates test rows given newest gate result first.
pub(crate) fn aggregate<'a>(
    rows: impl IntoIterator<Item = (&'a str, Option<&'a str>, TestStatus)>,
) -> Vec<FlakyStat> {
    let mut by_test: BTreeMap<&str, FlakyStat> = BTreeMap::new();
    for (test_id, journey, status) in rows {
        let stat = by_test.entry(test_id).or_insert_with(|| FlakyStat {
            test_id: test_id.to_string(),
            journey: journey.map(str::to_string),
            runs: 0,
            flaky: 0,
            failed: 0,
            flaky_rate: 0.0,
        });
        match status {
            TestStatus::Skipped => continue,
            TestStatus::Flaky => stat.flaky += 1,
            TestStatus::Failed => stat.failed += 1,
            TestStatus::Passed => {}
        }
        stat.runs += 1;
    }
    let mut stats: Vec<FlakyStat> = by_test
        .into_values()
        .filter(|s| s.flaky > 0)
        .map(|mut s| {
            s.flaky_rate = f64::from(s.flaky) / f64::from(s.runs);
            s
        })
        .collect();
    sort_stats(&mut stats);
    stats
}

pub(crate) fn sort_stats(stats: &mut [FlakyStat]) {
    stats.sort_by(|a, b| {
        b.flaky_rate
            .total_cmp(&a.flaky_rate)
            .then_with(|| a.test_id.cmp(&b.test_id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_counts_and_orders() {
        let rows = vec![
            ("b", Some("j.new"), TestStatus::Flaky),
            ("a", None, TestStatus::Passed),
            ("b", Some("j.old"), TestStatus::Passed),
            ("a", None, TestStatus::Flaky),
            ("c", None, TestStatus::Skipped),
            ("c", None, TestStatus::Flaky),
            ("d", None, TestStatus::Failed),
        ];
        let stats = aggregate(rows);
        let ids: Vec<_> = stats.iter().map(|s| s.test_id.as_str()).collect();
        // c: 1/1, a: 1/2, b: 1/2 (tie by id), d never flaky.
        assert_eq!(ids, ["c", "a", "b"]);
        assert_eq!(stats[2].journey.as_deref(), Some("j.new"));
        assert_eq!(stats[0].runs, 1);
    }

    #[test]
    fn window_zero_means_default() {
        assert_eq!(FlakyQuery::default().effective_window(), 50);
        let q = FlakyQuery {
            window: 3,
            ..FlakyQuery::default()
        };
        assert_eq!(q.effective_window(), 3);
    }
}
