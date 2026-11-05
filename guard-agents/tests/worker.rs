//! The queue worker: (ticket, qa) → seal → enqueue (ticket, coder) → PR,
//! with idle reasons, retries and lease heartbeats.

mod common;

use common::pipeline_fakes::*;
use guard_agents::pipeline::{HumanLaneReason, PipelineDeps, TicketPipeline};
use guard_agents::scheduler::{Slots, SpendLedger};
use guard_agents::worker::{
    Disposition, Idle, TaskPayload, Tick, Worker, enqueue_ticket, ticket_task,
};
use guard_core::ports::fakes::{FakeAgentRuntime, InMemoryTaskQueue};
use guard_core::ports::*;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn worker(w: &World, queue: Arc<dyn TaskQueue>) -> Worker {
    Worker::new(
        "host-a:1",
        queue,
        w.slots.clone(),
        w.pipeline.clone(),
        Duration::from_secs(300),
    )
}

fn ran(t: Tick) -> guard_agents::worker::Ran {
    match t {
        Tick::Ran(r) => *r,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn qa_task_seals_and_enqueues_the_coder_task() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes("coder", 1, {
        let mut d = app_diff("src/a.ts", 2);
        d.unified_diff.push_str("+// FIX\n");
        d
    });
    let queue = Arc::new(InMemoryTaskQueue::new());
    let qa_id = enqueue_ticket(queue.as_ref(), &input(), 0)
        .await
        .unwrap()
        .id;
    let worker = worker(&w, queue.clone());

    let first = ran(worker.tick().await.unwrap());
    assert_eq!(first.task, qa_id);
    assert_eq!(first.role, Role::Qa);
    let coder_id = match first.disposition {
        Disposition::Sealed { coder_task } => coder_task,
        other => panic!("{other:?}"),
    };
    assert_eq!(queue.get(qa_id).unwrap().state, TaskState::Succeeded);
    let coder_row = queue.get(coder_id).unwrap();
    assert_eq!(coder_row.role, Role::Coder);
    assert_eq!(
        coder_row.predicted_files,
        vec!["src/product-card.tsx".to_string()]
    );
    let payload: TaskPayload = serde_json::from_value(coder_row.payload).unwrap();
    assert_eq!(payload.sealed, Some(SealedSpec::seal(SPEC_PATH, GOOD_SPEC)));

    let second = ran(worker.tick().await.unwrap());
    assert_eq!(second.task, coder_id);
    assert!(matches!(second.disposition, Disposition::Passed { .. }));
    assert!(second.report.is_some());
    assert_eq!(queue.get(coder_id).unwrap().state, TaskState::Succeeded);
    assert!(matches!(
        worker.tick().await.unwrap(),
        Tick::Idle(Idle::NothingClaimable)
    ));
    assert_eq!(
        w.slots.free_host_slots(),
        4,
        "slot released after each task"
    );
}

#[tokio::test]
async fn human_lane_completes_the_task() {
    let w = world(config(false, None), completed_runtime(2));
    // QA never writes the spec: both attempts rejected.
    let queue = Arc::new(InMemoryTaskQueue::new());
    let id = enqueue_ticket(queue.as_ref(), &input(), 0)
        .await
        .unwrap()
        .id;
    let r = ran(worker(&w, queue.clone()).tick().await.unwrap());
    assert!(matches!(
        r.disposition,
        Disposition::HumanLane(HumanLaneReason::AttemptsExhausted { .. })
    ));
    assert_eq!(queue.get(id).unwrap().state, TaskState::Succeeded);
}

#[tokio::test]
async fn idle_when_host_slots_are_full_or_the_cap_is_reached() {
    let w = world(config(false, None), completed_runtime(1));
    let queue = Arc::new(InMemoryTaskQueue::new());
    let id = enqueue_ticket(queue.as_ref(), &input(), 0)
        .await
        .unwrap()
        .id;
    let worker = worker(&w, queue.clone());

    let held: Vec<_> = (0..4).map(|_| w.slots.try_task_slot().unwrap()).collect();
    assert!(matches!(
        worker.tick().await.unwrap(),
        Tick::Idle(Idle::HostSlotsFull)
    ));
    drop(held);

    w.spend.set(50.0);
    assert!(matches!(
        worker.tick().await.unwrap(),
        Tick::Idle(Idle::DailyCapReached)
    ));
    let row = queue.get(id).unwrap();
    assert_eq!(row.state, TaskState::Queued);
    assert_eq!(row.attempts, 0, "nothing was claimed");
}

#[tokio::test]
async fn conflicting_ticket_waits_for_the_running_one() {
    let w = world(config(false, None), completed_runtime(1));
    let queue = Arc::new(InMemoryTaskQueue::new());
    let mut other = input();
    other.ticket = "SHOP-102".into();
    other.run_id = "run-2".into();
    // SHOP-102 is running elsewhere and predicts the same file.
    enqueue_ticket(queue.as_ref(), &other, 10).await.unwrap();
    queue
        .claim("host-b:1", Duration::from_secs(300))
        .await
        .unwrap()
        .unwrap();
    let id = enqueue_ticket(queue.as_ref(), &input(), 0)
        .await
        .unwrap()
        .id;
    assert!(matches!(
        worker(&w, queue.clone()).tick().await.unwrap(),
        Tick::Idle(Idle::NothingClaimable)
    ));
    assert_eq!(queue.get(id).unwrap().state, TaskState::Queued);
}

#[tokio::test]
async fn bad_payload_fails_without_retry() {
    let w = world(config(false, None), completed_runtime(1));
    let queue = Arc::new(InMemoryTaskQueue::new());
    let mut t = ticket_task(&input(), Role::Qa, None).unwrap();
    t.payload = serde_json::json!({"nonsense": true});
    let id = queue.enqueue(&t).await.unwrap().id;
    let r = ran(worker(&w, queue.clone()).tick().await.unwrap());
    assert!(matches!(r.disposition, Disposition::Failed { .. }));
    assert_eq!(queue.get(id).unwrap().state, TaskState::Failed);

    // A coder task without a sealed spec is malformed too.
    assert!(ticket_task(&input(), Role::Coder, None).is_err());
}

/// Under the cap at claim time, over it when the attempt starts.
struct FlipLedger(AtomicU32);

#[async_trait::async_trait]
impl SpendLedger for FlipLedger {
    async fn spent_today_usd(&self) -> Result<f64, StoreError> {
        Ok(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            0.0
        } else {
            1e9
        })
    }
}

#[tokio::test]
async fn cap_reached_mid_task_requeues_for_the_next_utc_day() {
    let w = world(config(false, None), completed_runtime(1));
    let cfg = Arc::new(config(false, None));
    let slots = Arc::new(Slots::new(
        &cfg.scheduler,
        Arc::new(FlipLedger(AtomicU32::new(0))),
    ));
    let pipeline = Arc::new(
        TicketPipeline::new(PipelineDeps {
            config: cfg,
            slots: slots.clone(),
            runtimes: vec![w.runtime.clone()],
            sandbox: w.sandbox.clone(),
            gateway: w.gateway.clone(),
            evidence: w.evidence.clone(),
            layouts: w.layouts.clone(),
            checks: w.checks.clone(),
            publisher: w.publisher.clone(),
            facts: None,
        })
        .unwrap(),
    );
    let queue = Arc::new(InMemoryTaskQueue::new());
    let id = enqueue_ticket(queue.as_ref(), &input(), 0)
        .await
        .unwrap()
        .id;
    let worker = Worker::new(
        "w",
        queue.clone(),
        slots,
        pipeline,
        Duration::from_secs(300),
    );
    let r = ran(worker.tick().await.unwrap());
    match r.disposition {
        Disposition::Retrying { after, error } => {
            assert!(after > Duration::ZERO && after <= Duration::from_secs(86_400));
            assert!(error.contains("daily USD cap"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    let row = queue.get(id).unwrap();
    assert_eq!(row.state, TaskState::Queued);
    assert!(row.last_error.unwrap().contains("daily USD cap"));
}

/// Counts heartbeats; delegates everything to the in-memory queue.
struct CountingQueue {
    inner: InMemoryTaskQueue,
    beats: Mutex<u32>,
}

#[async_trait::async_trait]
impl TaskQueue for CountingQueue {
    async fn enqueue(&self, t: &NewTask) -> Result<TaskEnqueued, StoreError> {
        self.inner.enqueue(t).await
    }
    async fn claim(&self, w: &str, l: Duration) -> Result<Option<ClaimedTask>, StoreError> {
        self.inner.claim(w, l).await
    }
    async fn conflicts(&self, f: &[String]) -> Result<Vec<TaskId>, StoreError> {
        self.inner.conflicts(f).await
    }
    async fn heartbeat(&self, id: TaskId, w: &str, l: Duration) -> Result<(), StoreError> {
        *self.beats.lock().unwrap() += 1;
        self.inner.heartbeat(id, w, l).await
    }
    async fn complete(&self, id: TaskId, w: &str) -> Result<(), StoreError> {
        self.inner.complete(id, w).await
    }
    async fn fail(
        &self,
        id: TaskId,
        w: &str,
        e: &str,
        r: Option<Duration>,
    ) -> Result<(), StoreError> {
        self.inner.fail(id, w, e, r).await
    }
    async fn reschedule(
        &self,
        id: TaskId,
        w: &str,
        e: &str,
        after: Duration,
    ) -> Result<(), StoreError> {
        self.inner.reschedule(id, w, e, after).await
    }
}

/// Sleeps (virtual time) before answering like the wrapped fake.
struct SlowRuntime(FakeAgentRuntime, Duration);

#[async_trait::async_trait]
impl AgentRuntime for SlowRuntime {
    fn name(&self) -> &'static str {
        "fake"
    }
    async fn run(&self, s: &Sandbox, t: &AgentTask) -> Result<AgentOutcome, RuntimeError> {
        tokio::time::sleep(self.1).await;
        self.0.run(s, t).await
    }
}

#[tokio::test(start_paused = true)]
async fn long_tasks_heartbeat_at_a_third_of_the_lease() {
    let w = world(config(false, None), FakeAgentRuntime::new());
    let cfg = Arc::new(config(false, None));
    let slow = Arc::new(SlowRuntime(
        FakeAgentRuntime::new().with_outcome(Ok(FakeAgentRuntime::completed("qa", 0.1))),
        Duration::from_secs(350),
    ));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let pipeline = Arc::new(
        TicketPipeline::new(PipelineDeps {
            config: cfg,
            slots: w.slots.clone(),
            runtimes: vec![slow],
            sandbox: w.sandbox.clone(),
            gateway: w.gateway.clone(),
            evidence: w.evidence.clone(),
            layouts: w.layouts.clone(),
            checks: w.checks.clone(),
            publisher: w.publisher.clone(),
            facts: None,
        })
        .unwrap(),
    );
    let queue = Arc::new(CountingQueue {
        inner: InMemoryTaskQueue::new(),
        beats: Mutex::new(0),
    });
    enqueue_ticket(queue.as_ref(), &input(), 0).await.unwrap();
    let worker = Worker::new(
        "w",
        queue.clone(),
        w.slots.clone(),
        pipeline,
        Duration::from_secs(300),
    );
    let r = ran(worker.tick().await.unwrap());
    assert!(matches!(r.disposition, Disposition::Sealed { .. }));
    let beats = *queue.beats.lock().unwrap();
    assert_eq!(
        beats, 3,
        "350 s of work with a 300 s lease → beats at 100, 200, 300 s"
    );
}
