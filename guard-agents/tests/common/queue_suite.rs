//! `TaskQueue` semantics every implementation shares (ports/task_queue.rs).
//! Run against `InMemoryTaskQueue` (always) and `PgTaskQueue` (live).

use guard_core::ports::{NewTask, Role, StoreError, TaskId, TaskQueue, TaskState};
use std::time::Duration;

const LEASE: Duration = Duration::from_secs(300);

pub fn task(ticket: &str, role: Role, priority: i32, files: &[&str]) -> NewTask {
    NewTask {
        repo: "acme/shop".into(),
        ticket: ticket.into(),
        role,
        priority,
        predicted_files: files.iter().map(|s| s.to_string()).collect(),
        payload: serde_json::json!({ "agent": "coder" }),
        max_attempts: 3,
    }
}

/// Runs every shared case; `state_of` reads a task's state back.
pub async fn run_all<F, Fut>(make: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Box<dyn QueueUnderTest>>,
{
    dedupe_per_repo_ticket_role(make().await).await;
    claim_order_and_attempts(make().await).await;
    conflicting_tasks_wait(make().await).await;
    holder_checks(make().await).await;
    fail_without_retry_ends_failed(make().await).await;
    fail_with_retry_requeues_until_max(make().await).await;
    reschedule_gives_the_attempt_back(make().await).await;
}

#[async_trait::async_trait]
pub trait QueueUnderTest: Send + Sync {
    fn queue(&self) -> &dyn TaskQueue;
    async fn state(&self, id: TaskId) -> TaskState;
    async fn attempts(&self, id: TaskId) -> u32;
}

async fn dedupe_per_repo_ticket_role(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    let a = queue
        .enqueue(&task("SHOP-1", Role::Qa, 0, &[]))
        .await
        .unwrap();
    assert!(a.created);
    let again = queue
        .enqueue(&task("SHOP-1", Role::Qa, 5, &[]))
        .await
        .unwrap();
    assert_eq!(again.id, a.id);
    assert!(!again.created, "active (ticket, role) is deduped");
    let coder = queue
        .enqueue(&task("SHOP-1", Role::Coder, 0, &[]))
        .await
        .unwrap();
    assert!(
        coder.created && coder.id != a.id,
        "other role is another unit"
    );
    let mut other_repo = task("SHOP-1", Role::Qa, 0, &[]);
    other_repo.repo = "acme/other".into();
    assert!(queue.enqueue(&other_repo).await.unwrap().created);

    // Running is still active.
    let c = queue.claim("w1", LEASE).await.unwrap().unwrap();
    assert_eq!(c.task.id, a.id, "same priority: lowest id first");
    let running = queue
        .enqueue(&task("SHOP-1", Role::Qa, 0, &[]))
        .await
        .unwrap();
    assert!(!running.created && running.id == a.id);
    queue.complete(c.task.id, "w1").await.unwrap();
    // Finished: the pair can be enqueued again.
    let fresh = queue
        .enqueue(&task("SHOP-1", Role::Qa, 0, &[]))
        .await
        .unwrap();
    assert!(fresh.created && fresh.id != a.id);

    let mut bad = task("SHOP-9", Role::Qa, 0, &[]);
    bad.max_attempts = 0;
    assert!(matches!(
        queue.enqueue(&bad).await,
        Err(StoreError::Invalid(_))
    ));
}

async fn claim_order_and_attempts(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    assert!(queue.claim("w", LEASE).await.unwrap().is_none(), "empty");
    let low = queue.enqueue(&task("T-1", Role::Qa, 0, &[])).await.unwrap();
    let high = queue.enqueue(&task("T-2", Role::Qa, 9, &[])).await.unwrap();
    let low2 = queue.enqueue(&task("T-3", Role::Qa, 0, &[])).await.unwrap();

    let first = queue.claim("w", LEASE).await.unwrap().unwrap();
    assert_eq!(first.task.id, high.id, "higher priority first");
    assert_eq!(first.task.state, TaskState::Running);
    assert_eq!(first.task.attempts, 1);
    assert_eq!(first.task.locked_by.as_deref(), Some("w"));
    assert_eq!(first.lease, LEASE);
    assert_eq!(first.task.payload["agent"], "coder");
    let second = queue.claim("w", LEASE).await.unwrap().unwrap();
    assert_eq!(second.task.id, low.id, "then older / lower id");
    let third = queue.claim("w", LEASE).await.unwrap().unwrap();
    assert_eq!(third.task.id, low2.id);
    assert!(queue.claim("w", LEASE).await.unwrap().is_none());
    assert_eq!(q.attempts(high.id).await, 1);
}

async fn conflicting_tasks_wait(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    let a = queue
        .enqueue(&task("C-1", Role::Coder, 5, &["src/cart.ts", "src/pay.ts"]))
        .await
        .unwrap();
    let b = queue
        .enqueue(&task("C-2", Role::Coder, 4, &["src/pay.ts"]))
        .await
        .unwrap();
    let c = queue
        .enqueue(&task("C-3", Role::Coder, 3, &["src/other.ts"]))
        .await
        .unwrap();

    let first = queue.claim("w1", LEASE).await.unwrap().unwrap();
    assert_eq!(first.task.id, a.id);
    assert_eq!(
        queue.conflicts(&["src/pay.ts".to_string()]).await.unwrap(),
        vec![a.id]
    );
    assert!(
        queue
            .conflicts(&["src/none.ts".to_string()])
            .await
            .unwrap()
            .is_empty()
    );
    let second = queue.claim("w2", LEASE).await.unwrap().unwrap();
    assert_eq!(
        second.task.id, c.id,
        "C-2 shares src/pay.ts with C-1 and waits"
    );
    assert!(queue.claim("w3", LEASE).await.unwrap().is_none());
    assert_eq!(q.state(b.id).await, TaskState::Queued);

    queue.complete(a.id, "w1").await.unwrap();
    let third = queue.claim("w3", LEASE).await.unwrap().unwrap();
    assert_eq!(third.task.id, b.id, "runs once C-1 is done");
}

async fn holder_checks(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    let t = queue.enqueue(&task("H-1", Role::Qa, 0, &[])).await.unwrap();
    for r in [
        queue.heartbeat(t.id, "w", LEASE).await,
        queue.complete(t.id, "w").await,
        queue.fail(t.id, "w", "x", None).await,
    ] {
        assert!(matches!(r, Err(StoreError::Invalid(_))), "queued: {r:?}");
    }
    queue.claim("owner", LEASE).await.unwrap().unwrap();
    assert!(matches!(
        queue.heartbeat(t.id, "intruder", LEASE).await,
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        queue.complete(t.id, "intruder").await,
        Err(StoreError::Invalid(_))
    ));
    queue.heartbeat(t.id, "owner", LEASE).await.unwrap();
    queue.complete(t.id, "owner").await.unwrap();
    assert_eq!(q.state(t.id).await, TaskState::Succeeded);
    assert!(matches!(
        queue.complete(t.id, "owner").await,
        Err(StoreError::Invalid(_))
    ));
}

async fn fail_without_retry_ends_failed(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    let t = queue.enqueue(&task("F-1", Role::Qa, 0, &[])).await.unwrap();
    queue.claim("w", LEASE).await.unwrap().unwrap();
    queue.fail(t.id, "w", "bad payload", None).await.unwrap();
    assert_eq!(q.state(t.id).await, TaskState::Failed);
    assert!(queue.claim("w", LEASE).await.unwrap().is_none());
}

async fn fail_with_retry_requeues_until_max(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    let mut nt = task("R-1", Role::Coder, 0, &[]);
    nt.max_attempts = 2;
    let t = queue.enqueue(&nt).await.unwrap();
    queue.claim("w", LEASE).await.unwrap().unwrap();
    queue
        .fail(t.id, "w", "sandbox unavailable", Some(Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(q.state(t.id).await, TaskState::Queued);
    // Let the zero delay pass on any clock.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let again = queue.claim("w", LEASE).await.unwrap().unwrap();
    assert_eq!(again.task.attempts, 2);
    assert_eq!(
        again.task.last_error.as_deref(),
        Some("sandbox unavailable")
    );
    queue
        .fail(t.id, "w", "still down", Some(Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(
        q.state(t.id).await,
        TaskState::Failed,
        "max_attempts reached"
    );
}

async fn reschedule_gives_the_attempt_back(q: Box<dyn QueueUnderTest>) {
    let queue = q.queue();
    let mut nt = task("S-1", Role::Coder, 0, &[]);
    nt.max_attempts = 1;
    let t = queue.enqueue(&nt).await.unwrap();
    assert!(matches!(
        queue.reschedule(t.id, "w", "cap", Duration::ZERO).await,
        Err(StoreError::Invalid(_))
    ));
    for _ in 0..3 {
        queue.claim("w", LEASE).await.unwrap().unwrap();
        queue
            .reschedule(t.id, "w", "daily USD cap reached", Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(q.state(t.id).await, TaskState::Queued);
        assert_eq!(q.attempts(t.id).await, 0);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let again = queue.claim("w", LEASE).await.unwrap().unwrap();
    assert_eq!(again.task.attempts, 1);
    assert_eq!(
        again.task.last_error.as_deref(),
        Some("daily USD cap reached")
    );
}
