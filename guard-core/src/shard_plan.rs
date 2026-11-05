//! Duration-balanced shard planning.
//!
//! Longest-processing-time first: files sorted by weight descending (ties by
//! path ascending), each one assigned to the currently lightest shard (ties to
//! the lowest index). The result depends only on the inputs, never on order.

use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// Weight of every file when there is no history at all.
pub const DEFAULT_WEIGHT_MS: u64 = 1000;

/// One shard: files relative to the Playwright dir, plus the estimated duration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    pub files: Vec<String>,
    pub estimated_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShardPlanError {
    #[error("the number of shards must be at least 1")]
    ZeroShards,
}

impl Shard {
    /// `testMatch` entries: Playwright matches them against absolute paths.
    pub fn plan_entries(&self) -> Vec<String> {
        self.files.iter().map(|f| format!("**/{f}")).collect()
    }

    /// The `shard-<i>.json` body: `{"files": [...]}`.
    pub fn plan_json(&self) -> String {
        let body = serde_json::json!({ "files": self.plan_entries() });
        serde_json::to_string_pretty(&body).expect("plan serialises")
    }
}

/// Median, rounding the mean of the two middle values half up.
pub fn median(values: &[u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]).div_ceil(2)
    })
}

/// Weighs every file and sorts by weight descending, then path ascending.
/// Files without history get the median of all known durations, or
/// [`DEFAULT_WEIGHT_MS`] when the history is empty.
pub fn weigh(files: &[String], history: &BTreeMap<String, u64>) -> Vec<(String, u64)> {
    let known: Vec<u64> = history.values().copied().collect();
    let fallback = median(&known).unwrap_or(DEFAULT_WEIGHT_MS);
    let mut weighted: Vec<(String, u64)> = files
        .iter()
        .map(|f| (f.clone(), history.get(f).copied().unwrap_or(fallback)))
        .collect();
    weighted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    weighted.dedup_by(|a, b| a.0 == b.0);
    weighted
}

/// Plans `shards` shards; extra shards stay empty.
pub fn plan_shards(
    files: &[String],
    history: &BTreeMap<String, u64>,
    shards: usize,
) -> Result<Vec<Shard>, ShardPlanError> {
    if shards == 0 {
        return Err(ShardPlanError::ZeroShards);
    }
    let mut bins = vec![
        Shard {
            files: Vec::new(),
            estimated_ms: 0
        };
        shards
    ];
    for (file, weight) in weigh(files, history) {
        let lightest = bins
            .iter()
            .enumerate()
            .min_by_key(|(index, shard)| (shard.estimated_ms, *index))
            .map(|(index, _)| index)
            .expect("at least one shard");
        bins[lightest].estimated_ms += weight;
        bins[lightest].files.push(file);
    }
    Ok(bins)
}
