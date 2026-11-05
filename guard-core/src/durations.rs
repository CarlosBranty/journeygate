//! `.guard/durations.json`: historical per-file durations used by the shard planner.

use crate::verdict::TestOutcome;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationHistory {
    pub version: i64,
    pub files: BTreeMap<String, DurationEntry>,
}

impl Default for DurationHistory {
    fn default() -> Self {
        Self {
            version: 1,
            files: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationEntry {
    pub duration_ms: u64,
    pub samples: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DurationsError {
    #[error("malformed durations file: {0}")]
    Malformed(String),
    #[error("unsupported durations file version {0} (only 1 exists)")]
    UnsupportedVersion(i64),
}

impl DurationHistory {
    /// `path -> duration_ms`, the planner's input.
    pub fn known_durations(&self) -> BTreeMap<String, u64> {
        self.files
            .iter()
            .map(|(k, v)| (k.clone(), v.duration_ms))
            .collect()
    }

    /// [`Self::known_durations`] restricted to [`is_planned_spec`] files: the
    /// planner's input, so stale setup entries never skew the median fallback.
    pub fn planned_durations(&self) -> BTreeMap<String, u64> {
        self.known_durations()
            .into_iter()
            .filter(|(file, _)| is_planned_spec(file))
            .collect()
    }

    /// New value = the run's duration for a new file, else `round((old + new) / 2)`;
    /// `samples += 1`. Files absent from the run keep their entry.
    pub fn merge(&mut self, run: &BTreeMap<String, u64>) {
        for (file, &duration) in run {
            self.files
                .entry(file.clone())
                .and_modify(|e| {
                    e.duration_ms = (e.duration_ms + duration).div_ceil(2);
                    e.samples = e.samples.saturating_add(1);
                })
                .or_insert(DurationEntry {
                    duration_ms: duration,
                    samples: 1,
                });
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("durations serialise")
    }
}

pub fn parse_history(text: &str) -> Result<DurationHistory, DurationsError> {
    let history: DurationHistory =
        serde_json::from_str(text).map_err(|e| DurationsError::Malformed(e.to_string()))?;
    if history.version != 1 {
        return Err(DurationsError::UnsupportedVersion(history.version));
    }
    Ok(history)
}

/// Whether `file` is something `guard gate plan` can schedule: a `*.spec.ts`
/// outside `tests/setup/` (the setup project runs in every shard).
pub fn is_planned_spec(file: &str) -> bool {
    file.ends_with(".spec.ts") && !file.starts_with("tests/setup/")
}

/// Per planned spec file: the sum of `duration_ms` of its tests in this run.
pub fn run_durations(tests: &[TestOutcome]) -> BTreeMap<String, u64> {
    let mut run = BTreeMap::new();
    for test in tests.iter().filter(|t| is_planned_spec(&t.file)) {
        *run.entry(test.file.clone()).or_insert(0) += test.duration_ms;
    }
    run
}
