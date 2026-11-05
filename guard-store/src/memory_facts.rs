//! Orchestrator memory on Postgres (`memory_facts`, `memory_fact_candidates`;
//! `docs/contracts/memory.md`).
//!
//! - [`PgMemoryFacts`]: the write side ([`MemoryFactsWriter`]), used only by
//!   the orchestrator, connected as a member of `guard_orchestrator`.
//! - [`PgMemoryFactsReader`]: the read side ([`MemoryFactsReader`] only),
//!   connected with `GUARD_MEMORY_READ_DATABASE_URL` as a member of
//!   `guard_memory_reader` (`SELECT` on `memory_facts`, nothing else). Every
//!   session it opens is also `default_transaction_read_only`, so even a
//!   misconfigured privileged login cannot write through it.
//!
//! Writers of one topic are serialized with a transaction-scoped advisory
//! lock, so concurrent `record_fact` calls never race on the "one live fact
//! per topic" index.

use crate::pg::map_sqlx_error;
use crate::util::{format_ts, parse_ts, to_micros};
use guard_core::ports::{
    CandidateId, FactCandidate, FactId, FactScope, FactSource, FactStatus, FactsPage, FactsQuery,
    MemoryFact, MemoryFactsReader, MemoryFactsWriter, NewFact, PendingCandidate, StoreError,
};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Row};
use std::time::Duration;
use time::OffsetDateTime;

/// Connection URL of the read-only memory login (`guard mcp`, brief builder).
pub const MEMORY_READ_DATABASE_URL_ENV: &str = "GUARD_MEMORY_READ_DATABASE_URL";

const COLUMNS: &str = "id, repo, scope, scope_key, topic_key, body, commit_sha, run_id, \
                       evidence_uri, source, valid_from, invalid_at, recorded_at, updated_at, \
                       review_after, status, review_reason, supersedes, referenced_paths";

/// A fact with its last lifecycle change (`updated_at`, transaction time),
/// which [`MemoryFact`] does not carry. Used by the Engram export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFact {
    pub fact: MemoryFact,
    /// RFC 3339 UTC.
    pub updated_at: String,
}

// ---------------------------------------------------------------- names

pub(crate) fn scope_from(name: &str) -> Result<FactScope, StoreError> {
    match name {
        "repo" => Ok(FactScope::Repo),
        "area" => Ok(FactScope::Area),
        "journey" => Ok(FactScope::Journey),
        other => Err(StoreError::Invalid(format!("unknown fact scope {other:?}"))),
    }
}

pub(crate) fn status_from(name: &str) -> Result<FactStatus, StoreError> {
    match name {
        "active" => Ok(FactStatus::Active),
        "needs_review" => Ok(FactStatus::NeedsReview),
        "invalidated" => Ok(FactStatus::Invalidated),
        other => Err(StoreError::Invalid(format!(
            "unknown fact status {other:?}"
        ))),
    }
}

pub(crate) fn source_name(source: FactSource) -> &'static str {
    match source {
        FactSource::GateResult => "gate_result",
        FactSource::FailureFingerprint => "failure_fingerprint",
        FactSource::FlakyHistory => "flaky_history",
        FactSource::FixCommit => "fix_commit",
        FactSource::ApprovedDecision => "approved_decision",
        FactSource::ApprovedCandidate => "approved_candidate",
    }
}

pub(crate) fn source_from(name: &str) -> Result<FactSource, StoreError> {
    [
        FactSource::GateResult,
        FactSource::FailureFingerprint,
        FactSource::FlakyHistory,
        FactSource::FixCommit,
        FactSource::ApprovedDecision,
        FactSource::ApprovedCandidate,
    ]
    .into_iter()
    .find(|s| source_name(*s) == name)
    .ok_or_else(|| StoreError::Invalid(format!("unknown fact source {name:?}")))
}

// ---------------------------------------------------------------- rows

fn ts(row: &PgRow, column: &str) -> Result<String, StoreError> {
    row.try_get::<OffsetDateTime, _>(column)
        .map(format_ts)
        .map_err(map_sqlx_error)
}

fn opt_ts(row: &PgRow, column: &str) -> Result<Option<String>, StoreError> {
    row.try_get::<Option<OffsetDateTime>, _>(column)
        .map(|t| t.map(format_ts))
        .map_err(map_sqlx_error)
}

fn stored_fact(row: &PgRow) -> Result<StoredFact, StoreError> {
    let get_s = |c: &str| row.try_get::<String, _>(c).map_err(map_sqlx_error);
    let get_o = |c: &str| row.try_get::<Option<String>, _>(c).map_err(map_sqlx_error);
    let fact = MemoryFact {
        id: FactId(row.try_get("id").map_err(map_sqlx_error)?),
        repo: get_s("repo")?,
        scope: scope_from(&get_s("scope")?)?,
        scope_key: get_o("scope_key")?,
        topic_key: get_s("topic_key")?,
        body: get_s("body")?,
        commit_sha: get_s("commit_sha")?,
        run_id: get_o("run_id")?,
        evidence_uri: get_o("evidence_uri")?,
        source: source_from(&get_s("source")?)?,
        valid_from: ts(row, "valid_from")?,
        invalid_at: opt_ts(row, "invalid_at")?,
        recorded_at: ts(row, "recorded_at")?,
        review_after: opt_ts(row, "review_after")?,
        status: status_from(&get_s("status")?)?,
        review_reason: get_o("review_reason")?,
        supersedes: row
            .try_get::<Option<i64>, _>("supersedes")
            .map_err(map_sqlx_error)?
            .map(FactId),
        referenced_paths: row.try_get("referenced_paths").map_err(map_sqlx_error)?,
    };
    Ok(StoredFact {
        fact,
        updated_at: ts(row, "updated_at")?,
    })
}

fn memory_fact(row: &PgRow) -> Result<MemoryFact, StoreError> {
    stored_fact(row).map(|s| s.fact)
}

/// A validated [`NewFact`] with parsed timestamps.
struct Prepared<'a> {
    fact: &'a NewFact,
    valid_from: OffsetDateTime,
    review_after: Option<OffsetDateTime>,
}

impl<'a> Prepared<'a> {
    fn new(fact: &'a NewFact) -> Result<Self, StoreError> {
        fact.validate().map_err(StoreError::Invalid)?;
        let review_after = match &fact.review_after {
            Some(t) => Some(to_micros(parse_ts("review_after", t)?)),
            None => None,
        };
        Ok(Self {
            fact,
            valid_from: to_micros(parse_ts("valid_from", &fact.valid_from)?),
            review_after,
        })
    }

    fn lock_key(&self) -> String {
        let f = self.fact;
        format!(
            "memory_facts\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
            f.repo,
            f.scope.as_str(),
            f.scope_key.as_deref().unwrap_or(""),
            f.topic_key
        )
    }
}

// ---------------------------------------------------------------- shared reads

fn cursor_of(query: &FactsQuery) -> Result<i64, StoreError> {
    match &query.cursor {
        None => Ok(0),
        Some(c) => c
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| StoreError::Invalid(format!("bad cursor {c:?}"))),
    }
}

async fn list_facts_on(pool: &PgPool, query: &FactsQuery) -> Result<FactsPage, StoreError> {
    query.validate()?;
    let after = cursor_of(query)?;
    let statuses: Vec<String> = query
        .effective_statuses()
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    let as_of = match &query.as_of {
        Some(t) => Some(parse_ts("as_of", t)?),
        None => None,
    };
    let sql = format!(
        "SELECT {COLUMNS} FROM memory_facts
         WHERE repo = $1 AND id > $2 AND status = ANY($3)
           AND ($4::text IS NULL OR scope = $4)
           AND ($5::text IS NULL OR scope_key = $5)
           AND ($6::text IS NULL OR left(topic_key, length($6)) = $6)
           AND ($7::timestamptz IS NULL
                OR (valid_from <= $7 AND (invalid_at IS NULL OR $7 < invalid_at)))
         ORDER BY id
         LIMIT $8"
    );
    let rows = sqlx::query(&sql)
        .bind(&query.repo)
        .bind(after)
        .bind(&statuses)
        .bind(query.scope.map(FactScope::as_str))
        .bind(query.scope_key.as_deref())
        .bind(query.topic_prefix.as_deref())
        .bind(as_of)
        .bind(i64::from(query.limit) + 1)
        .fetch_all(pool)
        .await
        .map_err(map_sqlx_error)?;
    let mut facts = rows
        .iter()
        .map(memory_fact)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = (facts.len() > query.limit as usize).then(|| {
        facts.truncate(query.limit as usize);
        facts.last().map(|f| f.id.0.to_string()).unwrap_or_default()
    });
    Ok(FactsPage { facts, next_cursor })
}

async fn get_fact_on(pool: &PgPool, id: FactId) -> Result<Option<MemoryFact>, StoreError> {
    sqlx::query(&format!("SELECT {COLUMNS} FROM memory_facts WHERE id = $1"))
        .bind(id.0)
        .fetch_optional(pool)
        .await
        .map_err(map_sqlx_error)?
        .as_ref()
        .map(memory_fact)
        .transpose()
}

async fn topic_history_on(
    pool: &PgPool,
    repo: &str,
    topic_key: &str,
) -> Result<Vec<MemoryFact>, StoreError> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM memory_facts WHERE repo = $1 AND topic_key = $2 ORDER BY id"
    ))
    .bind(repo)
    .bind(topic_key)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)?
    .iter()
    .map(memory_fact)
    .collect()
}

/// Bi-temporal read (memory.md "Invariants"): the facts the store believed at
/// transaction time `recorded_as_of` about the code at valid time
/// `valid_as_of`. A fact counts when it was recorded by `recorded_as_of`,
/// `valid_from <= valid_as_of`, and it had not ended by `valid_as_of` — or its
/// end was recorded only after `recorded_as_of` (an invalidated row's
/// `updated_at` is when the invalidation was recorded: invalidated rows never
/// change again). Ordered by id; statuses are today's, not those at
/// `recorded_as_of`.
async fn believed_at_on(
    pool: &PgPool,
    repo: &str,
    recorded_as_of: &str,
    valid_as_of: &str,
) -> Result<Vec<MemoryFact>, StoreError> {
    let recorded = parse_ts("recorded_as_of", recorded_as_of)?;
    let valid = parse_ts("valid_as_of", valid_as_of)?;
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM memory_facts
         WHERE repo = $1 AND recorded_at <= $2 AND valid_from <= $3
           AND (invalid_at IS NULL OR $3 < invalid_at OR updated_at > $2)
         ORDER BY id"
    ))
    .bind(repo)
    .bind(recorded)
    .bind(valid)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)?
    .iter()
    .map(memory_fact)
    .collect()
}

/// Every fact of `repo` (any status) with its `updated_at`, ordered by id.
async fn stored_facts_on(pool: &PgPool, repo: &str) -> Result<Vec<StoredFact>, StoreError> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM memory_facts WHERE repo = $1 ORDER BY id"
    ))
    .bind(repo)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)?
    .iter()
    .map(stored_fact)
    .collect()
}

// ---------------------------------------------------------------- writes

/// Records `p` inside `conn`'s transaction. `Ok(Err(_))` is a rule violation.
async fn record_in(
    conn: &mut PgConnection,
    p: &Prepared<'_>,
    source: FactSource,
) -> Result<Result<FactId, String>, sqlx::Error> {
    let f = p.fact;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(p.lock_key())
        .execute(&mut *conn)
        .await?;
    let live = sqlx::query(
        "SELECT id, body, commit_sha, valid_from FROM memory_facts
         WHERE repo = $1 AND scope = $2 AND scope_key IS NOT DISTINCT FROM $3
           AND topic_key = $4 AND status <> 'invalidated'
         FOR UPDATE",
    )
    .bind(&f.repo)
    .bind(f.scope.as_str())
    .bind(f.scope_key.as_deref())
    .bind(&f.topic_key)
    .fetch_optional(&mut *conn)
    .await?;

    let mut supersedes = None;
    if let Some(row) = live {
        let id: i64 = row.try_get("id")?;
        let body: String = row.try_get("body")?;
        let commit: String = row.try_get("commit_sha")?;
        if body == f.body && commit == f.commit_sha {
            return Ok(Ok(FactId(id)));
        }
        let old_from: OffsetDateTime = row.try_get("valid_from")?;
        if p.valid_from < old_from {
            return Ok(Err(format!(
                "valid_from {} is before the live version {id}'s valid_from {}",
                format_ts(p.valid_from),
                format_ts(old_from)
            )));
        }
        sqlx::query(
            "UPDATE memory_facts SET status = 'invalidated', invalid_at = $2,
                                     review_reason = 'superseded'
             WHERE id = $1",
        )
        .bind(id)
        .bind(p.valid_from)
        .execute(&mut *conn)
        .await?;
        supersedes = Some(id);
    }

    let new_id: i64 = sqlx::query_scalar(
        "INSERT INTO memory_facts (repo, scope, scope_key, topic_key, body, commit_sha, run_id,
                                   evidence_uri, source, valid_from, review_after, supersedes,
                                   referenced_paths)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         RETURNING id",
    )
    .bind(&f.repo)
    .bind(f.scope.as_str())
    .bind(f.scope_key.as_deref())
    .bind(&f.topic_key)
    .bind(&f.body)
    .bind(&f.commit_sha)
    .bind(f.run_id.as_deref())
    .bind(f.evidence_uri.as_deref())
    .bind(source_name(source))
    .bind(p.valid_from)
    .bind(p.review_after)
    .bind(supersedes)
    .bind(&f.referenced_paths)
    .fetch_one(&mut *conn)
    .await?;

    if let Some(old) = supersedes {
        sqlx::query("UPDATE memory_facts SET review_reason = $2 WHERE id = $1")
            .bind(old)
            .bind(format!("superseded by {new_id}"))
            .execute(&mut *conn)
            .await?;
    }
    Ok(Ok(FactId(new_id)))
}

/// Locks fact `id` and returns its status; unknown ids are `Invalid`.
async fn lock_status(conn: &mut PgConnection, id: FactId) -> Result<FactStatus, StoreError> {
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM memory_facts WHERE id = $1 FOR UPDATE")
            .bind(id.0)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_sqlx_error)?;
    match status {
        Some(s) => status_from(&s),
        None => Err(StoreError::Invalid(format!("fact {} not found", id.0))),
    }
}

fn non_empty(field: &str, value: &str) -> Result<(), StoreError> {
    if value.trim().is_empty() {
        return Err(StoreError::Invalid(format!("{field} is empty")));
    }
    Ok(())
}

// ---------------------------------------------------------------- writer

/// The orchestrator's [`MemoryFactsWriter`] (and reader) on Postgres.
#[derive(Clone, Debug)]
pub struct PgMemoryFacts {
    pool: PgPool,
}

impl PgMemoryFacts {
    /// Uses `pool` (connected as a member of `guard_orchestrator`; migrations
    /// already applied, e.g. `PgStore::pool().clone()`).
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// See the bi-temporal rule on [`PgMemoryFactsReader::believed_at`].
    pub async fn believed_at(
        &self,
        repo: &str,
        recorded_as_of: &str,
        valid_as_of: &str,
    ) -> Result<Vec<MemoryFact>, StoreError> {
        believed_at_on(&self.pool, repo, recorded_as_of, valid_as_of).await
    }

    /// Every fact of `repo` with its `updated_at`, ordered by id.
    pub async fn stored_facts(&self, repo: &str) -> Result<Vec<StoredFact>, StoreError> {
        stored_facts_on(&self.pool, repo).await
    }

    async fn begin(&self) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StoreError> {
        self.pool.begin().await.map_err(map_sqlx_error)
    }
}

#[async_trait::async_trait]
impl MemoryFactsReader for PgMemoryFacts {
    async fn list_facts(&self, query: &FactsQuery) -> Result<FactsPage, StoreError> {
        list_facts_on(&self.pool, query).await
    }

    async fn get_fact(&self, id: FactId) -> Result<Option<MemoryFact>, StoreError> {
        get_fact_on(&self.pool, id).await
    }

    async fn topic_history(
        &self,
        repo: &str,
        topic_key: &str,
    ) -> Result<Vec<MemoryFact>, StoreError> {
        topic_history_on(&self.pool, repo, topic_key).await
    }
}

#[async_trait::async_trait]
impl MemoryFactsWriter for PgMemoryFacts {
    async fn record_fact(&self, fact: &NewFact) -> Result<FactId, StoreError> {
        let prepared = Prepared::new(fact)?;
        let mut tx = self.begin().await?;
        let id = record_in(&mut tx, &prepared, fact.source)
            .await
            .map_err(map_sqlx_error)?
            .map_err(StoreError::Invalid)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(id)
    }

    async fn mark_needs_review(&self, id: FactId, reason: &str) -> Result<(), StoreError> {
        non_empty("reason", reason)?;
        let mut tx = self.begin().await?;
        match lock_status(&mut tx, id).await? {
            FactStatus::NeedsReview => return Ok(()),
            FactStatus::Invalidated => {
                return Err(StoreError::Invalid(format!("fact {} is invalidated", id.0)));
            }
            FactStatus::Active => {}
        }
        sqlx::query(
            "UPDATE memory_facts SET status = 'needs_review', review_reason = $2 WHERE id = $1",
        )
        .bind(id.0)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn confirm(&self, id: FactId, review_after: Option<&str>) -> Result<(), StoreError> {
        let review_after = match review_after {
            Some(t) => Some(to_micros(parse_ts("review_after", t)?)),
            None => None,
        };
        let mut tx = self.begin().await?;
        if lock_status(&mut tx, id).await? == FactStatus::Invalidated {
            return Err(StoreError::Invalid(format!("fact {} is invalidated", id.0)));
        }
        sqlx::query(
            "UPDATE memory_facts SET status = 'active', review_reason = NULL,
                                     review_after = coalesce($2, review_after)
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(review_after)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    /// Already-invalidated facts are left unchanged (their `updated_at` is
    /// when the invalidation was recorded; see `believed_at`).
    async fn invalidate(
        &self,
        id: FactId,
        invalid_at: &str,
        reason: &str,
    ) -> Result<(), StoreError> {
        non_empty("reason", reason)?;
        let at = to_micros(parse_ts("invalid_at", invalid_at)?);
        let mut tx = self.begin().await?;
        if lock_status(&mut tx, id).await? == FactStatus::Invalidated {
            return Ok(());
        }
        sqlx::query(
            "UPDATE memory_facts SET status = 'invalidated', invalid_at = $2, review_reason = $3
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(at)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)
    }

    async fn enqueue_candidate(
        &self,
        candidate: &FactCandidate,
    ) -> Result<CandidateId, StoreError> {
        Prepared::new(&candidate.fact)?;
        non_empty("proposed_by", &candidate.proposed_by)?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO memory_fact_candidates (repo, fact, proposed_by, run_id)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(&candidate.fact.repo)
        .bind(Json(&candidate.fact))
        .bind(&candidate.proposed_by)
        .bind(candidate.fact.run_id.as_deref())
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(CandidateId(id))
    }

    async fn list_candidates(&self, repo: &str) -> Result<Vec<PendingCandidate>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, fact, proposed_by, created_at FROM memory_fact_candidates
             WHERE repo = $1 AND decision = 'pending' ORDER BY id",
        )
        .bind(repo)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter()
            .map(|row| {
                let Json(fact): Json<NewFact> = row.try_get("fact").map_err(map_sqlx_error)?;
                Ok(PendingCandidate {
                    id: CandidateId(row.try_get("id").map_err(map_sqlx_error)?),
                    candidate: FactCandidate {
                        fact,
                        proposed_by: row.try_get("proposed_by").map_err(map_sqlx_error)?,
                    },
                    created_at: ts(row, "created_at")?,
                })
            })
            .collect()
    }

    async fn approve_candidate(
        &self,
        id: CandidateId,
        approver: &str,
    ) -> Result<FactId, StoreError> {
        non_empty("approver", approver)?;
        let mut tx = self.begin().await?;
        let Json(fact): Json<NewFact> = pending_candidate(&mut tx, id).await?;
        let prepared = Prepared::new(&fact)?;
        let fact_id = record_in(&mut tx, &prepared, FactSource::ApprovedCandidate)
            .await
            .map_err(map_sqlx_error)?
            .map_err(StoreError::Invalid)?;
        sqlx::query(
            "UPDATE memory_fact_candidates
             SET decision = 'approved', decided_by = $2, decided_at = now(), fact_id = $3
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(approver)
        .bind(fact_id.0)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(fact_id)
    }

    async fn reject_candidate(
        &self,
        id: CandidateId,
        approver: &str,
        reason: &str,
    ) -> Result<(), StoreError> {
        non_empty("approver", approver)?;
        let mut tx = self.begin().await?;
        pending_candidate(&mut tx, id).await?;
        sqlx::query(
            "UPDATE memory_fact_candidates
             SET decision = 'rejected', decided_by = $2, decided_at = now(), reason = $3
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(approver)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)
    }
}

/// Locks a pending candidate and returns its fact.
async fn pending_candidate(
    conn: &mut PgConnection,
    id: CandidateId,
) -> Result<Json<NewFact>, StoreError> {
    sqlx::query_scalar(
        "SELECT fact FROM memory_fact_candidates WHERE id = $1 AND decision = 'pending'
         FOR UPDATE",
    )
    .bind(id.0)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_sqlx_error)?
    .ok_or_else(|| StoreError::Invalid(format!("candidate {} not pending", id.0)))
}

// ---------------------------------------------------------------- reader

/// The read-only [`MemoryFactsReader`] handed to `guard mcp` and the brief
/// builder. It has no write method, and its sessions are read-only.
#[derive(Clone, Debug)]
pub struct PgMemoryFactsReader {
    pool: PgPool,
}

impl PgMemoryFactsReader {
    /// Connects to `url` (a login that is a member of `guard_memory_reader`).
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err(StoreError::Unconfigured(format!(
                "{MEMORY_READ_DATABASE_URL_ENV} must be a postgres:// URL"
            )));
        }
        let options: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StoreError::Unconfigured(format!("{MEMORY_READ_DATABASE_URL_ENV}: {e}"))
        })?;
        Self::connect_with(options).await
    }

    /// Connects with the URL in `GUARD_MEMORY_READ_DATABASE_URL`.
    pub async fn from_env() -> Result<Self, StoreError> {
        let url = std::env::var(MEMORY_READ_DATABASE_URL_ENV).map_err(|_| {
            StoreError::Unconfigured(format!("{MEMORY_READ_DATABASE_URL_ENV} is not set"))
        })?;
        Self::connect(&url).await
    }

    /// Connects with `options`, adding `default_transaction_read_only=on`.
    pub async fn connect_with(options: PgConnectOptions) -> Result<Self, StoreError> {
        let options = options.options([("default_transaction_read_only", "on")]);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(map_sqlx_error)?;
        Ok(Self { pool })
    }

    /// The read-only pool (for tests and diagnostics).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    /// Bi-temporal read: what the store believed at transaction time
    /// `recorded_as_of` about the code at valid time `valid_as_of` (both RFC
    /// 3339). Ordered by id.
    pub async fn believed_at(
        &self,
        repo: &str,
        recorded_as_of: &str,
        valid_as_of: &str,
    ) -> Result<Vec<MemoryFact>, StoreError> {
        believed_at_on(&self.pool, repo, recorded_as_of, valid_as_of).await
    }

    /// Every fact of `repo` with its `updated_at`, ordered by id.
    pub async fn stored_facts(&self, repo: &str) -> Result<Vec<StoredFact>, StoreError> {
        stored_facts_on(&self.pool, repo).await
    }
}

#[async_trait::async_trait]
impl MemoryFactsReader for PgMemoryFactsReader {
    async fn list_facts(&self, query: &FactsQuery) -> Result<FactsPage, StoreError> {
        list_facts_on(&self.pool, query).await
    }

    async fn get_fact(&self, id: FactId) -> Result<Option<MemoryFact>, StoreError> {
        get_fact_on(&self.pool, id).await
    }

    async fn topic_history(
        &self,
        repo: &str,
        topic_key: &str,
    ) -> Result<Vec<MemoryFact>, StoreError> {
        topic_history_on(&self.pool, repo, topic_key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for s in [
            FactSource::GateResult,
            FactSource::FailureFingerprint,
            FactSource::FlakyHistory,
            FactSource::FixCommit,
            FactSource::ApprovedDecision,
            FactSource::ApprovedCandidate,
        ] {
            assert_eq!(source_from(source_name(s)).unwrap(), s);
            // Same spelling as the port's serde names (and the table check).
            assert_eq!(
                serde_json::to_value(s).unwrap(),
                serde_json::json!(source_name(s))
            );
        }
        for s in [FactScope::Repo, FactScope::Area, FactScope::Journey] {
            assert_eq!(scope_from(s.as_str()).unwrap(), s);
        }
        for s in [
            FactStatus::Active,
            FactStatus::NeedsReview,
            FactStatus::Invalidated,
        ] {
            assert_eq!(status_from(s.as_str()).unwrap(), s);
        }
        assert!(source_from("agent").is_err());
    }

    #[test]
    fn cursors_are_non_negative_ids() {
        let mut q = FactsQuery::for_repo("a/b");
        assert_eq!(cursor_of(&q).unwrap(), 0);
        q.cursor = Some("42".into());
        assert_eq!(cursor_of(&q).unwrap(), 42);
        for bad in ["-1", "x", ""] {
            q.cursor = Some(bad.into());
            assert!(cursor_of(&q).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn the_reader_rejects_non_postgres_urls() {
        assert!(matches!(
            PgMemoryFactsReader::connect("mysql://x").await,
            Err(StoreError::Unconfigured(_))
        ));
    }
}
