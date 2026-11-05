//! Background work started by webhooks (`202` first, process after).
//!
//! A counter of in-flight tasks plus a `Notify`, so tests (and graceful
//! shutdown) can wait until every accepted delivery has been processed
//! without keeping finished `JoinHandle`s around.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct BackgroundTasks {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    in_flight: AtomicUsize,
    idle: Notify,
}

/// Decrements on drop, so a panicking task still counts as finished.
struct Guard(Arc<Inner>);

impl Drop for Guard {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl BackgroundTasks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `future` on the tokio runtime.
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.inner.in_flight.fetch_add(1, Ordering::AcqRel);
        let guard = Guard(self.inner.clone());
        tokio::spawn(async move {
            let _guard = guard;
            future.await;
        });
    }

    pub fn in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::Acquire)
    }

    /// Resolves once no task is running.
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.inner.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.in_flight() == 0 {
                return;
            }
            notified.await;
        }
    }

    /// [`wait_idle`](Self::wait_idle) for at most `grace`; `false` when tasks
    /// were still running (shutdown must finish before the orchestrator's
    /// SIGKILL, e.g. `docker stop`'s 10 s).
    pub async fn drain(&self, grace: std::time::Duration) -> bool {
        tokio::time::timeout(grace, self.wait_idle()).await.is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn wait_idle_waits_for_spawned_work() {
        let tasks = BackgroundTasks::new();
        let done = Arc::new(AtomicBool::new(false));
        let flag = done.clone();
        tasks.spawn(async move {
            tokio::task::yield_now().await;
            flag.store(true, Ordering::SeqCst);
        });
        tasks.wait_idle().await;
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(tasks.in_flight(), 0);
    }

    #[tokio::test]
    async fn a_panicking_task_still_finishes() {
        let tasks = BackgroundTasks::new();
        tasks.spawn(async { panic!("boom") });
        tasks.wait_idle().await;
        assert_eq!(tasks.in_flight(), 0);
    }

    #[tokio::test]
    async fn drain_gives_up_after_the_grace_period() {
        let tasks = BackgroundTasks::new();
        tasks.spawn(std::future::pending());
        let drained = tasks.drain(std::time::Duration::from_millis(50)).await;
        assert!(!drained);
        assert_eq!(tasks.in_flight(), 1);
        let idle = BackgroundTasks::new();
        assert!(idle.drain(std::time::Duration::from_millis(50)).await);
    }

    #[tokio::test]
    async fn idle_without_tasks_returns_at_once() {
        BackgroundTasks::new().wait_idle().await;
    }
}
