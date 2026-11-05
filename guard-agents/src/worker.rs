//! The queue worker: claims one (ticket, role) task at a time within the
//! scheduler slots and runs its pipeline stage.
//!
//! - `(ticket, qa)`: QA stage. Sealed → enqueues `(ticket, coder)` with the
//!   sealed spec in the payload, then completes.
//! - `(ticket, coder)`: Coder stage (attempt loop, best-of-N) → PR or human lane.
//! - Business outcomes (sealed, PR, human lane) complete the task; the
//!   report is returned so the orchestrator records runs and memory proposals.
//! - Infrastructure errors fail the task with a retry ([`PipelineError::retry_after`]);
//!   misconfiguration and malformed payloads fail it for good.
//! - While a stage runs, the lease is extended every `lease / 3`.

use crate::pipeline::seams::{PublishedPr, TicketInput};
use crate::pipeline::{HumanLaneReason, PipelineError, StageOutcome, StageReport, TicketPipeline};
use crate::scheduler::{SchedulerError, Slots, TaskSlot};
use guard_core::ports::{
    ClaimedTask, NewTask, Role, SealedSpec, StoreError, TaskEnqueued, TaskId, TaskQueue,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Infrastructure retries of one (ticket, role) task (crashed worker, lost host).
pub const QUEUE_MAX_ATTEMPTS: u32 = 3;

/// `task_queue.payload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPayload {
    pub input: TicketInput,
    /// Required for the coder role, absent for qa.
    #[serde(default)]
    pub sealed: Option<SealedSpec>,
}

/// The `NewTask` of one (ticket, role); the coder role needs the sealed spec.
pub fn ticket_task(
    input: &TicketInput,
    role: Role,
    sealed: Option<SealedSpec>,
) -> Result<NewTask, StoreError> {
    match (role, &sealed) {
        (Role::Coder, None) => {
            return Err(StoreError::Invalid(
                "a coder task needs the sealed spec".into(),
            ));
        }
        (Role::Qa, Some(_)) => {
            return Err(StoreError::Invalid(
                "a qa task carries no sealed spec".into(),
            ));
        }
        _ => {}
    }
    let payload = serde_json::to_value(TaskPayload {
        input: input.clone(),
        sealed,
    })
    .map_err(|e| StoreError::Invalid(e.to_string()))?;
    Ok(NewTask {
        repo: input.repo.clone(),
        ticket: input.ticket.clone(),
        role,
        priority: 0,
        predicted_files: input.predicted_files.clone(),
        payload,
        max_attempts: QUEUE_MAX_ATTEMPTS,
    })
}

/// `guard agents run <TICKET>`: enqueues `(ticket, qa)`.
pub async fn enqueue_ticket(
    queue: &dyn TaskQueue,
    input: &TicketInput,
    priority: i32,
) -> Result<TaskEnqueued, StoreError> {
    let mut task = ticket_task(input, Role::Qa, None)?;
    task.priority = priority;
    queue.enqueue(&task).await
}

#[derive(Debug, Clone, PartialEq)]
pub enum Idle {
    HostSlotsFull,
    DailyCapReached,
    /// The spend ledger did not answer: fail closed, start nothing.
    LedgerUnavailable(String),
    /// Empty, delayed, or every candidate conflicts with a running task.
    NothingClaimable,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Disposition {
    Sealed { coder_task: TaskId },
    Passed { pr: PublishedPr },
    HumanLane(HumanLaneReason),
    Retrying { error: String, after: Duration },
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ran {
    pub task: TaskId,
    pub ticket: String,
    pub role: Role,
    pub disposition: Disposition,
    pub report: Option<StageReport>,
    /// The task's ticket input (`None` when the payload was malformed).
    pub input: Option<TicketInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tick {
    Idle(Idle),
    Ran(Box<Ran>),
}

pub struct Worker {
    id: String,
    queue: Arc<dyn TaskQueue>,
    slots: Arc<Slots>,
    pipeline: Arc<TicketPipeline>,
    lease: Duration,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Worker {
    /// `id` is unique per process and host (`<host>:<n>`); `lease` is `[scheduler].lease_secs`.
    pub fn new(
        id: impl Into<String>,
        queue: Arc<dyn TaskQueue>,
        slots: Arc<Slots>,
        pipeline: Arc<TicketPipeline>,
        lease: Duration,
    ) -> Self {
        Self {
            id: id.into(),
            queue,
            slots,
            pipeline,
            lease,
        }
    }

    /// One scheduling step. Only queue errors are `Err`.
    pub async fn tick(&self) -> Result<Tick, StoreError> {
        let Some(slot) = self.slots.try_task_slot() else {
            return Ok(Tick::Idle(Idle::HostSlotsFull));
        };
        match self.slots.check_budget().await {
            Ok(()) => {}
            Err(SchedulerError::DailyCapReached { .. }) => {
                return Ok(Tick::Idle(Idle::DailyCapReached));
            }
            Err(SchedulerError::Ledger(e)) => {
                return Ok(Tick::Idle(Idle::LedgerUnavailable(e.to_string())));
            }
        }
        let Some(claimed) = self.queue.claim(&self.id, self.lease).await? else {
            return Ok(Tick::Idle(Idle::NothingClaimable));
        };
        let ran = self.run_claimed(&claimed, &slot).await?;
        Ok(Tick::Ran(Box::new(ran)))
    }

    async fn run_claimed(&self, c: &ClaimedTask, slot: &TaskSlot) -> Result<Ran, StoreError> {
        let t = &c.task;
        let mut ran = Ran {
            task: t.id,
            ticket: t.ticket.clone(),
            role: t.role,
            disposition: Disposition::Failed {
                error: String::new(),
            },
            report: None,
            input: None,
        };
        let payload = match self.payload_of(c) {
            Ok(p) => p,
            Err(error) => {
                self.queue.fail(t.id, &self.id, &error, None).await?;
                ran.disposition = Disposition::Failed { error };
                return Ok(ran);
            }
        };
        let mut payload = payload;
        payload.input.claim = t.attempts;
        ran.input = Some(payload.input.clone());
        let result = self.with_heartbeat(t.id, async {
            match (t.role, &payload.sealed) {
                (Role::Qa, _) => self.pipeline.run_qa(&payload.input, slot).await,
                (Role::Coder, Some(sealed)) => {
                    self.pipeline.run_coder(&payload.input, sealed, slot).await
                }
                (Role::Coder, None) => unreachable!("checked by payload_of"),
            }
        });
        match result.await {
            Ok(report) => {
                ran.disposition = match &report.outcome {
                    StageOutcome::Sealed(sealed) => {
                        let next = ticket_task(&payload.input, Role::Coder, Some(sealed.clone()))?;
                        let next = NewTask {
                            priority: t.priority,
                            ..next
                        };
                        let coder_task = self.queue.enqueue(&next).await?.id;
                        Disposition::Sealed { coder_task }
                    }
                    StageOutcome::Passed { pr, .. } => Disposition::Passed { pr: pr.clone() },
                    StageOutcome::HumanLane(r) => Disposition::HumanLane(r.clone()),
                };
                self.queue.complete(t.id, &self.id).await?;
                ran.report = Some(report);
            }
            // A budget wait is not an infrastructure failure: it must not
            // use up the queue's attempts.
            Err(e @ PipelineError::Scheduler(SchedulerError::DailyCapReached { .. })) => {
                let error = e.to_string();
                let after = crate::scheduler::until_next_utc_day(now_unix());
                self.queue.reschedule(t.id, &self.id, &error, after).await?;
                ran.disposition = Disposition::Retrying { error, after };
            }
            Err(e) => {
                let error = e.to_string();
                let retry = e.retry_after(now_unix());
                self.queue.fail(t.id, &self.id, &error, retry).await?;
                ran.disposition = match retry {
                    Some(after) if t.attempts < t.max_attempts => {
                        Disposition::Retrying { error, after }
                    }
                    _ => Disposition::Failed { error },
                };
            }
        }
        Ok(ran)
    }

    fn payload_of(&self, c: &ClaimedTask) -> Result<TaskPayload, String> {
        let t = &c.task;
        let p: TaskPayload = serde_json::from_value(t.payload.clone())
            .map_err(|e| format!("malformed task payload: {e}"))?;
        if p.input.repo != t.repo || p.input.ticket != t.ticket {
            return Err("task payload names another repo or ticket".into());
        }
        match (t.role, &p.sealed) {
            (Role::Coder, None) => Err("coder task payload has no sealed spec".into()),
            (Role::Qa, Some(_)) => Err("qa task payload carries a sealed spec".into()),
            _ => Ok(p),
        }
    }

    /// Runs `work`, extending the lease every `lease / 3` until it finishes.
    /// A failed heartbeat (lease lost) does not abort the work, which must
    /// still tear down its sandbox and key; `complete`/`fail` then report it.
    async fn with_heartbeat<F>(&self, id: TaskId, work: F) -> Result<StageReport, PipelineError>
    where
        F: std::future::Future<Output = Result<StageReport, PipelineError>>,
    {
        let every = (self.lease / 3).max(Duration::from_millis(10));
        let beat = async {
            let mut interval = tokio::time::interval(every);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await; // immediate first tick
            loop {
                interval.tick().await;
                let _ = self.queue.heartbeat(id, &self.id, self.lease).await;
            }
        };
        tokio::pin!(work);
        tokio::select! {
            biased;
            r = &mut work => r,
            _ = beat => unreachable!("the heartbeat loop never ends"),
        }
    }
}
