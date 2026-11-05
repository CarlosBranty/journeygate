//! The three slots of architecture section 16.
//!
//! 1. **Host slots** (`[scheduler].host_slots`): concurrent sandboxes on this
//!    host. A claimed task holds one [`TaskSlot`] for its whole life (its
//!    attempts are sequential); every extra best-of-N candidate takes one more.
//! 2. **Provider slots** (`[scheduler].provider_slots`): concurrent attempts
//!    per model provider prefix ([`crate::config::provider_of`]). Providers
//!    without an entry are not limited here (the gateway still rate-limits).
//! 3. **Daily USD cap** (`[scheduler].daily_usd_cap`): no new attempt starts
//!    once today's (UTC) spend reaches it; running attempts finish. A ledger
//!    that cannot answer fails closed (no new attempt).
//!
//! File conflicts are the queue's job: `TaskQueue::claim` skips a task whose
//! predicted files intersect a running task, so the new task waits.

use crate::config::{SchedulerConfig, provider_of};
use guard_core::ports::StoreError;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SchedulerError {
    #[error("daily USD cap reached: spent {spent:.4} of {cap:.4}")]
    DailyCapReached { spent: f64, cap: f64 },
    #[error("spend ledger unavailable: {0}")]
    Ledger(StoreError),
}

/// Today's (UTC day) gateway spend in USD.
#[async_trait::async_trait]
pub trait SpendLedger: Send + Sync {
    async fn spent_today_usd(&self) -> Result<f64, StoreError>;
}

/// A settable in-memory ledger (tests, CI-native mode without Postgres).
#[derive(Debug)]
pub struct FixedSpend {
    state: Mutex<Result<f64, StoreError>>,
}

impl FixedSpend {
    pub fn new(spent: f64) -> Self {
        Self {
            state: Mutex::new(Ok(spent)),
        }
    }

    pub fn set(&self, spent: f64) {
        *lock(&self.state) = Ok(spent);
    }

    pub fn add(&self, usd: f64) {
        let mut s = lock(&self.state);
        if let Ok(v) = s.as_mut() {
            *v += usd;
        }
    }

    pub fn fail_with(&self, error: StoreError) {
        *lock(&self.state) = Err(error);
    }
}

impl Default for FixedSpend {
    fn default() -> Self {
        Self::new(0.0)
    }
}

#[async_trait::async_trait]
impl SpendLedger for FixedSpend {
    async fn spent_today_usd(&self) -> Result<f64, StoreError> {
        lock(&self.state).clone()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Delay from `unix_secs` until the next UTC midnight (where the daily cap resets).
pub fn until_next_utc_day(unix_secs: u64) -> Duration {
    const DAY: u64 = 86_400;
    Duration::from_secs(DAY - unix_secs % DAY)
}

/// One host slot, held by a claimed task. Released on drop.
#[derive(Debug)]
pub struct TaskSlot {
    _permit: OwnedSemaphorePermit,
}

/// Permission to start one attempt: its provider slot (if limited). Released on drop.
#[derive(Debug)]
pub struct AttemptPermit {
    provider: String,
    _permit: Option<OwnedSemaphorePermit>,
}

impl AttemptPermit {
    pub fn provider(&self) -> &str {
        &self.provider
    }
}

pub struct Slots {
    host: Arc<Semaphore>,
    providers: BTreeMap<String, Arc<Semaphore>>,
    ledger: Arc<dyn SpendLedger>,
    daily_cap: f64,
}

impl Slots {
    pub fn new(config: &SchedulerConfig, ledger: Arc<dyn SpendLedger>) -> Self {
        Self {
            host: Arc::new(Semaphore::new(config.host_slots as usize)),
            providers: config
                .provider_slots
                .iter()
                .map(|(p, n)| (p.clone(), Arc::new(Semaphore::new(*n as usize))))
                .collect(),
            ledger,
            daily_cap: config.daily_usd_cap,
        }
    }

    /// A host slot now, or `None` when the host is full.
    pub fn try_task_slot(&self) -> Option<TaskSlot> {
        self.host
            .clone()
            .try_acquire_owned()
            .ok()
            .map(|p| TaskSlot { _permit: p })
    }

    /// Waits for a host slot (extra best-of-N candidates).
    pub async fn task_slot(&self) -> TaskSlot {
        let permit = self
            .host
            .clone()
            .acquire_owned()
            .await
            .expect("host semaphore is never closed");
        TaskSlot { _permit: permit }
    }

    pub fn free_host_slots(&self) -> usize {
        self.host.available_permits()
    }

    /// `Err` once today's spend reaches the cap or the ledger cannot answer.
    pub async fn check_budget(&self) -> Result<(), SchedulerError> {
        let spent = self
            .ledger
            .spent_today_usd()
            .await
            .map_err(SchedulerError::Ledger)?;
        if spent >= self.daily_cap {
            return Err(SchedulerError::DailyCapReached {
                spent,
                cap: self.daily_cap,
            });
        }
        Ok(())
    }

    /// Budget check, then waits for the provider slot of `model`.
    pub async fn attempt_permit(&self, model: &str) -> Result<AttemptPermit, SchedulerError> {
        self.check_budget().await?;
        let provider = provider_of(model);
        let permit = match self.providers.get(&provider) {
            Some(sem) => Some(
                sem.clone()
                    .acquire_owned()
                    .await
                    .expect("provider semaphore is never closed"),
            ),
            None => None,
        };
        Ok(AttemptPermit {
            provider,
            _permit: permit,
        })
    }
}
