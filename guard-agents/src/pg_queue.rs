//! Postgres `TaskQueue` (`task_queue` table of
//! `guard-store/migrations/20261101000003_agent_lane.sql`) and the
//! `spend_records` daily ledger. Uses guard-store's pinned sqlx and its pool.
//!
//! Claim protocol (one transaction):
//! 1. `pg_advisory_xact_lock` serializes claimers, so two claimers can never
//!    both start tasks whose predicted files overlap (each would otherwise
//!    miss the other's uncommitted `running` row).
//! 2. Running tasks whose lease expired at `max_attempts` become `failed`.
//! 3. The best claimable row is picked with `FOR UPDATE SKIP LOCKED` (rows a
//!    holder is heartbeating/completing are skipped, never waited on) and
//!    updated to `running` with `locked_until = clock_timestamp() + lease`.
//!
//! The lease is the visibility timeout: a crashed worker's task becomes
//! claimable again once it passes. Lease times use `clock_timestamp()`, not
//! the transaction start, because a claimer may wait on the advisory lock.

use crate::scheduler::SpendLedger;
use guard_core::ports::{
    ClaimedTask, NewTask, QueuedTask, Role, StoreError, TaskEnqueued, TaskId, TaskQueue, TaskState,
};
use guard_store::map_sqlx_error;
use guard_store::sqlx::postgres::PgRow;
use guard_store::sqlx::types::Json;
use guard_store::sqlx::{self, PgPool, Row};
use std::time::Duration;

/// Advisory lock key of the claim critical section ("guardTQ").
pub const CLAIM_LOCK_KEY: i64 = 0x0067_7561_7264_5451;

const COLUMNS: &str = "id, repo, ticket, role, priority, predicted_files, payload, state, \
                       attempts, max_attempts, locked_by, last_error";

#[derive(Clone)]
pub struct PgTaskQueue {
    pool: PgPool,
}

impl PgTaskQueue {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// One task by id (any state).
    pub async fn get(&self, id: TaskId) -> Result<Option<QueuedTask>, StoreError> {
        sqlx::query(&format!("SELECT {COLUMNS} FROM task_queue WHERE id = $1"))
            .bind(id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?
            .map(|r| row_to_task(&r))
            .transpose()
    }

    async fn active_id(&self, task: &NewTask) -> Result<Option<TaskId>, StoreError> {
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM task_queue
             WHERE repo = $1 AND ticket = $2 AND role = $3 AND state IN ('queued', 'running')",
        )
        .bind(&task.repo)
        .bind(&task.ticket)
        .bind(task.role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(id.map(TaskId))
    }
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn to_u32(v: i32, column: &str) -> Result<u32, StoreError> {
    u32::try_from(v).map_err(|_| StoreError::Invalid(format!("negative {column}: {v}")))
}

fn row_to_task(r: &PgRow) -> Result<QueuedTask, StoreError> {
    let role: String = r.try_get("role").map_err(map_sqlx_error)?;
    let state: String = r.try_get("state").map_err(map_sqlx_error)?;
    let state = [
        TaskState::Queued,
        TaskState::Running,
        TaskState::Succeeded,
        TaskState::Failed,
    ]
    .into_iter()
    .find(|s| s.as_str() == state)
    .ok_or_else(|| StoreError::Invalid(format!("unknown task state {state:?}")))?;
    let Json(payload): Json<serde_json::Value> = r.try_get("payload").map_err(map_sqlx_error)?;
    Ok(QueuedTask {
        id: TaskId(r.try_get("id").map_err(map_sqlx_error)?),
        repo: r.try_get("repo").map_err(map_sqlx_error)?,
        ticket: r.try_get("ticket").map_err(map_sqlx_error)?,
        role: Role::from_name(&role)
            .ok_or_else(|| StoreError::Invalid(format!("unknown role {role:?}")))?,
        priority: r.try_get("priority").map_err(map_sqlx_error)?,
        predicted_files: r.try_get("predicted_files").map_err(map_sqlx_error)?,
        payload,
        state,
        attempts: to_u32(r.try_get("attempts").map_err(map_sqlx_error)?, "attempts")?,
        max_attempts: to_u32(
            r.try_get("max_attempts").map_err(map_sqlx_error)?,
            "max_attempts",
        )?,
        locked_by: r.try_get("locked_by").map_err(map_sqlx_error)?,
        last_error: r.try_get("last_error").map_err(map_sqlx_error)?,
    })
}

fn not_holder(id: TaskId, worker: &str) -> StoreError {
    StoreError::Invalid(format!("task {} is not leased by {worker}", id.0))
}

#[async_trait::async_trait]
impl TaskQueue for PgTaskQueue {
    async fn enqueue(&self, task: &NewTask) -> Result<TaskEnqueued, StoreError> {
        task.validate()?;
        let max_attempts = i32::try_from(task.max_attempts)
            .map_err(|_| StoreError::Invalid("max_attempts too large".into()))?;
        let mut files = task.predicted_files.clone();
        files.sort();
        files.dedup();
        // The active row may finish between a lost insert and the lookup;
        // a few rounds settle it.
        for _ in 0..5 {
            let inserted: Option<i64> = sqlx::query_scalar(
                "INSERT INTO task_queue (repo, ticket, role, priority, predicted_files, payload, max_attempts)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (repo, ticket, role) WHERE state IN ('queued', 'running') DO NOTHING
                 RETURNING id",
            )
            .bind(&task.repo)
            .bind(&task.ticket)
            .bind(task.role.as_str())
            .bind(task.priority)
            .bind(&files)
            .bind(Json(&task.payload))
            .bind(max_attempts)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx_error)?;
            if let Some(id) = inserted {
                return Ok(TaskEnqueued {
                    id: TaskId(id),
                    created: true,
                });
            }
            if let Some(id) = self.active_id(task).await? {
                return Ok(TaskEnqueued { id, created: false });
            }
        }
        Err(StoreError::Unavailable(
            "enqueue kept racing with the active task".into(),
        ))
    }

    async fn claim(
        &self,
        worker: &str,
        lease: Duration,
    ) -> Result<Option<ClaimedTask>, StoreError> {
        if worker.is_empty() {
            return Err(StoreError::Invalid("worker id is empty".into()));
        }
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(CLAIM_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        sqlx::query(
            "UPDATE task_queue
             SET state = 'failed', locked_by = NULL, locked_until = NULL, updated_at = now(),
                 last_error = 'lease expired after ' || attempts || ' attempts'
             WHERE state = 'running' AND locked_until <= clock_timestamp()
               AND attempts >= max_attempts",
        )
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        let row = sqlx::query(&format!(
            "UPDATE task_queue t
             SET state = 'running', attempts = t.attempts + 1, locked_by = $1,
                 locked_until = clock_timestamp() + make_interval(secs => $2), updated_at = now()
             WHERE t.id = (
                 SELECT c.id FROM task_queue c
                 WHERE ((c.state = 'queued' AND c.run_after <= clock_timestamp())
                        OR (c.state = 'running' AND c.locked_until <= clock_timestamp()
                            AND c.attempts < c.max_attempts))
                   AND NOT EXISTS (
                       SELECT 1 FROM task_queue r
                       WHERE r.state = 'running' AND r.locked_until > clock_timestamp()
                         AND r.id <> c.id AND r.predicted_files && c.predicted_files)
                 ORDER BY c.priority DESC, c.run_after, c.id
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED)
             RETURNING {COLUMNS}"
        ))
        .bind(worker)
        .bind(secs(lease))
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_error)?;
        tx.commit().await.map_err(map_sqlx_error)?;
        row.map(|r| {
            Ok(ClaimedTask {
                task: row_to_task(&r)?,
                lease,
            })
        })
        .transpose()
    }

    async fn conflicts(&self, files: &[String]) -> Result<Vec<TaskId>, StoreError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM task_queue
             WHERE state = 'running' AND locked_until > clock_timestamp() AND predicted_files && $1
             ORDER BY id",
        )
        .bind(files)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(ids.into_iter().map(TaskId).collect())
    }

    async fn heartbeat(&self, id: TaskId, worker: &str, lease: Duration) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE task_queue
             SET locked_until = clock_timestamp() + make_interval(secs => $3), updated_at = now()
             WHERE id = $1 AND state = 'running' AND locked_by = $2",
        )
        .bind(id.0)
        .bind(worker)
        .bind(secs(lease))
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if done.rows_affected() == 1 {
            Ok(())
        } else {
            Err(not_holder(id, worker))
        }
    }

    async fn complete(&self, id: TaskId, worker: &str) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE task_queue
             SET state = 'succeeded', locked_by = NULL, locked_until = NULL, updated_at = now()
             WHERE id = $1 AND state = 'running' AND locked_by = $2",
        )
        .bind(id.0)
        .bind(worker)
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if done.rows_affected() == 1 {
            Ok(())
        } else {
            Err(not_holder(id, worker))
        }
    }

    async fn fail(
        &self,
        id: TaskId,
        worker: &str,
        error: &str,
        retry_after: Option<Duration>,
    ) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE task_queue
             SET last_error = $3, locked_by = NULL, locked_until = NULL, updated_at = now(),
                 state = CASE WHEN $4::float8 IS NOT NULL AND attempts < max_attempts
                              THEN 'queued' ELSE 'failed' END,
                 run_after = CASE WHEN $4::float8 IS NOT NULL AND attempts < max_attempts
                                  THEN clock_timestamp() + make_interval(secs => $4::float8)
                                  ELSE run_after END
             WHERE id = $1 AND state = 'running' AND locked_by = $2",
        )
        .bind(id.0)
        .bind(worker)
        .bind(error)
        .bind(retry_after.map(secs))
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if done.rows_affected() == 1 {
            Ok(())
        } else {
            Err(not_holder(id, worker))
        }
    }

    async fn reschedule(
        &self,
        id: TaskId,
        worker: &str,
        reason: &str,
        after: Duration,
    ) -> Result<(), StoreError> {
        let done = sqlx::query(
            "UPDATE task_queue
             SET last_error = $3, locked_by = NULL, locked_until = NULL, updated_at = now(),
                 state = 'queued', attempts = greatest(attempts - 1, 0),
                 run_after = clock_timestamp() + make_interval(secs => $4::float8)
             WHERE id = $1 AND state = 'running' AND locked_by = $2",
        )
        .bind(id.0)
        .bind(worker)
        .bind(reason)
        .bind(secs(after))
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        if done.rows_affected() == 1 {
            Ok(())
        } else {
            Err(not_holder(id, worker))
        }
    }
}

/// Today's (UTC) gateway spend from `spend_records` (`[scheduler].daily_usd_cap`).
#[derive(Clone)]
pub struct PgSpendLedger {
    pool: PgPool,
}

impl PgSpendLedger {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl SpendLedger for PgSpendLedger {
    async fn spent_today_usd(&self) -> Result<f64, StoreError> {
        sqlx::query_scalar(
            "SELECT coalesce(sum(spend_usd), 0)::float8 FROM spend_records
             WHERE start_time >= date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)
    }
}
