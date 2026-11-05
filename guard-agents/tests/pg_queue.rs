//! `PgTaskQueue` and `PgSpendLedger`. The shared suite also runs against
//! `InMemoryTaskQueue` so both implementations keep the same semantics.
//! Live: `DATABASE_URL=... cargo test -p guard-agents --test pg_queue -- --include-ignored`.

mod common;

use common::queue_suite::{self, QueueUnderTest, task};
use guard_agents::pg_queue::{PgSpendLedger, PgTaskQueue};
use guard_agents::scheduler::SpendLedger;
use guard_core::ports::fakes::InMemoryTaskQueue;
use guard_core::ports::{Role, TaskId, TaskQueue, TaskState};
use guard_store::sqlx;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

const LEASE: Duration = Duration::from_secs(300);

struct Mem(InMemoryTaskQueue);

#[async_trait::async_trait]
impl QueueUnderTest for Mem {
    fn queue(&self) -> &dyn TaskQueue {
        &self.0
    }
    async fn state(&self, id: TaskId) -> TaskState {
        self.0.get(id).unwrap().state
    }
    async fn attempts(&self, id: TaskId) -> u32 {
        self.0.get(id).unwrap().attempts
    }
}

struct Pg(PgTaskQueue);

#[async_trait::async_trait]
impl QueueUnderTest for Pg {
    fn queue(&self) -> &dyn TaskQueue {
        &self.0
    }
    async fn state(&self, id: TaskId) -> TaskState {
        self.0.get(id).await.unwrap().unwrap().state
    }
    async fn attempts(&self, id: TaskId) -> u32 {
        self.0.get(id).await.unwrap().unwrap().attempts
    }
}

#[tokio::test]
async fn in_memory_queue_passes_the_shared_suite() {
    queue_suite::run_all(|| async { Box::new(Mem(InMemoryTaskQueue::new())) as Box<_> }).await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn pg_queue_passes_the_shared_suite() {
    let Some(h) = common::pg().await else { return };
    let pool = h.pool.clone();
    queue_suite::run_all(|| {
        let pool = pool.clone();
        async move {
            sqlx::query("TRUNCATE task_queue RESTART IDENTITY CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            Box::new(Pg(PgTaskQueue::new(pool))) as Box<_>
        }
    })
    .await;
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn expired_lease_is_reclaimed_then_fails_at_max_attempts() {
    let Some(h) = common::pg().await else { return };
    let q = PgTaskQueue::new(h.pool.clone());
    let mut nt = task("L-1", Role::Coder, 0, &["src/a.ts"]);
    nt.max_attempts = 2;
    let t = q.enqueue(&nt).await.unwrap();
    // Generous: the claim lock is per database, so parallel tests in other
    // schemas can delay a claim by tens of milliseconds.
    let short = Duration::from_millis(1500);
    let first = q.claim("crashed-worker", short).await.unwrap().unwrap();
    assert_eq!(first.task.attempts, 1);
    // A live lease blocks a conflicting task and is not reclaimable.
    q.enqueue(&task("L-2", Role::Coder, 0, &["src/a.ts"]))
        .await
        .unwrap();
    assert!(q.claim("w2", LEASE).await.unwrap().is_none());
    tokio::time::sleep(Duration::from_millis(2000)).await;
    // Visibility timeout passed: the lease is free again, attempts grows.
    let second = q.claim("w2", short).await.unwrap().unwrap();
    assert_eq!(second.task.id, t.id);
    assert_eq!(second.task.attempts, 2);
    assert_eq!(second.task.locked_by.as_deref(), Some("w2"));
    // The old holder lost the lease.
    assert!(q.complete(t.id, "crashed-worker").await.is_err());
    tokio::time::sleep(Duration::from_millis(2000)).await;
    // Expired at max_attempts: failed; L-2 is claimable now.
    let next = q.claim("w3", LEASE).await.unwrap().unwrap();
    assert_eq!(next.task.ticket, "L-2");
    let row = q.get(t.id).await.unwrap().unwrap();
    assert_eq!(row.state, TaskState::Failed);
    assert!(row.last_error.unwrap().contains("lease expired"));
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn heartbeat_extends_the_lease() {
    let Some(h) = common::pg().await else { return };
    let q = PgTaskQueue::new(h.pool.clone());
    let t = q.enqueue(&task("HB-1", Role::Qa, 0, &[])).await.unwrap();
    q.claim("w1", Duration::from_millis(200))
        .await
        .unwrap()
        .unwrap();
    q.heartbeat(t.id, "w1", LEASE).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        q.claim("w2", LEASE).await.unwrap().is_none(),
        "lease was extended"
    );
    q.complete(t.id, "w1").await.unwrap();
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn retry_delay_is_respected() {
    let Some(h) = common::pg().await else { return };
    let q = PgTaskQueue::new(h.pool.clone());
    let t = q.enqueue(&task("D-1", Role::Qa, 0, &[])).await.unwrap();
    q.claim("w", LEASE).await.unwrap().unwrap();
    q.fail(t.id, "w", "later", Some(Duration::from_secs(3600)))
        .await
        .unwrap();
    assert_eq!(q.get(t.id).await.unwrap().unwrap().state, TaskState::Queued);
    assert!(q.claim("w", LEASE).await.unwrap().is_none(), "delayed");
    h.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL"]
async fn concurrent_claimers_never_share_a_task() {
    let Some(h) = common::pg().await else { return };
    let q = Arc::new(PgTaskQueue::new(h.pool.clone()));
    for i in 0..40 {
        q.enqueue(&task(&format!("P-{i}"), Role::Coder, i % 3, &[]))
            .await
            .unwrap();
    }
    let mut workers = Vec::new();
    for w in 0..8 {
        let q = q.clone();
        workers.push(tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(c) = q.claim(&format!("w{w}"), LEASE).await.unwrap() {
                got.push(c.task.id);
            }
            got
        }));
    }
    let mut all = Vec::new();
    for w in workers {
        all.extend(w.await.unwrap());
    }
    let unique: BTreeSet<TaskId> = all.iter().copied().collect();
    assert_eq!(all.len(), 40, "every task claimed");
    assert_eq!(unique.len(), 40, "never twice");
    h.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL"]
async fn concurrent_claimers_respect_file_conflicts() {
    let Some(h) = common::pg().await else { return };
    let q = Arc::new(PgTaskQueue::new(h.pool.clone()));
    for i in 0..10 {
        q.enqueue(&task(&format!("X-{i}"), Role::Coder, 0, &["src/shared.ts"]))
            .await
            .unwrap();
    }
    let mut claims = Vec::new();
    for w in 0..8 {
        let q = q.clone();
        claims.push(tokio::spawn(async move {
            q.claim(&format!("w{w}"), LEASE).await.unwrap()
        }));
    }
    let mut won = 0;
    for c in claims {
        won += c.await.unwrap().is_some() as usize;
    }
    assert_eq!(
        won, 1,
        "only one task touching src/shared.ts runs at a time"
    );
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn spend_ledger_sums_the_current_utc_day() {
    let Some(h) = common::pg().await else { return };
    for (id, spend, when) in [
        ("r1", 1.25, "now()"),
        (
            "r2",
            2.5,
            "date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'",
        ),
        ("r3", 100.0, "now() - interval '2 days'"),
    ] {
        sqlx::query(&format!(
            "INSERT INTO spend_records (request_id, model, spend_usd, start_time, end_time)
             VALUES ($1, 'openai/gpt-5', $2, {when}, {when})"
        ))
        .bind(id)
        .bind(spend)
        .execute(&h.pool)
        .await
        .unwrap();
    }
    let ledger = PgSpendLedger::new(h.pool.clone());
    let spent = ledger.spent_today_usd().await.unwrap();
    assert!((spent - 3.75).abs() < 1e-9, "{spent}");
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn worker_runs_a_ticket_end_to_end_on_postgres() {
    use common::pipeline_fakes::*;
    use guard_agents::worker::{Disposition, Tick, Worker, enqueue_ticket};
    let Some(h) = common::pg().await else { return };
    let q = Arc::new(PgTaskQueue::new(h.pool.clone()));
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let mut fix = app_diff("src/a.ts", 2);
    fix.unified_diff.push_str("+// FIX\n");
    w.sandbox.coder_writes("coder", 1, fix);
    let qa = enqueue_ticket(q.as_ref(), &input(), 0).await.unwrap().id;
    let worker = Worker::new(
        "pg-w",
        q.clone(),
        w.slots.clone(),
        w.pipeline.clone(),
        LEASE,
    );

    let coder = match worker.tick().await.unwrap() {
        Tick::Ran(r) => match r.disposition {
            Disposition::Sealed { coder_task } => coder_task,
            other => panic!("{other:?}"),
        },
        other => panic!("{other:?}"),
    };
    let row = q.get(coder).await.unwrap().unwrap();
    assert_eq!(row.role, Role::Coder);
    assert_eq!(row.payload["sealed"]["path"], SPEC_PATH);
    assert!(matches!(
        worker.tick().await.unwrap(),
        Tick::Ran(r) if matches!(r.disposition, Disposition::Passed { .. })
    ));
    for id in [qa, coder] {
        assert_eq!(
            q.get(id).await.unwrap().unwrap().state,
            TaskState::Succeeded
        );
    }
    h.cleanup().await;
}
