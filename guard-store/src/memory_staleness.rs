//! The staleness job (docs/contracts/memory.md, "Staleness"). Runs in the
//! orchestrator after each merge to the default branch and nightly. An
//! `active` fact becomes `needs_review` when, checked in this order:
//!
//! 1. a commit after its `commit_sha` touched one of its `referenced_paths`,
//!    or a file whose fan-out (direct importers) reaches one of them — reason
//!    `"<path> changed in <short sha>"`;
//! 2. its `review_after` is before `now` — `"review_after passed"`;
//! 3. it is a `failure/` or `fingerprint/` fact whose fingerprint is absent
//!    from the last N gate runs of its journey — `"fingerprint absent for N runs"`.
//!
//! Git, the import graph and gate history come in through the small traits
//! below so the job runs the same against fakes and production adapters
//! (`git log --name-only <sha>..HEAD`, `guard-impact`, [`GateHistoryRuns`]).

use crate::util::parse_ts;
use guard_core::fingerprint::LABEL_PREFIX;
use guard_core::ports::memory_facts::MAX_FACTS_PAGE;
use guard_core::ports::{
    FactId, FactScope, FactStatus, FactsQuery, GateHistory, MemoryFact, MemoryFactsWriter,
    StoreError,
};
use std::collections::{BTreeMap, BTreeSet};

/// N of rule 3.
pub const DEFAULT_FINGERPRINT_WINDOW: u32 = 20;
/// Gate results [`GateHistoryRuns`] scans to find the last N runs of a journey.
pub const DEFAULT_GATE_SCAN: u32 = 500;

/// A file changed after some commit, with the newest commit that touched it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedFile {
    /// Repo-relative, forward slashes.
    pub path: String,
    pub commit: String,
}

/// "Which files did commits after `since` touch?" (`git log --name-only
/// --format=%H <since>..HEAD`, newest commit per path).
#[async_trait::async_trait]
pub trait TouchedFiles: Send + Sync {
    async fn touched_since(&self, repo: &str, since: &str) -> Result<Vec<TouchedFile>, StoreError>;
}

/// The import graph: files that import `path` directly (its fan-out).
pub trait FanOutReach: Send + Sync {
    fn dependents(&self, path: &str) -> Vec<String>;
}

/// Fingerprint labels seen in the last runs of a journey.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentRuns {
    /// Runs looked at (at most the `n` asked for).
    pub runs: u32,
    /// `guard-fp-<8hex>` labels of any test in those runs.
    pub labels: BTreeSet<String>,
}

#[async_trait::async_trait]
pub trait FingerprintRuns: Send + Sync {
    /// The last `n` gate runs of `repo` that ran `journey` (every run of the
    /// repo when `None`), newest first.
    async fn recent(
        &self,
        repo: &str,
        journey: Option<&str>,
        n: u32,
    ) -> Result<RecentRuns, StoreError>;
}

/// [`FingerprintRuns`] over any [`GateHistory`] (Postgres or in memory).
pub struct GateHistoryRuns<'a> {
    history: &'a dyn GateHistory,
    scan: u32,
}

impl<'a> GateHistoryRuns<'a> {
    pub fn new(history: &'a dyn GateHistory) -> Self {
        Self {
            history,
            scan: DEFAULT_GATE_SCAN,
        }
    }

    /// Looks at the newest `scan` gate results (all repos) at most.
    pub fn with_scan(mut self, scan: u32) -> Self {
        self.scan = scan.max(1);
        self
    }
}

#[async_trait::async_trait]
impl FingerprintRuns for GateHistoryRuns<'_> {
    async fn recent(
        &self,
        repo: &str,
        journey: Option<&str>,
        n: u32,
    ) -> Result<RecentRuns, StoreError> {
        let results = self.history.recent_gate_results(self.scan).await?;
        let mut out = RecentRuns::default();
        for r in results
            .iter()
            .filter(|r| r.report.repo.as_deref() == Some(repo))
        {
            if out.runs >= n {
                break;
            }
            let tests: Vec<_> = r
                .report
                .tests
                .iter()
                .filter(|t| journey.is_none() || t.journey.as_deref() == journey)
                .collect();
            if journey.is_some() && tests.is_empty() {
                continue;
            }
            out.runs += 1;
            out.labels.extend(
                tests
                    .iter()
                    .filter_map(|t| t.fingerprint.as_ref())
                    .map(|f| f.label()),
            );
        }
        Ok(out)
    }
}

pub struct StalenessSources<'a> {
    pub touched: &'a dyn TouchedFiles,
    /// Rule 1 through fan-out; `None` checks direct changes only.
    pub fan_out: Option<&'a dyn FanOutReach>,
    /// Rule 3; `None` skips it.
    pub fingerprints: Option<&'a dyn FingerprintRuns>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalenessConfig {
    /// RFC 3339.
    pub now: String,
    /// N of rule 3.
    pub fingerprint_window: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StalenessReport {
    /// `active` facts looked at.
    pub checked: usize,
    /// Facts marked `needs_review`, in id order, with the reason.
    pub marked: Vec<(FactId, String)>,
}

/// The fingerprint label of a `failure/` or `fingerprint/` topic.
fn fingerprint_label(topic: &str) -> Option<&str> {
    let label = topic
        .strip_prefix("failure/")
        .or_else(|| topic.strip_prefix("fingerprint/"))?;
    label.starts_with(LABEL_PREFIX).then_some(label)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

async fn active_facts(
    writer: &dyn MemoryFactsWriter,
    repo: &str,
) -> Result<Vec<MemoryFact>, StoreError> {
    let mut query = FactsQuery {
        statuses: vec![FactStatus::Active],
        limit: MAX_FACTS_PAGE,
        ..FactsQuery::for_repo(repo)
    };
    let mut facts = vec![];
    loop {
        let page = writer.list_facts(&query).await?;
        facts.extend(page.facts);
        match page.next_cursor {
            Some(c) => query.cursor = Some(c),
            None => return Ok(facts),
        }
    }
}

/// Runs the three rules over every `active` fact of `repo`.
pub async fn run_staleness(
    writer: &dyn MemoryFactsWriter,
    repo: &str,
    sources: &StalenessSources<'_>,
    config: &StalenessConfig,
) -> Result<StalenessReport, StoreError> {
    let now = parse_ts("now", &config.now)?;
    let facts = active_facts(writer, repo).await?;
    let mut touched_cache: BTreeMap<String, Vec<TouchedFile>> = BTreeMap::new();
    let mut runs_cache: BTreeMap<Option<String>, RecentRuns> = BTreeMap::new();
    let mut report = StalenessReport {
        checked: facts.len(),
        ..StalenessReport::default()
    };

    for fact in &facts {
        let mut reason = None;

        // 1. Referenced files touched since the fact's commit.
        if !fact.referenced_paths.is_empty() {
            if !touched_cache.contains_key(&fact.commit_sha) {
                let touched = sources
                    .touched
                    .touched_since(repo, &fact.commit_sha)
                    .await?;
                touched_cache.insert(fact.commit_sha.clone(), touched);
            }
            let referenced: BTreeSet<&str> =
                fact.referenced_paths.iter().map(String::as_str).collect();
            for change in &touched_cache[&fact.commit_sha] {
                let reaches = referenced.contains(change.path.as_str())
                    || sources.fan_out.is_some_and(|g| {
                        g.dependents(&change.path)
                            .iter()
                            .any(|d| referenced.contains(d.as_str()))
                    });
                if reaches {
                    reason = Some(format!(
                        "{} changed in {}",
                        change.path,
                        short(&change.commit)
                    ));
                    break;
                }
            }
        }

        // 2. The review deadline passed.
        if reason.is_none() {
            if let Some(deadline) = &fact.review_after {
                if parse_ts("review_after", deadline)? < now {
                    reason = Some("review_after passed".to_string());
                }
            }
        }

        // 3. The fingerprint stopped showing up.
        if reason.is_none() {
            if let (Some(runs), Some(label)) =
                (sources.fingerprints, fingerprint_label(&fact.topic_key))
            {
                let journey = (fact.scope == FactScope::Journey)
                    .then(|| fact.scope_key.clone())
                    .flatten();
                if !runs_cache.contains_key(&journey) {
                    let recent = runs
                        .recent(repo, journey.as_deref(), config.fingerprint_window)
                        .await?;
                    runs_cache.insert(journey.clone(), recent);
                }
                let recent = &runs_cache[&journey];
                if recent.runs >= config.fingerprint_window && !recent.labels.contains(label) {
                    reason = Some(format!(
                        "fingerprint absent for {} runs",
                        config.fingerprint_window
                    ));
                }
            }
        }

        if let Some(reason) = reason {
            writer.mark_needs_review(fact.id, &reason).await?;
            report.marked.push((fact.id, reason));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_topics_carry_their_label() {
        assert_eq!(
            fingerprint_label("failure/guard-fp-1a2b3c4d"),
            Some("guard-fp-1a2b3c4d")
        );
        assert_eq!(
            fingerprint_label("fingerprint/guard-fp-1a2b3c4d"),
            Some("guard-fp-1a2b3c4d")
        );
        assert_eq!(fingerprint_label("flaky/guard-fp-1a2b3c4d"), None);
        assert_eq!(fingerprint_label("failure/other"), None);
    }
}
