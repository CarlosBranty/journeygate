//! `agent_runs` rows (one per sandboxed attempt) from a [`StageReport`]'s
//! [`AttemptRecord`]s, and the time helpers the orchestrator side needs.
//!
//! [`PgRunLog`] upserts by `(run_id, role, attempt, agent)`, the table's
//! unique key (migration `20261101000004`), so replaying a stage report is
//! idempotent and best-of-N candidates (same run, role and attempt number,
//! different `[agents.<name>]`) each keep their own row.

use crate::pipeline::{AttemptRecord, StageReport};
use guard_core::ports::{StoreError, TaskId, TaskStatus};
use guard_store::map_sqlx_error;
use guard_store::sqlx::types::Json;
use guard_store::sqlx::{self, PgPool};
use serde::Serialize;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The attempt-independent columns of the rows of one stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunContext {
    pub run_id: String,
    pub task_id: Option<TaskId>,
    pub repo: String,
    pub ticket: String,
    pub base_commit: String,
}

/// `agent_runs.status`.
pub fn status_column(status: Option<TaskStatus>) -> &'static str {
    match status {
        Some(TaskStatus::Completed) => "completed",
        Some(TaskStatus::Failed) => "failed",
        Some(TaskStatus::BudgetExceeded) => "budget_exceeded",
        Some(TaskStatus::TimedOut) => "timed_out",
        Some(TaskStatus::Refused) => "refused",
        None => "error",
    }
}

#[async_trait::async_trait]
pub trait RunLog: Send + Sync {
    async fn record(&self, ctx: &RunContext, attempt: &AttemptRecord) -> Result<(), StoreError>;
}

/// Records every attempt of `report`; returns how many rows were written.
pub async fn record_stage(
    log: &dyn RunLog,
    ctx: &RunContext,
    report: &StageReport,
) -> Result<usize, StoreError> {
    for a in &report.attempts {
        log.record(ctx, a).await?;
    }
    Ok(report.attempts.len())
}

pub struct PgRunLog {
    pool: PgPool,
}

impl PgRunLog {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl RunLog for PgRunLog {
    async fn record(&self, ctx: &RunContext, a: &AttemptRecord) -> Result<(), StoreError> {
        let result = serde_json::json!({
            "status": a.status,
            "outcome": a.outcome.map(|o| o.as_str()),
            "detail": a.detail,
            "changed_lines": a.changed_lines,
        });
        sqlx::query(
            "INSERT INTO agent_runs (run_id, task_id, repo, ticket, role, attempt, agent, runtime,
                 model, arm, sandbox_id, key_alias, base_commit, status, outcome,
                 sealed_spec_sha256, diff_paths, usage, cost_usd, steps, memory_reads,
                 facts_brief_ids, result, finished_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                 $18, $19::float8::numeric(12, 6), $20, $21, $22, $23, now())
             ON CONFLICT (run_id, role, attempt, agent) DO UPDATE SET
                 task_id = EXCLUDED.task_id, runtime = EXCLUDED.runtime,
                 model = EXCLUDED.model, arm = EXCLUDED.arm, sandbox_id = EXCLUDED.sandbox_id,
                 key_alias = EXCLUDED.key_alias, status = EXCLUDED.status,
                 outcome = EXCLUDED.outcome, sealed_spec_sha256 = EXCLUDED.sealed_spec_sha256,
                 diff_paths = EXCLUDED.diff_paths, usage = EXCLUDED.usage,
                 cost_usd = EXCLUDED.cost_usd, steps = EXCLUDED.steps,
                 memory_reads = EXCLUDED.memory_reads, facts_brief_ids = EXCLUDED.facts_brief_ids,
                 result = EXCLUDED.result, finished_at = EXCLUDED.finished_at",
        )
        .bind(&ctx.run_id)
        .bind(ctx.task_id.map(|t| t.0))
        .bind(&ctx.repo)
        .bind(&ctx.ticket)
        .bind(a.role.as_str())
        .bind(a.attempt as i32)
        .bind(&a.agent)
        .bind(&a.runtime)
        .bind(&a.model)
        .bind(&a.arm)
        .bind(&a.sandbox_id)
        .bind(&a.key_alias)
        .bind(&ctx.base_commit)
        .bind(status_column(a.status))
        .bind(a.outcome.map(|o| o.as_str()))
        .bind(&a.sealed_spec_sha256)
        .bind(&a.diff_paths)
        .bind(a.usage.map(Json))
        .bind(a.cost_usd)
        .bind(a.steps.map(|s| s as i32))
        .bind(a.memory_reads as i32)
        .bind(&a.facts_brief_ids)
        .bind(Json(result))
        .execute(&self.pool)
        .await
        .map_err(map_sqlx_error)?;
        Ok(())
    }
}

/// In-memory run log (tests, `guard agents` without Postgres).
#[derive(Default)]
pub struct MemoryRunLog {
    rows: Mutex<Vec<(RunContext, AttemptRecord)>>,
}

impl MemoryRunLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn rows(&self) -> Vec<(RunContext, AttemptRecord)> {
        self.rows.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl RunLog for MemoryRunLog {
    async fn record(&self, ctx: &RunContext, a: &AttemptRecord) -> Result<(), StoreError> {
        let mut rows = self.rows.lock().unwrap_or_else(|p| p.into_inner());
        rows.retain(|(c, r)| {
            !(c.run_id == ctx.run_id
                && r.role == a.role
                && r.attempt == a.attempt
                && r.agent == a.agent)
        });
        rows.push((ctx.clone(), a.clone()));
        Ok(())
    }
}

// ---------------------------------------------------------------- time

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `(year, month, day)` of a day count since 1970-01-01 (H. Hinnant's
/// `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn parts(unix: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let (y, m, d) = civil_from_days(days);
    (y, m, d, secs / 3600, (secs % 3600) / 60, secs % 60)
}

/// `2026-11-01T10:00:00Z`.
pub fn rfc3339(unix: u64) -> String {
    let (y, m, d, hh, mm, ss) = parts(unix);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// `2026-11-01 10:00:00` (the gateway's spend-log format).
pub fn gateway_time(unix: u64) -> String {
    let (y, m, d, hh, mm, ss) = parts(unix);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}")
}

/// The UTC day around `unix` in gateway format: `[00:00, next 00:00)`.
pub fn utc_day_window(unix: u64) -> (String, String) {
    let start = unix - unix % 86_400;
    (gateway_time(start), gateway_time(start + 86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_format_in_utc() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        // 2026-11-01T10:20:30Z
        assert_eq!(rfc3339(1_793_528_430), "2026-11-01T10:20:30Z");
        assert_eq!(gateway_time(1_793_528_430), "2026-11-01 10:20:30");
        assert_eq!(
            utc_day_window(1_793_528_430),
            ("2026-11-01 00:00:00".into(), "2026-11-02 00:00:00".into())
        );
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn statuses_map_to_the_column_values() {
        assert_eq!(status_column(Some(TaskStatus::Completed)), "completed");
        assert_eq!(
            status_column(Some(TaskStatus::BudgetExceeded)),
            "budget_exceeded"
        );
        assert_eq!(status_column(None), "error");
    }
}
