//! `TaskQueue`: the agent work queue (architecture section 16).
//!
//! The unit of work is the (ticket, role) pair. Postgres `task_queue` table
//! (`guard-store/migrations/20261101000003_agent_lane.sql`), claimed with
//! `FOR UPDATE SKIP LOCKED`. Semantics every implementation shares:
//!
//! - **Dedupe**: one active (`queued`/`running`) task per (repo, ticket, role);
//!   enqueueing it again returns the active task with `created = false`.
//! - **Claim**: the best claimable task — `queued` with `run_after <= now`, or
//!   `running` with an expired lease — ordered by higher `priority`, older
//!   `run_after`, lower id. A task whose `predicted_files` intersect those of
//!   a `running` task with a live lease is skipped (it waits). Rows locked by
//!   another claimer are skipped, never waited on. Each claim increments `attempts`.
//! - `complete`, `fail`, `heartbeat` need the current lease holder, else
//!   `StoreError::Invalid`.
//! - `fail` with `retry_after = Some(d)` re-queues at `now + d` while
//!   `attempts < max_attempts`; otherwise (or with `None`) the task is `failed`.
//!   An expired lease at `max_attempts` also ends `failed`.
//! - `reschedule` re-queues at `now + d` and gives the claim's attempt back:
//!   a wait that is not an infrastructure failure (the daily USD cap) never
//!   uses up `max_attempts`.
//!
//! Queue attempts are infrastructure retries (crashed worker, lost host). The
//! Coder's 3-attempt budget of a ticket is [`super::agent::AttemptPolicy`],
//! counted by the harness inside one claimed task.

use super::agent::Role;
use super::decision_log::StoreError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TaskId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Succeeded => "succeeded",
            TaskState::Failed => "failed",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, TaskState::Queued | TaskState::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTask {
    pub repo: String,
    pub ticket: String,
    pub role: Role,
    /// Higher runs first.
    pub priority: i32,
    /// Repo-relative files the impact analysis predicts the task touches.
    pub predicted_files: Vec<String>,
    /// Scheduler data (agent name, model, budget); opaque to the queue.
    pub payload: serde_json::Value,
    /// Infrastructure retries, at least 1.
    pub max_attempts: u32,
}

impl NewTask {
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.repo.is_empty() || self.ticket.is_empty() {
            return Err(StoreError::Invalid(
                "task repo and ticket are required".into(),
            ));
        }
        if self.max_attempts == 0 {
            return Err(StoreError::Invalid(
                "max_attempts must be at least 1".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedTask {
    pub id: TaskId,
    pub repo: String,
    pub ticket: String,
    pub role: Role,
    pub priority: i32,
    pub predicted_files: Vec<String>,
    pub payload: serde_json::Value,
    pub state: TaskState,
    pub attempts: u32,
    pub max_attempts: u32,
    pub locked_by: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskEnqueued {
    pub id: TaskId,
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedTask {
    pub task: QueuedTask,
    pub lease: Duration,
}

#[async_trait::async_trait]
pub trait TaskQueue: Send + Sync {
    async fn enqueue(&self, task: &NewTask) -> Result<TaskEnqueued, StoreError>;

    /// `None` when nothing is claimable (empty, delayed, or every candidate conflicts).
    async fn claim(&self, worker: &str, lease: Duration)
    -> Result<Option<ClaimedTask>, StoreError>;

    /// Running tasks (live lease) whose predicted files intersect `files`, by id.
    async fn conflicts(&self, files: &[String]) -> Result<Vec<TaskId>, StoreError>;

    /// Extends the lease to `now + lease`.
    async fn heartbeat(&self, id: TaskId, worker: &str, lease: Duration) -> Result<(), StoreError>;

    async fn complete(&self, id: TaskId, worker: &str) -> Result<(), StoreError>;

    async fn fail(
        &self,
        id: TaskId,
        worker: &str,
        error: &str,
        retry_after: Option<Duration>,
    ) -> Result<(), StoreError>;

    /// Re-queues the claimed task at `now + after` without consuming the
    /// claim's attempt (`attempts` is decremented). Needs the lease holder.
    async fn reschedule(
        &self,
        id: TaskId,
        worker: &str,
        reason: &str,
        after: Duration,
    ) -> Result<(), StoreError>;
}
