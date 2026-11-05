//! Durable job queue for the Phase 4 scheduler (architecture section 16: the
//! unit of work is the `(ticket, role)` pair; the queue lives in Postgres with
//! `FOR UPDATE SKIP LOCKED`, in the same database as the decision log).
//!
//! Semantics shared by [`crate::PgStore`] and [`crate::MemoryStore`]:
//!
//! - `claim` takes the best claimable job of a queue: `queued` with
//!   `run_after <= now`, or `running` with an expired lease. Order: higher
//!   `priority` first, then oldest `run_after`, then lowest id. Each claim
//!   increments `attempts`.
//! - An expired lease whose job already used `max_attempts` goes `dead`
//!   (`last_error = "lease expired after <n> attempts"`) instead of being claimed.
//! - `complete`, `fail` and `heartbeat` need the current lease holder
//!   (`status = running`, `locked_by = worker`); otherwise `StoreError::Invalid`.
//! - `fail` re-queues with `run_after = now + retry_after` while
//!   `attempts < max_attempts`, else marks the job `dead`.
//! - A `dedupe_key` is unique among active (`queued`/`running`) jobs of a
//!   queue: enqueueing it again returns the active job with `created = false`.

use guard_core::ports::StoreError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    /// Failed `max_attempts` times (or its last lease expired at the limit).
    Dead,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Dead => "dead",
        }
    }

    pub fn from_name(name: &str) -> Option<JobStatus> {
        [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Succeeded,
            JobStatus::Dead,
        ]
        .into_iter()
        .find(|s| s.as_str() == name)
    }

    pub fn is_active(self) -> bool {
        matches!(self, JobStatus::Queued | JobStatus::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewJob {
    /// Queue name (e.g. `agent`). Non-empty.
    pub queue: String,
    /// E.g. `SHOP-1:coder`. `None` = never deduplicated.
    pub dedupe_key: Option<String>,
    pub payload: serde_json::Value,
    /// Higher runs first.
    pub priority: i32,
    /// At least 1.
    pub max_attempts: u32,
}

impl NewJob {
    pub(crate) fn validate(&self) -> Result<(), StoreError> {
        if self.queue.is_empty() {
            return Err(StoreError::Invalid("job queue name is empty".into()));
        }
        if self.max_attempts == 0 || self.max_attempts > i32::MAX as u32 {
            return Err(StoreError::Invalid(format!(
                "max_attempts must be in 1..={}, got {}",
                i32::MAX,
                self.max_attempts
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub queue: String,
    pub dedupe_key: Option<String>,
    pub payload: serde_json::Value,
    pub priority: i32,
    pub status: JobStatus,
    pub attempts: u32,
    pub max_attempts: u32,
    /// The worker holding the lease (only while `running`).
    pub locked_by: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enqueued {
    pub id: JobId,
    /// `false` when an active job with the same `dedupe_key` already existed.
    pub created: bool,
}

#[async_trait::async_trait]
pub trait JobQueue: Send + Sync {
    async fn enqueue(&self, job: &NewJob) -> Result<Enqueued, StoreError>;

    /// Leases the next claimable job of `queue` to `worker` for `lease`.
    async fn claim(
        &self,
        queue: &str,
        worker: &str,
        lease: Duration,
    ) -> Result<Option<Job>, StoreError>;

    /// Extends the lease of a job `worker` holds.
    async fn heartbeat(&self, id: JobId, worker: &str, lease: Duration) -> Result<(), StoreError>;

    async fn complete(&self, id: JobId, worker: &str) -> Result<(), StoreError>;

    /// Records a failed attempt; retries after `retry_after` or goes `dead`.
    async fn fail(
        &self,
        id: JobId,
        worker: &str,
        error: &str,
        retry_after: Duration,
    ) -> Result<(), StoreError>;

    async fn get(&self, id: JobId) -> Result<Option<Job>, StoreError>;
}

pub(crate) fn lease_lost(id: JobId, worker: &str) -> StoreError {
    StoreError::Invalid(format!(
        "job {} is not running under a lease held by {worker}",
        id.0
    ))
}

/// Must match the SQL in `PgStore::claim`.
pub(crate) fn lease_expired_error(attempts: u32) -> String {
    format!("lease expired after {attempts} attempts")
}

/// Longest lease or retry delay: 100 years (keeps timestamp math in range).
pub const MAX_DELAY: Duration = Duration::from_secs(100 * 365 * 24 * 3600);

pub(crate) fn clamp_delay(d: Duration) -> Duration {
    d.min(MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_round_trip() {
        for s in [
            JobStatus::Queued,
            JobStatus::Running,
            JobStatus::Succeeded,
            JobStatus::Dead,
        ] {
            assert_eq!(JobStatus::from_name(s.as_str()), Some(s));
        }
        assert_eq!(JobStatus::from_name("failed"), None);
    }

    #[test]
    fn delays_are_clamped() {
        assert_eq!(clamp_delay(Duration::MAX), MAX_DELAY);
        assert_eq!(clamp_delay(Duration::from_secs(1)), Duration::from_secs(1));
    }

    #[test]
    fn lease_expired_message() {
        assert_eq!(lease_expired_error(3), "lease expired after 3 attempts");
    }
}
