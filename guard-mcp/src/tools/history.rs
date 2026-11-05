//! `guard_history`: read-only, paginated access to the orchestrator's memory
//! facts (docs/contracts/memory.md, "`guard_history` MCP tool";
//! mcp-tools.md, Phase 4 tools).
//!
//! Backed by a [`MemoryFactsReader`] only: in production the read-only
//! `guard_store::PgMemoryFactsReader` (`GUARD_MEMORY_READ_DATABASE_URL`, a
//! `guard_memory_reader` login, read-only sessions). Nothing here can record,
//! approve or invalidate a fact; the reader trait has no such method.
//!
//! Every fact comes with its id, an evidence pointer (and a 1 h presigned URL
//! when an evidence store is configured) and a `needs_review` flag. The whole
//! `tools/call` result (pretty text plus `structuredContent`) is hard-capped
//! at [`MAX_HISTORY_RESULT_BYTES`]: facts that do not fit are left for the
//! next page (`next_cursor`), so a response stays well under 10k tokens.
//!
//! Wiring (integrator): add [`GUARD_HISTORY`] and [`spec`] to the tool list,
//! dispatch `tools/call` through [`parse_args`] and [`run`], and give the
//! server a [`HistoryContext`] when the reader URL is set (`None` answers
//! `server_not_configured`).

use super::ToolSpec;
use crate::args::ArgError;
use crate::error::ToolError;
use guard_core::ports::memory_facts::{DEFAULT_FACTS_PAGE, MAX_FACTS_PAGE};
use guard_core::ports::{
    EvidenceStore, FactScope, FactStatus, FactsQuery, MemoryFact, MemoryFactsReader, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;

pub const GUARD_HISTORY: &str = "guard_history";

/// Budget of one whole result: the pretty JSON text block plus the compact
/// `structuredContent` (the same value). At a conservative 3 bytes per token
/// that is at most 8k tokens.
pub const MAX_HISTORY_RESULT_BYTES: usize = 24_000;

/// Expiry of `evidence_url`.
pub const EVIDENCE_URL_EXPIRY: Duration = Duration::from_secs(3600);

/// The tool definition; schemas are the memory.md contract blocks.
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: GUARD_HISTORY,
        description: "Orchestrator memory: evidence-backed facts about this repo, its areas and \
                      journeys (flaky tests, recurring failures, fixes, decisions). Read-only and \
                      paginated; each fact has an id, an evidence pointer and a needs_review flag. \
                      With topic_key, every version of that topic.",
        input_schema: json!({"type": "object", "additionalProperties": false, "properties": {
          "scope": {"enum": ["repo", "area", "journey"]},
          "scope_key": {"type": "string"},
          "topic_prefix": {"type": "string"},
          "topic_key": {"type": "string", "description": "Exact topic: return every version (topic_history)"},
          "include_invalidated": {"type": "boolean", "default": false},
          "as_of": {"type": "string", "description": "RFC 3339 valid-time point: the versions valid then (valid_from <= as_of < invalid_at), superseded ones included"},
          "cursor": {"type": "string"},
          "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 20}
        }}),
        output_schema: json!({"type": "object", "required": ["repo", "facts", "next_cursor"], "properties": {
          "repo": {"type": "string"},
          "facts": {"type": "array", "maxItems": 50, "items": {"type": "object",
            "required": ["id", "scope", "topic_key", "body", "status", "needs_review", "commit_sha", "valid_from"],
            "properties": {
              "id": {"type": "integer"}, "scope": {"type": "string"}, "scope_key": {"type": ["string", "null"]},
              "topic_key": {"type": "string"}, "body": {"type": "string"},
              "status": {"enum": ["active", "needs_review", "invalidated"]}, "needs_review": {"type": "boolean"},
              "review_reason": {"type": ["string", "null"]},
              "commit_sha": {"type": "string"}, "run_id": {"type": ["string", "null"]},
              "evidence_uri": {"type": ["string", "null"]}, "evidence_url": {"type": ["string", "null"], "description": "presigned GET, 1 h"},
              "valid_from": {"type": "string"}, "invalid_at": {"type": ["string", "null"]},
              "supersedes": {"type": ["integer", "null"]}
            }}},
          "next_cursor": {"type": ["string", "null"]}
        }}),
        read_only: true,
        open_world: false,
    }
}

/// Validated `guard_history` arguments.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    #[serde(default)]
    pub scope: Option<FactScope>,
    #[serde(default)]
    pub scope_key: Option<String>,
    #[serde(default)]
    pub topic_prefix: Option<String>,
    #[serde(default)]
    pub topic_key: Option<String>,
    #[serde(default)]
    pub include_invalidated: bool,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    DEFAULT_FACTS_PAGE
}

/// Parses `tools/call` arguments (absent = `{}`); mismatches are JSON-RPC
/// invalid params, like every other tool's.
pub fn parse_args(arguments: Option<Map<String, Value>>) -> Result<HistoryRequest, ArgError> {
    let value = Value::Object(arguments.unwrap_or_default());
    let request: HistoryRequest =
        serde_json::from_value(value).map_err(|e| ArgError(format!("{GUARD_HISTORY}: {e}")))?;
    if !(1..=MAX_FACTS_PAGE).contains(&request.limit) {
        return Err(ArgError(format!(
            "{GUARD_HISTORY}: limit must be in 1..={MAX_FACTS_PAGE}"
        )));
    }
    Ok(request)
}

/// What the tool reads with. Holds a reader only.
#[derive(Clone)]
pub struct HistoryContext {
    pub reader: Arc<dyn MemoryFactsReader>,
    /// The repo `guard mcp` runs in (`owner/name`).
    pub repo: String,
    /// Presigns `evidence_url`; `None` leaves it null.
    pub evidence: Option<Arc<dyn EvidenceStore>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryFact {
    pub id: i64,
    pub scope: String,
    pub scope_key: Option<String>,
    pub topic_key: String,
    pub body: String,
    pub status: String,
    pub needs_review: bool,
    pub review_reason: Option<String>,
    pub commit_sha: String,
    pub run_id: Option<String>,
    pub evidence_uri: Option<String>,
    pub evidence_url: Option<String>,
    pub valid_from: String,
    pub invalid_at: Option<String>,
    pub supersedes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryOutput {
    pub repo: String,
    pub facts: Vec<HistoryFact>,
    pub next_cursor: Option<String>,
}

/// Bytes of the `tools/call` result `guard mcp` sends for `output`: the
/// pretty JSON text block plus the compact `structuredContent`.
pub fn result_bytes(output: &HistoryOutput) -> usize {
    let pretty = serde_json::to_string_pretty(output).map_or(0, |s| s.len());
    let compact = serde_json::to_string(output).map_or(0, |s| s.len());
    pretty + compact
}

fn store_error(e: StoreError) -> ToolError {
    match e {
        StoreError::Invalid(m) => ToolError::config(m),
        StoreError::Unconfigured(m) => ToolError::server_not_configured(m),
        StoreError::Unavailable(m) => ToolError::server(m),
    }
}

/// `s3://<bucket>/<key>` → `<key>`.
fn evidence_key(uri: &str) -> Option<&str> {
    let rest = uri.strip_prefix("s3://")?;
    let (_bucket, key) = rest.split_once('/')?;
    (!key.is_empty()).then_some(key)
}

fn to_history(f: MemoryFact, evidence: Option<&dyn EvidenceStore>) -> HistoryFact {
    let evidence_url = match (evidence, f.evidence_uri.as_deref().and_then(evidence_key)) {
        (Some(store), Some(key)) => store.presign_get(key, EVIDENCE_URL_EXPIRY).ok(),
        _ => None,
    };
    HistoryFact {
        id: f.id.0,
        scope: f.scope.as_str().to_string(),
        scope_key: f.scope_key,
        topic_key: f.topic_key,
        body: f.body,
        status: f.status.as_str().to_string(),
        needs_review: f.status == FactStatus::NeedsReview,
        review_reason: f.review_reason,
        commit_sha: f.commit_sha,
        run_id: f.run_id,
        evidence_uri: f.evidence_uri,
        evidence_url,
        valid_from: f.valid_from,
        invalid_at: f.invalid_at,
        supersedes: f.supersedes.map(|s| s.0),
    }
}

/// Drops trailing facts until the result fits [`MAX_HISTORY_RESULT_BYTES`];
/// the cursor then points after the last fact kept.
fn cap(mut output: HistoryOutput) -> HistoryOutput {
    let mut cut = false;
    while output.facts.len() > 1 && result_bytes(&output) > MAX_HISTORY_RESULT_BYTES {
        output.facts.pop();
        cut = true;
    }
    if cut {
        output.next_cursor = output.facts.last().map(|f| f.id.to_string());
    }
    output
}

/// Runs one call. `None` context = no reader URL configured.
pub async fn run(
    context: Option<&HistoryContext>,
    request: HistoryRequest,
) -> Result<HistoryOutput, ToolError> {
    let Some(ctx) = context else {
        return Err(ToolError::server_not_configured(
            "guard_history needs GUARD_MEMORY_READ_DATABASE_URL (a guard_memory_reader login)",
        ));
    };
    let evidence = ctx.evidence.as_deref();
    let limit = request.limit.clamp(1, MAX_FACTS_PAGE) as usize;
    let (facts, next_cursor) = match &request.topic_key {
        Some(topic) => {
            // Every version, any status, oldest first; the cursor skips ids
            // already returned when a long history spans several calls.
            let after = match &request.cursor {
                None => 0,
                Some(c) => c
                    .parse::<i64>()
                    .map_err(|_| ToolError::config(format!("bad cursor {c:?}")))?,
            };
            let mut versions: Vec<MemoryFact> = ctx
                .reader
                .topic_history(&ctx.repo, topic)
                .await
                .map_err(store_error)?
                .into_iter()
                .filter(|f| f.id.0 > after)
                .filter(|f| request.scope.is_none_or(|s| s == f.scope))
                .filter(|f| request.scope_key.is_none() || request.scope_key == f.scope_key)
                .collect();
            let more = versions.len() > limit;
            versions.truncate(limit);
            let next = more
                .then(|| versions.last().map(|f| f.id.0.to_string()))
                .flatten();
            (versions, next)
        }
        None => {
            let statuses = if request.include_invalidated {
                vec![
                    FactStatus::Active,
                    FactStatus::NeedsReview,
                    FactStatus::Invalidated,
                ]
            } else {
                vec![]
            };
            let query = FactsQuery {
                repo: ctx.repo.clone(),
                scope: request.scope,
                scope_key: request.scope_key.clone(),
                topic_prefix: request.topic_prefix.clone(),
                statuses,
                as_of: request.as_of.clone(),
                cursor: request.cursor.clone(),
                limit: limit as u32,
            };
            let page = ctx.reader.list_facts(&query).await.map_err(store_error)?;
            (page.facts, page.next_cursor)
        }
    };
    Ok(cap(HistoryOutput {
        repo: ctx.repo.clone(),
        facts: facts.into_iter().map(|f| to_history(f, evidence)).collect(),
        next_cursor,
    }))
}

#[cfg(test)]
mod tests;
