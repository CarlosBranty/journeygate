//! Periodic orchestrator jobs of `guard serve` (the memory staleness job,
//! key and network reapers): each runs once at startup, then every
//! `interval`, until shutdown. A failing run is logged and retried at the
//! next tick; runs of one job never overlap.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// What one run returns: a one-line summary, or why it failed.
pub type JobFuture = Pin<Box<dyn Future<Output = Result<String, String>> + Send>>;

type JobFn = dyn Fn() -> JobFuture + Send + Sync;

#[derive(Clone)]
pub struct PeriodicJob {
    name: String,
    interval: Duration,
    run: Arc<JobFn>,
}

impl std::fmt::Debug for PeriodicJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeriodicJob")
            .field("name", &self.name)
            .field("interval", &self.interval)
            .finish()
    }
}

impl PeriodicJob {
    /// `interval` is clamped to at least 1 ms.
    pub fn new<F, Fut>(name: impl Into<String>, interval: Duration, run: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        Self {
            name: name.into(),
            interval: interval.max(Duration::from_millis(1)),
            run: Arc::new(move || Box::pin(run()) as JobFuture),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    pub async fn run_once(&self) -> Result<String, String> {
        (self.run)().await
    }
}

/// Jobs started by [`start`]; aborted by [`RunningJobs::stop`] or on drop.
pub struct RunningJobs {
    handles: Vec<JoinHandle<()>>,
}

impl RunningJobs {
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Aborts every job (a run in progress is cancelled) and waits for them.
    pub async fn stop(mut self) {
        for h in &self.handles {
            h.abort();
        }
        for h in self.handles.drain(..) {
            let _ = h.await;
        }
    }
}

impl Drop for RunningJobs {
    fn drop(&mut self) {
        for h in &self.handles {
            h.abort();
        }
    }
}

/// Spawns every job on the current tokio runtime.
pub fn start(jobs: &[PeriodicJob]) -> RunningJobs {
    let handles = jobs
        .iter()
        .cloned()
        .map(|job| {
            tokio::spawn(async move {
                let mut every = tokio::time::interval(job.interval);
                every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    every.tick().await;
                    match job.run_once().await {
                        Ok(summary) => tracing::info!(job = %job.name, %summary, "job ran"),
                        Err(error) => tracing::warn!(job = %job.name, %error, "job failed"),
                    }
                }
            })
        })
        .collect();
    RunningJobs { handles }
}
