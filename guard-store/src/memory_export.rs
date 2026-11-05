//! Engram-compatible export of a repo's facts (docs/contracts/memory.md,
//! "Engram export"; schema `schemas/engram-export.schema.json`): the backup
//! format `engram import <file.json>` reads, format version `0.2.0`.
//!
//! The integrator wires `guard memory export --engram <file.json>` to
//! [`engram_export`] over `PgMemoryFacts::stored_facts`, which carries the
//! real `updated_at`. [`engram_export_from_reader`] works on any reader and
//! approximates `updated_at` by `invalid_at`, else `recorded_at`.
//!
//! Provisional (memory.md): the source → observation `type` mapping, and how
//! Engram's importer treats the session and compares timestamps. No import
//! into a real `engram` binary has been run.

use crate::memory_brief::all_facts;
use crate::memory_facts::StoredFact;
use guard_core::ports::{
    FactSource, FactStatus, FactsQuery, MemoryFact, MemoryFactsReader, StoreError,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// `currentExportVersion` of Engram's `internal/store/store.go`.
pub const ENGRAM_EXPORT_VERSION: &str = "0.2.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngramExport {
    pub version: String,
    pub exported_at: String,
    pub sessions: Vec<EngramSession>,
    pub observations: Vec<EngramObservation>,
    /// Always empty.
    pub prompts: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngramSession {
    pub id: String,
    pub project: String,
    pub directory: String,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngramObservation {
    pub id: i64,
    pub sync_id: String,
    pub session_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub content: String,
    pub project: String,
    pub scope: String,
    pub topic_key: String,
    pub revision_count: u32,
    pub duplicate_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by_sync_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

/// Engram observation type of a fact source (provisional mapping).
pub fn observation_type(source: FactSource) -> &'static str {
    match source {
        FactSource::GateResult | FactSource::FlakyHistory => "discovery",
        FactSource::FailureFingerprint | FactSource::FixCommit => "bugfix",
        FactSource::ApprovedDecision => "decision",
        FactSource::ApprovedCandidate => "pattern",
    }
}

fn sync_id(id: i64) -> String {
    format!("guard-fact-{id}")
}

fn scope_label(f: &MemoryFact) -> String {
    match &f.scope_key {
        Some(k) => format!("{}:{k}", f.scope.as_str()),
        None => f.scope.as_str().to_string(),
    }
}

fn content(f: &MemoryFact) -> String {
    let mut text = format!(
        "{}\n\nscope: {}\ncommit: {}",
        f.body,
        scope_label(f),
        f.commit_sha
    );
    if let Some(run) = &f.run_id {
        text.push_str(&format!("\nrun: {run}"));
    }
    if let Some(e) = &f.evidence_uri {
        text.push_str(&format!("\nevidence: {e}"));
    }
    text.push_str(&format!("\nstatus: {}", f.status.as_str()));
    text
}

fn topic_key(f: &MemoryFact) -> String {
    match &f.scope_key {
        Some(k) => format!("guard/{}/{k}/{}", f.scope.as_str(), f.topic_key),
        None => format!("guard/{}/{}", f.scope.as_str(), f.topic_key),
    }
}

/// The export of the facts of `repo` in `facts` (any order; emitted by id).
pub fn engram_export(repo: &str, facts: &[StoredFact], exported_at: &str) -> EngramExport {
    let session_id = format!("guard-{repo}");
    let mut sorted: Vec<&StoredFact> = facts.iter().filter(|s| s.fact.repo == repo).collect();
    sorted.sort_by_key(|s| s.fact.id);
    let superseded_by: BTreeMap<i64, i64> = sorted
        .iter()
        .filter_map(|s| s.fact.supersedes.map(|old| (old.0, s.fact.id.0)))
        .collect();
    let started_at = sorted
        .iter()
        .map(|s| s.fact.recorded_at.as_str())
        .min()
        .unwrap_or(exported_at)
        .to_string();
    let observations = sorted
        .iter()
        .map(|s| {
            let f = &s.fact;
            EngramObservation {
                id: f.id.0,
                sync_id: sync_id(f.id.0),
                session_id: session_id.clone(),
                kind: observation_type(f.source).to_string(),
                title: f.topic_key.clone(),
                content: content(f),
                project: repo.to_string(),
                scope: "project".to_string(),
                topic_key: topic_key(f),
                revision_count: 1,
                duplicate_count: 1,
                review_after: f.review_after.clone(),
                superseded_by_sync_id: superseded_by.get(&f.id.0).map(|n| sync_id(*n)),
                created_at: f.recorded_at.clone(),
                updated_at: s.updated_at.clone(),
                deleted_at: (f.status == FactStatus::Invalidated)
                    .then(|| f.invalid_at.clone())
                    .flatten(),
            }
        })
        .collect();
    EngramExport {
        version: ENGRAM_EXPORT_VERSION.to_string(),
        exported_at: exported_at.to_string(),
        sessions: vec![EngramSession {
            id: session_id,
            project: repo.to_string(),
            directory: String::new(),
            started_at,
        }],
        observations,
        prompts: vec![],
    }
}

/// [`engram_export`] over a reader (every status). `updated_at` is
/// approximated by `invalid_at`, else `recorded_at`.
pub async fn engram_export_from_reader(
    reader: &dyn MemoryFactsReader,
    repo: &str,
    exported_at: &str,
) -> Result<EngramExport, StoreError> {
    let query = FactsQuery {
        statuses: vec![
            FactStatus::Active,
            FactStatus::NeedsReview,
            FactStatus::Invalidated,
        ],
        ..FactsQuery::for_repo(repo)
    };
    let facts: Vec<StoredFact> = all_facts(reader, query)
        .await?
        .into_iter()
        .map(|fact| StoredFact {
            updated_at: fact
                .invalid_at
                .clone()
                .unwrap_or_else(|| fact.recorded_at.clone()),
            fact,
        })
        .collect();
    Ok(engram_export(repo, &facts, exported_at))
}
