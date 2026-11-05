//! Orchestrator memory: bi-temporal facts about a repo, its areas and journeys
//! (`docs/contracts/memory.md`).
//!
//! **Write side is orchestrator-only.** [`MemoryFactsWriter`] is implemented
//! by the Postgres adapter connected as the `guard_memory_writer` role and is
//! held only by the orchestrator process. Everything an agent can reach
//! (`guard mcp` `guard_history`, the facts brief in `task.json`) is built from
//! a [`MemoryFactsReader`] — in production an adapter connected as the
//! `guard_memory_reader` role, which the database grants `SELECT` only.
//! Agent suggestions arrive as [`super::agent::MemoryProposal`]s in
//! `result.json` and only become facts through the human-approval candidate
//! queue ([`MemoryFactsWriter::approve_candidate`]).
//!
//! Time model: `valid_from`/`invalid_at` is valid time (when the fact holds
//! for the code), `recorded_at` is transaction time (when the store learned
//! it). Rows are never deleted; a correction supersedes.

use super::decision_log::StoreError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Hard byte cap of the facts brief injected at sandbox start.
pub const FACTS_BRIEF_MAX_BYTES: usize = 4096;
/// First line of every brief (stable, so it is part of the cached prefix).
pub const FACTS_BRIEF_HEADER: &str =
    "# Repository facts (orchestrator memory; verify items marked needs_review)";
/// Largest fact body.
pub const MAX_FACT_BODY_BYTES: usize = 1024;
pub const MAX_TOPIC_KEY_LEN: usize = 120;
/// Largest page of [`MemoryFactsReader::list_facts`].
pub const MAX_FACTS_PAGE: u32 = 50;
pub const DEFAULT_FACTS_PAGE: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FactId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CandidateId(pub i64);

/// What a fact is about. `area` and `journey` need a `scope_key`
/// (`[areas]` key / journey id); `repo` must not have one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactScope {
    Repo,
    Area,
    Journey,
}

impl FactScope {
    pub fn as_str(self) -> &'static str {
        match self {
            FactScope::Repo => "repo",
            FactScope::Area => "area",
            FactScope::Journey => "journey",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactStatus {
    Active,
    /// Possibly stale (referenced file changed, `review_after` passed,
    /// fingerprint absent for N runs). Still served, flagged.
    NeedsReview,
    /// No longer true; never served in a brief.
    Invalidated,
}

impl FactStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FactStatus::Active => "active",
            FactStatus::NeedsReview => "needs_review",
            FactStatus::Invalidated => "invalidated",
        }
    }
}

/// Which orchestrator writer produced a fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    GateResult,
    FailureFingerprint,
    FlakyHistory,
    FixCommit,
    ApprovedDecision,
    /// An agent proposal a human approved.
    ApprovedCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFact {
    pub id: FactId,
    pub repo: String,
    pub scope: FactScope,
    pub scope_key: Option<String>,
    pub topic_key: String,
    pub body: String,
    /// Commit the fact was observed at.
    pub commit_sha: String,
    pub run_id: Option<String>,
    /// Pointer to evidence (`s3://<bucket>/<key>`), never inline bytes.
    pub evidence_uri: Option<String>,
    pub source: FactSource,
    pub valid_from: String,
    pub invalid_at: Option<String>,
    pub recorded_at: String,
    pub review_after: Option<String>,
    pub status: FactStatus,
    /// Why it is `needs_review` / `invalidated`.
    pub review_reason: Option<String>,
    /// The fact this one replaced (same repo, scope, scope_key, topic_key).
    pub supersedes: Option<FactId>,
    /// Repo-relative files whose change makes the fact `needs_review`.
    pub referenced_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFact {
    pub repo: String,
    pub scope: FactScope,
    pub scope_key: Option<String>,
    pub topic_key: String,
    pub body: String,
    pub commit_sha: String,
    pub run_id: Option<String>,
    pub evidence_uri: Option<String>,
    pub source: FactSource,
    /// RFC 3339 UTC.
    pub valid_from: String,
    pub review_after: Option<String>,
    pub referenced_paths: Vec<String>,
}

impl NewFact {
    pub fn validate(&self) -> Result<(), String> {
        if self.repo.trim().is_empty() {
            return Err("repo is empty".into());
        }
        if !is_topic_key(&self.topic_key) {
            return Err(format!(
                "topic_key {:?} must match ^[a-z0-9][a-z0-9._/-]*$ (max {MAX_TOPIC_KEY_LEN})",
                self.topic_key
            ));
        }
        match (self.scope, &self.scope_key) {
            (FactScope::Repo, Some(_)) => {
                return Err("scope_key must be absent for scope repo".into());
            }
            (FactScope::Area | FactScope::Journey, None) => {
                return Err("scope_key is required for scope area/journey".into());
            }
            (_, Some(k)) if k.trim().is_empty() => return Err("scope_key is empty".into()),
            _ => {}
        }
        if self.body.trim().is_empty() || self.body.len() > MAX_FACT_BODY_BYTES {
            return Err(format!("body must be 1..={MAX_FACT_BODY_BYTES} bytes"));
        }
        let sha_ok = (7..=64).contains(&self.commit_sha.len())
            && self
                .commit_sha
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !sha_ok {
            return Err("commit_sha must be lowercase hex".into());
        }
        if self.valid_from.trim().is_empty() {
            return Err("valid_from is empty".into());
        }
        Ok(())
    }
}

pub fn is_topic_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    key.len() <= MAX_TOPIC_KEY_LEN
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._/-".contains(&b))
}

/// An agent proposal waiting for a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactCandidate {
    /// `source` is overwritten with `approved_candidate` on approval.
    pub fact: NewFact,
    /// `<runtime>:<role>` of the proposing agent.
    pub proposed_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCandidate {
    pub id: CandidateId,
    pub candidate: FactCandidate,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsQuery {
    pub repo: String,
    pub scope: Option<FactScope>,
    pub scope_key: Option<String>,
    pub topic_prefix: Option<String>,
    /// Empty = `active` and `needs_review`.
    pub statuses: Vec<FactStatus>,
    /// Valid-time point: `valid_from <= as_of < invalid_at`.
    pub as_of: Option<String>,
    /// Opaque, from [`FactsPage::next_cursor`].
    pub cursor: Option<String>,
    /// `1..=MAX_FACTS_PAGE`.
    pub limit: u32,
}

impl FactsQuery {
    pub fn for_repo(repo: &str) -> Self {
        Self {
            repo: repo.to_string(),
            scope: None,
            scope_key: None,
            topic_prefix: None,
            statuses: vec![],
            as_of: None,
            cursor: None,
            limit: DEFAULT_FACTS_PAGE,
        }
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        if !(1..=MAX_FACTS_PAGE).contains(&self.limit) {
            return Err(StoreError::Invalid(format!(
                "limit must be in 1..={MAX_FACTS_PAGE}"
            )));
        }
        Ok(())
    }

    /// The statuses this query matches.
    pub fn effective_statuses(&self) -> Vec<FactStatus> {
        if self.statuses.is_empty() && self.as_of.is_some() {
            // A valid-time point must see versions superseded since then
            // (`invalid_at` is set iff the status is `invalidated`).
            vec![
                FactStatus::Active,
                FactStatus::NeedsReview,
                FactStatus::Invalidated,
            ]
        } else if self.statuses.is_empty() {
            vec![FactStatus::Active, FactStatus::NeedsReview]
        } else {
            self.statuses.clone()
        }
    }
}

/// Facts ordered by id ascending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPage {
    pub facts: Vec<MemoryFact>,
    pub next_cursor: Option<String>,
}

/// The memory injected into `task.json` (`facts_brief`), at most
/// [`FACTS_BRIEF_MAX_BYTES`] of UTF-8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsBrief {
    pub text: String,
    /// Facts included, in rendering order.
    pub fact_ids: Vec<FactId>,
    /// Some eligible fact did not fit.
    pub truncated: bool,
}

impl FactsBrief {
    /// Deterministic rendering (memory.md "Facts brief"): skips `invalidated`,
    /// orders by (scope, scope_key, topic_key, id), one line per fact, stops at
    /// the first line that would exceed `cap` bytes.
    pub fn render(facts: &[MemoryFact], cap: usize) -> FactsBrief {
        let cap = cap.min(FACTS_BRIEF_MAX_BYTES);
        let mut eligible: Vec<&MemoryFact> = facts
            .iter()
            .filter(|f| f.status != FactStatus::Invalidated)
            .collect();
        eligible.sort_by(|a, b| {
            (a.scope, &a.scope_key, &a.topic_key, a.id).cmp(&(
                b.scope,
                &b.scope_key,
                &b.topic_key,
                b.id,
            ))
        });
        let header = format!("{FACTS_BRIEF_HEADER}\n");
        if header.len() > cap {
            return FactsBrief {
                text: String::new(),
                fact_ids: vec![],
                truncated: !eligible.is_empty(),
            };
        }
        let mut text = header;
        let mut fact_ids = Vec::new();
        let mut truncated = false;
        for f in eligible {
            let scope = match &f.scope_key {
                Some(k) => format!("{}:{k}", f.scope.as_str()),
                None => f.scope.as_str().to_string(),
            };
            let body = f.body.split_whitespace().collect::<Vec<_>>().join(" ");
            let flag = if f.status == FactStatus::NeedsReview {
                " (needs_review)"
            } else {
                ""
            };
            let line = format!("- [{}] {scope} {}: {body}{flag}\n", f.id.0, f.topic_key);
            if text.len() + line.len() > cap {
                truncated = true;
                break;
            }
            text.push_str(&line);
            fact_ids.push(f.id);
        }
        FactsBrief {
            text,
            fact_ids,
            truncated,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > FACTS_BRIEF_MAX_BYTES {
            return Err(format!(
                "facts_brief is {} bytes, over the {FACTS_BRIEF_MAX_BYTES}-byte cap",
                self.text.len()
            ));
        }
        Ok(())
    }
}

/// Read side: everything `guard mcp` and the brief builder may use.
#[async_trait::async_trait]
pub trait MemoryFactsReader: Send + Sync {
    async fn list_facts(&self, query: &FactsQuery) -> Result<FactsPage, StoreError>;

    async fn get_fact(&self, id: FactId) -> Result<Option<MemoryFact>, StoreError>;

    /// Every version of one topic (any status), oldest first.
    async fn topic_history(
        &self,
        repo: &str,
        topic_key: &str,
    ) -> Result<Vec<MemoryFact>, StoreError>;
}

/// Write side, orchestrator only. Unknown ids are `StoreError::Invalid`.
#[async_trait::async_trait]
pub trait MemoryFactsWriter: MemoryFactsReader {
    /// Validates, then inserts. An `active`/`needs_review` fact with the same
    /// (repo, scope, scope_key, topic_key) becomes `invalidated` with
    /// `invalid_at = fact.valid_from`, and the new row `supersedes` it.
    /// Idempotent: when that live fact already has the same `body` and
    /// `commit_sha`, nothing changes and its id is returned.
    async fn record_fact(&self, fact: &NewFact) -> Result<FactId, StoreError>;

    /// `active` → `needs_review` (staleness job). No-op on `needs_review`.
    async fn mark_needs_review(&self, id: FactId, reason: &str) -> Result<(), StoreError>;

    /// A reviewed fact goes back to `active`, optionally with a new `review_after`.
    async fn confirm(&self, id: FactId, review_after: Option<&str>) -> Result<(), StoreError>;

    async fn invalidate(
        &self,
        id: FactId,
        invalid_at: &str,
        reason: &str,
    ) -> Result<(), StoreError>;

    async fn enqueue_candidate(&self, candidate: &FactCandidate)
    -> Result<CandidateId, StoreError>;

    /// Pending candidates of a repo, oldest first.
    async fn list_candidates(&self, repo: &str) -> Result<Vec<PendingCandidate>, StoreError>;

    /// Records the candidate as a fact (`source = approved_candidate`). A
    /// candidate is decided once.
    async fn approve_candidate(
        &self,
        id: CandidateId,
        approver: &str,
    ) -> Result<FactId, StoreError>;

    async fn reject_candidate(
        &self,
        id: CandidateId,
        approver: &str,
        reason: &str,
    ) -> Result<(), StoreError>;
}

/// A reader view of a writer that cannot be turned back into the writer.
pub struct ReadOnlyFacts(Arc<dyn MemoryFactsWriter>);

/// Hands a reader-only trait object to code that must not write.
pub fn read_only(writer: Arc<dyn MemoryFactsWriter>) -> Arc<dyn MemoryFactsReader> {
    Arc::new(ReadOnlyFacts(writer))
}

#[async_trait::async_trait]
impl MemoryFactsReader for ReadOnlyFacts {
    async fn list_facts(&self, query: &FactsQuery) -> Result<FactsPage, StoreError> {
        self.0.list_facts(query).await
    }

    async fn get_fact(&self, id: FactId) -> Result<Option<MemoryFact>, StoreError> {
        self.0.get_fact(id).await
    }

    async fn topic_history(
        &self,
        repo: &str,
        topic_key: &str,
    ) -> Result<Vec<MemoryFact>, StoreError> {
        self.0.topic_history(repo, topic_key).await
    }
}
