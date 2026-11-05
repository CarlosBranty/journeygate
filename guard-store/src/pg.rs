//! `PgStore`: `DecisionLog`, `GateHistory`, `FlakyHistory` and `JobQueue` on
//! Postgres through sqlx's runtime API (no compile-time `query!`, so building
//! never needs a database). Schema: `guard-store/migrations/`, contract:
//! `docs/contracts/decision-log.md`.

use crate::flaky::{FlakyHistory, FlakyQuery, FlakyStat, aggregate, status_name};
use crate::jobs::{Enqueued, Job, JobId, JobQueue, JobStatus, NewJob, clamp_delay, lease_lost};
use crate::util::{
    area_of, check_decision, format_ts, kind_from, lane_kind_from, lane_kind_name, lane_parts,
    mode_name, normalize_label, parse_ts, source_from, source_name,
};
use guard_core::ports::*;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::time::Duration;
use time::OffsetDateTime;

/// Embedded migrations (`guard-store/migrations/`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Environment variable holding the connection URL.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";

#[derive(Clone, Debug)]
pub struct PgStore {
    pool: PgPool,
}

/// Maps sqlx errors as `docs/contracts/decision-log.md` § Error mapping says.
pub fn map_sqlx_error(e: sqlx::Error) -> StoreError {
    use sqlx::Error as E;
    match e {
        E::Configuration(err) => StoreError::Unconfigured(err.to_string()),
        E::Database(_)
        | E::ColumnDecode { .. }
        | E::Decode(_)
        | E::Encode(_)
        | E::ColumnNotFound(_)
        | E::ColumnIndexOutOfBounds { .. }
        | E::TypeNotFound { .. }
        | E::RowNotFound => StoreError::Invalid(e.to_string()),
        // PoolTimedOut, PoolClosed, Io, Tls, Protocol, WorkerCrashed, ...
        other => StoreError::Unavailable(other.to_string()),
    }
}

fn map_migrate_error(e: sqlx::migrate::MigrateError) -> StoreError {
    match e {
        sqlx::migrate::MigrateError::Execute(inner) => map_sqlx_error(inner),
        other => StoreError::Invalid(format!("migrations: {other}")),
    }
}

fn micros(d: Duration) -> i64 {
    i64::try_from(clamp_delay(d).as_micros()).unwrap_or(i64::MAX)
}

impl PgStore {
    /// Connects to `url` (`postgres://user:pass@host:5432/guard`): pool of
    /// 10 connections, 5 s acquire timeout. Does not run migrations.
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err(StoreError::Unconfigured(format!(
                "{DATABASE_URL_ENV} must be a postgres:// URL"
            )));
        }
        let options: PgConnectOptions = url.parse().map_err(|e: sqlx::Error| {
            StoreError::Unconfigured(format!("{DATABASE_URL_ENV}: {e}"))
        })?;
        Self::connect_with(options).await
    }

    /// Connects with the URL in `DATABASE_URL`.
    pub async fn from_env() -> Result<Self, StoreError> {
        let url = std::env::var(DATABASE_URL_ENV)
            .map_err(|_| StoreError::Unconfigured(format!("{DATABASE_URL_ENV} is not set")))?;
        Self::connect(&url).await
    }

    pub async fn connect_with(options: PgConnectOptions) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(options)
            .await
            .map_err(map_sqlx_error)?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies pending migrations (`guard serve` startup). Idempotent.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        MIGRATOR.run(&self.pool).await.map_err(map_migrate_error)
    }

    /// `SELECT 1`, for `/healthz`.
    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(map_sqlx_error)
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }
}

// ---------------------------------------------------------------- decision log

fn calibration_row(row: &PgRow) -> Result<CalibrationRow, StoreError> {
    let decision: Json<TriageDecision> = row.try_get("decision").map_err(map_sqlx_error)?;
    let human_label_id: Option<i64> = row.try_get("human_label_id").map_err(map_sqlx_error)?;
    let human = match human_label_id {
        None => None,
        Some(_) => {
            let get_s = |c: &str| -> Result<Option<String>, StoreError> {
                row.try_get::<Option<String>, _>(c).map_err(map_sqlx_error)
            };
            let labeled_at: OffsetDateTime = row.try_get("labeled_at").map_err(map_sqlx_error)?;
            Some(HumanLabel {
                ticket_key: row.try_get("ticket_key").map_err(map_sqlx_error)?,
                labeled_at: format_ts(labeled_at),
                area: area_of(get_s("human_area")?),
                kind: get_s("human_kind")?.as_deref().map(kind_from).transpose()?,
                lane: get_s("human_lane")?
                    .as_deref()
                    .map(lane_kind_from)
                    .transpose()?,
                flow_change: row.try_get("human_flow_change").map_err(map_sqlx_error)?,
                labels: row
                    .try_get::<Option<Vec<String>>, _>("human_labels")
                    .map_err(map_sqlx_error)?
                    .unwrap_or_default(),
                source: source_from(&get_s("human_source")?.unwrap_or_default())?,
            })
        }
    };
    Ok(CalibrationRow {
        decision_id: DecisionId(row.try_get("decision_id").map_err(map_sqlx_error)?),
        decision: decision.0,
        human,
    })
}

#[async_trait::async_trait]
impl DecisionLog for PgStore {
    async fn record_decision(&self, d: &TriageDecision) -> Result<DecisionId, StoreError> {
        let decided_at = check_decision(d)?;
        let (decided_lane, decided_reasons) = lane_parts(&d.decided);
        let (effective_lane, effective_reasons) = lane_parts(&d.effective);
        let a = d.answers.as_ref();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO triage_decisions (
                ticket_key, decided_at, mode, engine_model, engine_error, permutation_seed,
                decided_lane, decided_reasons, effective_lane, effective_reasons,
                area, area_confidence, kind, kind_confidence, risk_score, risk_confidence,
                clear_e2e_criteria, touches_money_or_security, agent_suitable,
                changes_existing_flow, decision)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                     $17, $18, $19, $20, $21)
             RETURNING id",
        )
        .bind(&d.ticket_key)
        .bind(decided_at)
        .bind(mode_name(d.mode))
        .bind(a.map(|a| a.model.clone()))
        .bind(&d.engine_error)
        // The u64 seed reinterpreted bit-for-bit as i64.
        .bind(d.permutation_seed.map(|s| s as i64))
        .bind(decided_lane)
        .bind(decided_reasons)
        .bind(effective_lane)
        .bind(effective_reasons)
        .bind(a.map(|a| a.area.value.0.clone()))
        .bind(a.map(|a| a.area.confidence))
        .bind(a.map(|a| a.kind.value.as_str()))
        .bind(a.map(|a| a.kind.confidence))
        .bind(a.map(|a| a.risk.value))
        .bind(a.map(|a| a.risk.confidence))
        .bind(a.map(|a| a.clear_e2e_criteria))
        .bind(a.map(|a| a.touches_money_or_security))
        .bind(a.map(|a| a.agent_suitable))
        .bind(a.map(|a| a.changes_existing_flow))
        .bind(Json(d))
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(DecisionId(id))
    }

    async fn record_human_label(&self, label: &HumanLabel) -> Result<(), StoreError> {
        let (l, labeled_at) = normalize_label(label)?;
        sqlx::query(
            "INSERT INTO human_labels
                (ticket_key, labeled_at, area, kind, lane, flow_change, labels, source)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(&l.ticket_key)
        .bind(labeled_at)
        .bind(l.area.as_ref().map(|a| a.0.clone()))
        .bind(l.kind.map(Kind::as_str))
        .bind(l.lane.map(lane_kind_name))
        .bind(l.flow_change)
        .bind(&l.labels)
        .bind(source_name(l.source))
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }

    async fn list_for_calibration(
        &self,
        query: &CalibrationQuery,
    ) -> Result<Vec<CalibrationRow>, StoreError> {
        let since = query
            .since
            .as_deref()
            .map(|v| parse_ts("since", v))
            .transpose()?;
        let limit = if query.limit == 0 {
            1000
        } else {
            i64::from(query.limit)
        };
        let rows = sqlx::query(
            "SELECT * FROM calibration
             WHERE ($1::timestamptz IS NULL OR decided_at >= $1)
               AND ($2::text IS NULL OR mode = $2)
               AND (NOT $3 OR human_label_id IS NOT NULL)
             ORDER BY decided_at DESC, decision_id DESC
             LIMIT $4",
        )
        .bind(since)
        .bind(query.mode.map(mode_name))
        .bind(query.labeled_only)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(calibration_row).collect()
    }
}

// ---------------------------------------------------------------- gate history

fn stored_gate(row: &PgRow) -> Result<StoredGateResult, StoreError> {
    let received_at: OffsetDateTime = row.try_get("received_at").map_err(map_sqlx_error)?;
    let report: Json<GateReport> = row.try_get("report").map_err(map_sqlx_error)?;
    let outcome: Json<ReportOutcome> = row.try_get("outcome").map_err(map_sqlx_error)?;
    Ok(StoredGateResult {
        id: GateResultId(row.try_get("id").map_err(map_sqlx_error)?),
        received_at: format_ts(received_at),
        report: report.0,
        outcome: outcome.0,
    })
}

const GATE_COLUMNS: &str = "id, received_at, report, outcome";

#[async_trait::async_trait]
impl GateHistory for PgStore {
    async fn record_gate_result(
        &self,
        report: &GateReport,
        outcome: &ReportOutcome,
    ) -> Result<GateResultId, StoreError> {
        let regressions = i32::try_from(report.counts.regressions)
            .map_err(|_| StoreError::Invalid("regressions out of range".into()))?;
        let pr_number = report
            .pr
            .as_ref()
            .map(|p| i64::try_from(p.number))
            .transpose()
            .map_err(|_| StoreError::Invalid("pr number out of range".into()))?;
        let provider = report.provider.map(|p| match p {
            CodeHostProvider::Github => "github",
            CodeHostProvider::Gitlab => "gitlab",
        });
        let verdict = match report.verdict {
            GateVerdict::Green => "green",
            GateVerdict::Red => "red",
        };

        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO gate_results (repo, run_id, provider, pr_number, ticket_key, head_sha,
                                       base_sha, verdict, regressions, report, outcome)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (repo, run_id) DO UPDATE SET
                provider = EXCLUDED.provider, pr_number = EXCLUDED.pr_number,
                ticket_key = EXCLUDED.ticket_key, head_sha = EXCLUDED.head_sha,
                base_sha = EXCLUDED.base_sha, verdict = EXCLUDED.verdict,
                regressions = EXCLUDED.regressions, report = EXCLUDED.report,
                outcome = EXCLUDED.outcome, received_at = now()
             RETURNING id",
        )
        .bind(report.repo.as_deref().unwrap_or(""))
        .bind(&report.run_id)
        .bind(provider)
        .bind(pr_number)
        .bind(&report.ticket_key)
        .bind(&report.head_sha)
        .bind(&report.base_sha)
        .bind(verdict)
        .bind(regressions)
        .bind(Json(report))
        .bind(Json(outcome))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;

        sqlx::query("DELETE FROM gate_test_results WHERE gate_result_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        if !report.tests.is_empty() {
            let ids: Vec<&str> = report.tests.iter().map(|t| t.test_id.as_str()).collect();
            let journeys: Vec<Option<&str>> =
                report.tests.iter().map(|t| t.journey.as_deref()).collect();
            let statuses: Vec<&str> = report.tests.iter().map(|t| status_name(t.status)).collect();
            let blocking: Vec<bool> = report.tests.iter().map(|t| t.blocking).collect();
            let attempts: Vec<i32> = report
                .tests
                .iter()
                .map(|t| i32::try_from(t.attempts).unwrap_or(i32::MAX))
                .collect();
            sqlx::query(
                "INSERT INTO gate_test_results
                    (gate_result_id, test_id, journey, status, blocking, attempts)
                 SELECT $1, * FROM UNNEST($2::text[], $3::text[], $4::text[], $5::bool[], $6::int4[])",
            )
            .bind(id)
            .bind(ids)
            .bind(journeys)
            .bind(statuses)
            .bind(blocking)
            .bind(attempts)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(GateResultId(id))
    }

    async fn recent_gate_results(&self, limit: u32) -> Result<Vec<StoredGateResult>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {GATE_COLUMNS} FROM gate_results ORDER BY received_at DESC, id DESC LIMIT $1"
        ))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        rows.iter().map(stored_gate).collect()
    }

    async fn latest_for_pr(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Option<StoredGateResult>, StoreError> {
        let Ok(number) = i64::try_from(number) else {
            return Ok(None);
        };
        let row = sqlx::query(&format!(
            "SELECT {GATE_COLUMNS} FROM gate_results WHERE repo = $1 AND pr_number = $2
             ORDER BY received_at DESC, id DESC LIMIT 1"
        ))
        .bind(repo)
        .bind(number)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(stored_gate).transpose()
    }
}

#[async_trait::async_trait]
impl FlakyHistory for PgStore {
    async fn flaky_stats(&self, query: &FlakyQuery) -> Result<Vec<FlakyStat>, StoreError> {
        let rows: Vec<(String, Option<String>, String)> = sqlx::query_as(
            "WITH recent AS (
                SELECT id, received_at FROM gate_results
                WHERE ($1::text IS NULL OR repo = $1)
                ORDER BY received_at DESC, id DESC
                LIMIT $2)
             SELECT t.test_id, t.journey, t.status
             FROM recent r JOIN gate_test_results t ON t.gate_result_id = r.id
             ORDER BY r.received_at DESC, r.id DESC",
        )
        .bind(&query.repo)
        .bind(i64::from(query.effective_window()))
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        let parsed = rows
            .iter()
            .map(|(id, journey, status)| {
                let status = match status.as_str() {
                    "passed" => TestStatus::Passed,
                    "flaky" => TestStatus::Flaky,
                    "failed" => TestStatus::Failed,
                    "skipped" => TestStatus::Skipped,
                    other => {
                        return Err(StoreError::Invalid(format!(
                            "unknown test status {other:?}"
                        )));
                    }
                };
                Ok((id.as_str(), journey.as_deref(), status))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(aggregate(parsed))
    }
}

// ---------------------------------------------------------------- job queue

const JOB_COLUMNS: &str = "id, queue, dedupe_key, payload, priority, status, attempts, max_attempts, locked_by, last_error";

fn job_row(row: &PgRow) -> Result<Job, StoreError> {
    let status: String = row.try_get("status").map_err(map_sqlx_error)?;
    let attempts: i32 = row.try_get("attempts").map_err(map_sqlx_error)?;
    let max_attempts: i32 = row.try_get("max_attempts").map_err(map_sqlx_error)?;
    let payload: Json<serde_json::Value> = row.try_get("payload").map_err(map_sqlx_error)?;
    Ok(Job {
        id: JobId(row.try_get("id").map_err(map_sqlx_error)?),
        queue: row.try_get("queue").map_err(map_sqlx_error)?,
        dedupe_key: row.try_get("dedupe_key").map_err(map_sqlx_error)?,
        payload: payload.0,
        priority: row.try_get("priority").map_err(map_sqlx_error)?,
        status: JobStatus::from_name(&status)
            .ok_or_else(|| StoreError::Invalid(format!("unknown job status {status:?}")))?,
        attempts: attempts.max(0) as u32,
        max_attempts: max_attempts.max(0) as u32,
        locked_by: row.try_get("locked_by").map_err(map_sqlx_error)?,
        last_error: row.try_get("last_error").map_err(map_sqlx_error)?,
    })
}

impl PgStore {
    async fn held_update(
        &self,
        id: JobId,
        worker: &str,
        sql: &str,
        bind_text: Option<&str>,
        bind_micros: Option<i64>,
    ) -> Result<(), StoreError> {
        let mut q = sqlx::query(sql).bind(id.0).bind(worker);
        if let Some(text) = bind_text {
            q = q.bind(text);
        }
        if let Some(m) = bind_micros {
            q = q.bind(m);
        }
        let done = q.execute(&self.pool).await.map_err(map_sqlx_error)?;
        if done.rows_affected() == 0 {
            return Err(lease_lost(id, worker));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl JobQueue for PgStore {
    async fn enqueue(&self, job: &NewJob) -> Result<Enqueued, StoreError> {
        job.validate()?;
        // The active job may finish between the insert and the lookup: retry.
        for _ in 0..3 {
            let inserted: Option<i64> = sqlx::query_scalar(
                "INSERT INTO jobs (queue, dedupe_key, payload, priority, max_attempts)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (queue, dedupe_key)
                    WHERE dedupe_key IS NOT NULL AND status IN ('queued', 'running')
                 DO NOTHING
                 RETURNING id",
            )
            .bind(&job.queue)
            .bind(&job.dedupe_key)
            .bind(Json(&job.payload))
            .bind(job.priority)
            .bind(job.max_attempts as i32)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
            if let Some(id) = inserted {
                return Ok(Enqueued {
                    id: JobId(id),
                    created: true,
                });
            }
            let existing: Option<i64> = sqlx::query_scalar(
                "SELECT id FROM jobs
                 WHERE queue = $1 AND dedupe_key = $2 AND status IN ('queued', 'running')",
            )
            .bind(&job.queue)
            .bind(&job.dedupe_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
            if let Some(id) = existing {
                return Ok(Enqueued {
                    id: JobId(id),
                    created: false,
                });
            }
        }
        Err(StoreError::Unavailable(
            "enqueue kept racing with the active job of the same dedupe_key".into(),
        ))
    }

    async fn claim(
        &self,
        queue: &str,
        worker: &str,
        lease: Duration,
    ) -> Result<Option<Job>, StoreError> {
        // Expired leases that used every attempt go dead first.
        sqlx::query(
            "UPDATE jobs SET status = 'dead',
                    last_error = 'lease expired after ' || attempts || ' attempts',
                    locked_by = NULL, locked_until = NULL, updated_at = now()
             WHERE id IN (
                SELECT id FROM jobs
                WHERE queue = $1 AND status = 'running' AND locked_until <= now()
                  AND attempts >= max_attempts
                FOR UPDATE SKIP LOCKED)",
        )
        .bind(queue)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;

        let row = sqlx::query(&format!(
            "UPDATE jobs SET status = 'running', attempts = attempts + 1, locked_by = $2,
                    locked_until = now() + $3 * interval '1 microsecond', updated_at = now()
             WHERE id = (
                SELECT id FROM jobs
                WHERE queue = $1
                  AND ((status = 'queued' AND run_after <= now())
                    OR (status = 'running' AND locked_until <= now() AND attempts < max_attempts))
                ORDER BY priority DESC, run_after, id
                FOR UPDATE SKIP LOCKED
                LIMIT 1)
             RETURNING {JOB_COLUMNS}"
        ))
        .bind(queue)
        .bind(worker)
        .bind(micros(lease))
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        row.as_ref().map(job_row).transpose()
    }

    async fn heartbeat(&self, id: JobId, worker: &str, lease: Duration) -> Result<(), StoreError> {
        self.held_update(
            id,
            worker,
            "UPDATE jobs SET locked_until = now() + $3 * interval '1 microsecond', updated_at = now()
             WHERE id = $1 AND status = 'running' AND locked_by = $2",
            None,
            Some(micros(lease)),
        )
        .await
    }

    async fn complete(&self, id: JobId, worker: &str) -> Result<(), StoreError> {
        self.held_update(
            id,
            worker,
            "UPDATE jobs SET status = 'succeeded', locked_by = NULL, locked_until = NULL,
                    updated_at = now()
             WHERE id = $1 AND status = 'running' AND locked_by = $2",
            None,
            None,
        )
        .await
    }

    async fn fail(
        &self,
        id: JobId,
        worker: &str,
        error: &str,
        retry_after: Duration,
    ) -> Result<(), StoreError> {
        self.held_update(
            id,
            worker,
            "UPDATE jobs SET last_error = $3, locked_by = NULL, locked_until = NULL,
                    updated_at = now(),
                    status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
                    run_after = CASE WHEN attempts >= max_attempts THEN run_after
                                     ELSE now() + $4 * interval '1 microsecond' END
             WHERE id = $1 AND status = 'running' AND locked_by = $2",
            Some(error),
            Some(micros(retry_after)),
        )
        .await
    }

    async fn get(&self, id: JobId) -> Result<Option<Job>, StoreError> {
        let row = sqlx::query(&format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
        row.as_ref().map(job_row).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_mapping_follows_the_contract() {
        assert!(matches!(
            map_sqlx_error(sqlx::Error::PoolTimedOut),
            StoreError::Unavailable(_)
        ));
        assert!(matches!(
            map_sqlx_error(sqlx::Error::PoolClosed),
            StoreError::Unavailable(_)
        ));
        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        assert!(matches!(
            map_sqlx_error(sqlx::Error::Io(io)),
            StoreError::Unavailable(_)
        ));
        assert!(matches!(
            map_sqlx_error(sqlx::Error::RowNotFound),
            StoreError::Invalid(_)
        ));
        assert!(matches!(
            map_sqlx_error(sqlx::Error::ColumnNotFound("x".into())),
            StoreError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn bad_url_is_unconfigured() {
        let err = PgStore::connect("mysql://nope").await.unwrap_err();
        assert!(matches!(err, StoreError::Unconfigured(_)), "{err:?}");
    }

    #[tokio::test]
    async fn unreachable_database_is_unavailable() {
        // Port 9 on loopback: nothing listens. Pool acquire fails fast.
        let err = PgStore::connect("postgres://u:p@127.0.0.1:9/guard")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable(_)), "{err:?}");
    }

    #[test]
    fn migrations_are_embedded_in_order() {
        let versions: Vec<i64> = MIGRATOR.iter().map(|m| m.version).collect();
        assert_eq!(
            versions,
            [
                20261101000001,
                20261101000002,
                20261101000003,
                20261101000004
            ]
        );
    }
}
