//! Behavioral suite for the durable `JobQueue` (Phase 4 task queue).

use guard_core::ports::StoreError;
use guard_store::{JobQueue, JobStatus, NewJob};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

const LEASE: Duration = Duration::from_secs(300);
const LATER: Duration = Duration::from_secs(3600);

fn job(dedupe: Option<&str>) -> NewJob {
    NewJob {
        queue: "agent".into(),
        dedupe_key: dedupe.map(str::to_string),
        payload: serde_json::json!({"ticket": "SHOP-1", "role": "coder"}),
        priority: 0,
        max_attempts: 3,
    }
}

pub async fn enqueue_then_claim(q: &impl JobQueue) {
    let e = q.enqueue(&job(None)).await.unwrap();
    assert!(e.created);
    let claimed = q.claim("agent", "w1", LEASE).await.unwrap().expect("a job");
    assert_eq!(claimed.id, e.id);
    assert_eq!(claimed.status, JobStatus::Running);
    assert_eq!(claimed.attempts, 1);
    assert_eq!(claimed.locked_by.as_deref(), Some("w1"));
    assert_eq!(claimed.payload, job(None).payload);
    // Nothing left to claim.
    assert!(q.claim("agent", "w2", LEASE).await.unwrap().is_none());
    // Other queues are independent.
    assert!(q.claim("other", "w1", LEASE).await.unwrap().is_none());
}

pub async fn claim_order_is_priority_then_fifo(q: &impl JobQueue) {
    let a = q.enqueue(&job(None)).await.unwrap().id;
    let b = q.enqueue(&job(None)).await.unwrap().id;
    let urgent = q
        .enqueue(&NewJob {
            priority: 10,
            ..job(None)
        })
        .await
        .unwrap()
        .id;
    let order: Vec<_> = [
        q.claim("agent", "w", LEASE).await.unwrap().unwrap().id,
        q.claim("agent", "w", LEASE).await.unwrap().unwrap().id,
        q.claim("agent", "w", LEASE).await.unwrap().unwrap().id,
    ]
    .into();
    assert_eq!(order, [urgent, a, b]);
}

pub async fn dedupe_key_returns_active_job(q: &impl JobQueue) {
    let first = q.enqueue(&job(Some("SHOP-1:coder"))).await.unwrap();
    let again = q.enqueue(&job(Some("SHOP-1:coder"))).await.unwrap();
    assert!(first.created);
    assert!(!again.created);
    assert_eq!(again.id, first.id);
    // Still active while running.
    q.claim("agent", "w1", LEASE).await.unwrap().unwrap();
    let running = q.enqueue(&job(Some("SHOP-1:coder"))).await.unwrap();
    assert_eq!(running.id, first.id);
    // Once finished, the same key can be enqueued again.
    q.complete(first.id, "w1").await.unwrap();
    let fresh = q.enqueue(&job(Some("SHOP-1:coder"))).await.unwrap();
    assert!(fresh.created);
    assert_ne!(fresh.id, first.id);
    // A different key is a different job.
    let other = q.enqueue(&job(Some("SHOP-1:qa"))).await.unwrap();
    assert!(other.created);
}

pub async fn complete_requires_the_lease_holder(q: &impl JobQueue) {
    let id = q.enqueue(&job(None)).await.unwrap().id;
    q.claim("agent", "w1", LEASE).await.unwrap().unwrap();
    let err = q.complete(id, "intruder").await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    q.complete(id, "w1").await.unwrap();
    let done = q.get(id).await.unwrap().unwrap();
    assert_eq!(done.status, JobStatus::Succeeded);
    assert_eq!(done.locked_by, None);
    // Completing twice is rejected (not running any more).
    assert!(q.complete(id, "w1").await.is_err());
}

pub async fn fail_retries_until_max_attempts_then_dead(q: &impl JobQueue) {
    let id = q
        .enqueue(&NewJob {
            max_attempts: 2,
            ..job(None)
        })
        .await
        .unwrap()
        .id;
    q.claim("agent", "w1", LEASE).await.unwrap().unwrap();
    q.fail(id, "w1", "tests red", Duration::ZERO).await.unwrap();
    let retry = q.get(id).await.unwrap().unwrap();
    assert_eq!(retry.status, JobStatus::Queued);
    assert_eq!(retry.last_error.as_deref(), Some("tests red"));

    let again = q.claim("agent", "w2", LEASE).await.unwrap().unwrap();
    assert_eq!(again.id, id);
    assert_eq!(again.attempts, 2);
    q.fail(id, "w2", "still red", Duration::ZERO).await.unwrap();
    let dead = q.get(id).await.unwrap().unwrap();
    assert_eq!(dead.status, JobStatus::Dead);
    assert_eq!(dead.last_error.as_deref(), Some("still red"));
    assert!(q.claim("agent", "w3", LEASE).await.unwrap().is_none());
}

pub async fn fail_with_backoff_delays_the_retry(q: &impl JobQueue) {
    let id = q.enqueue(&job(None)).await.unwrap().id;
    q.claim("agent", "w1", LEASE).await.unwrap().unwrap();
    q.fail(id, "w1", "429", LATER).await.unwrap();
    assert!(q.claim("agent", "w1", LEASE).await.unwrap().is_none());
    assert_eq!(q.get(id).await.unwrap().unwrap().status, JobStatus::Queued);
}

pub async fn expired_lease_is_reclaimed(q: &impl JobQueue) {
    let id = q.enqueue(&job(None)).await.unwrap().id;
    // A zero lease expires immediately (the worker died).
    q.claim("agent", "w1", Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    let stolen = q.claim("agent", "w2", LEASE).await.unwrap().unwrap();
    assert_eq!(stolen.id, id);
    assert_eq!(stolen.attempts, 2);
    assert_eq!(stolen.locked_by.as_deref(), Some("w2"));
    // The old holder lost its lease.
    assert!(q.complete(id, "w1").await.is_err());
    q.complete(id, "w2").await.unwrap();
}

pub async fn expired_lease_at_max_attempts_goes_dead(q: &impl JobQueue) {
    let id = q
        .enqueue(&NewJob {
            max_attempts: 1,
            ..job(None)
        })
        .await
        .unwrap()
        .id;
    q.claim("agent", "w1", Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    assert!(q.claim("agent", "w2", LEASE).await.unwrap().is_none());
    let dead = q.get(id).await.unwrap().unwrap();
    assert_eq!(dead.status, JobStatus::Dead);
    assert!(dead.last_error.unwrap().contains("lease expired"));
}

pub async fn heartbeat_extends_only_own_lease(q: &impl JobQueue) {
    let id = q.enqueue(&job(None)).await.unwrap().id;
    q.claim("agent", "w1", Duration::ZERO)
        .await
        .unwrap()
        .unwrap();
    // Renewed before anyone stole it: no longer claimable.
    q.heartbeat(id, "w1", LEASE).await.unwrap();
    assert!(q.claim("agent", "w2", LEASE).await.unwrap().is_none());
    assert!(q.heartbeat(id, "w2", LEASE).await.is_err());
}

pub async fn unknown_job(q: &impl JobQueue) {
    assert!(q.get(guard_store::JobId(987_654)).await.unwrap().is_none());
    assert!(q.complete(guard_store::JobId(987_654), "w").await.is_err());
}

pub async fn invalid_new_job_is_rejected(q: &impl JobQueue) {
    let err = q
        .enqueue(&NewJob {
            max_attempts: 0,
            ..job(None)
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    let err = q
        .enqueue(&NewJob {
            queue: String::new(),
            ..job(None)
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
}

/// Many workers claiming at once never get the same job twice
/// (`FOR UPDATE SKIP LOCKED` in Postgres, the mutex in memory).
pub async fn concurrent_claims_are_exclusive<Q: JobQueue + 'static>(q: Arc<Q>) {
    const JOBS: usize = 20;
    for _ in 0..JOBS {
        q.enqueue(&job(None)).await.unwrap();
    }
    let mut handles = Vec::new();
    for w in 0..8 {
        let q = q.clone();
        handles.push(tokio::spawn(async move {
            let mut got = Vec::new();
            while let Some(j) = q.claim("agent", &format!("w{w}"), LEASE).await.unwrap() {
                got.push(j.id);
            }
            got
        }));
    }
    let mut all = Vec::new();
    for h in handles {
        all.extend(h.await.unwrap());
    }
    let unique: BTreeSet<_> = all.iter().copied().collect();
    assert_eq!(all.len(), JOBS, "every job claimed exactly once: {all:?}");
    assert_eq!(unique.len(), JOBS);
}
