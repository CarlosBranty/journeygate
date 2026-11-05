//! Jira webhook redelivery handling: duplicate suppression and retries.
//!
//! Jira authenticates deliveries with a static secret only (no timestamp,
//! nonce or delivery id), and redelivers on its own. A delivery identical to
//! the previous one for the same `(event, issue)` within [`DEDUPE_WINDOW`] is
//! dropped, so a redelivered `issue_created` never triages twice. A label
//! change that is reverted and redone is not identical to the previous
//! delivery, so it is recorded each time.
//!
//! Accepted deliveries are processed after the `202`; a transient failure
//! (transport, rate limit, 5xx, store unavailable) is retried with
//! exponential backoff, [`RetryPolicy`]. The queue is in memory: a process
//! exit still loses the deliveries in flight (a durable queue needs a port).

use crate::triage::TriageError;
use guard_core::ports::{StoreError, TrackerError};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEDUPE_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Entries kept before expired ones are pruned.
const PRUNE_AT: usize = 4096;

#[derive(Clone, Default)]
pub struct RecentDeliveries {
    inner: Arc<Mutex<HashMap<String, (String, Instant)>>>,
}

impl RecentDeliveries {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `fingerprint` as the latest delivery for `key`; `false` when
    /// it repeats the previous one within the window (a duplicate).
    pub fn first_seen(&self, key: &str, fingerprint: &str) -> bool {
        self.first_seen_at(key, fingerprint, Instant::now())
    }

    fn first_seen_at(&self, key: &str, fingerprint: &str, now: Instant) -> bool {
        let mut map = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= PRUNE_AT {
            map.retain(|_, (_, at)| now.duration_since(*at) < DEDUPE_WINDOW);
        }
        if let Some((previous, at)) = map.get(key) {
            if previous == fingerprint && now.duration_since(*at) < DEDUPE_WINDOW {
                return false;
            }
        }
        map.insert(key.to_string(), (fingerprint.to_string(), now));
        true
    }

    /// Forgets `key` (its processing failed for good: a redelivery may retry).
    pub fn forget(&self, key: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
    }
}

/// Attempts and first backoff of webhook processing (the backoff doubles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub attempts: u32,
    pub initial_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 4,
            initial_backoff: Duration::from_secs(2),
        }
    }
}

/// Worth retrying: the next attempt may succeed without any change.
pub fn is_transient(error: &TriageError) -> bool {
    match error {
        TriageError::Tracker(TrackerError::Transport(_) | TrackerError::RateLimited) => true,
        TriageError::Tracker(TrackerError::Rejected { status, .. }) => *status >= 500,
        TriageError::Store(StoreError::Unavailable(_)) => true,
        _ => false,
    }
}

/// Runs `op` until it succeeds, fails for good, or the attempts run out.
pub async fn with_retries<T, F, Fut>(policy: RetryPolicy, mut op: F) -> Result<T, TriageError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, TriageError>>,
{
    let mut delay = policy.initial_backoff;
    let mut attempt = 1;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) if attempt < policy.attempts.max(1) && is_transient(&err) => {
                tracing::warn!(error = %err, attempt, "webhook processing failed; retrying");
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_consecutive_deliveries_are_duplicates_within_the_window() {
        let d = RecentDeliveries::new();
        let t0 = Instant::now();
        assert!(d.first_seen_at("created:SHOP-1", "", t0));
        assert!(!d.first_seen_at("created:SHOP-1", "", t0 + Duration::from_secs(5)));
        assert!(d.first_seen_at("created:SHOP-2", "", t0));
        assert!(d.first_seen_at("created:SHOP-1", "", t0 + DEDUPE_WINDOW * 2));
        assert!(d.first_seen_at("labels:SHOP-1", "a", t0));
        assert!(d.first_seen_at("labels:SHOP-1", "b", t0));
        assert!(d.first_seen_at("labels:SHOP-1", "a", t0));
        d.forget("labels:SHOP-1");
        assert!(d.first_seen_at("labels:SHOP-1", "a", t0));
    }

    #[tokio::test]
    async fn only_transient_errors_are_retried() {
        let policy = RetryPolicy {
            attempts: 3,
            initial_backoff: Duration::from_millis(1),
        };
        let mut calls = 0;
        let result: Result<(), _> = with_retries(policy, || {
            calls += 1;
            async { Err(TriageError::Tracker(TrackerError::Transport("x".into()))) }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls, 3);
        let mut calls = 0;
        let result: Result<(), _> = with_retries(policy, || {
            calls += 1;
            async {
                Err(TriageError::Tracker(TrackerError::NotFound(
                    "SHOP-1".into(),
                )))
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }
}
