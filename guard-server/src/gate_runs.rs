//! Idempotency of `POST /api/gate-result` (server-api.md): one report per
//! `(repo, run_id)` reaches Jira, however often CI retries or redelivers.
//!
//! Each run gets a `OnceCell` holding its reporting outcome: concurrent
//! deliveries of one run wait for the first, later ones reuse its outcome.
//! The cache is per process and bounded; across restarts the gate history is
//! searched for the run before reporting again.

use guard_core::ports::ReportOutcome;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// Runs remembered by one process (oldest evicted first).
pub const MAX_REMEMBERED_RUNS: usize = 1024;

type Key = (String, String);

#[derive(Clone, Default)]
pub struct GateRuns {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    cells: HashMap<Key, Arc<OnceCell<ReportOutcome>>>,
    order: VecDeque<Key>,
}

impl GateRuns {
    pub fn new() -> Self {
        Self::default()
    }

    /// The outcome cell of one run, created on first use.
    pub fn cell(&self, repo: &str, run_id: &str) -> Arc<OnceCell<ReportOutcome>> {
        let key = (repo.to_string(), run_id.to_string());
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cell) = inner.cells.get(&key) {
            return cell.clone();
        }
        let cell = Arc::new(OnceCell::new());
        inner.cells.insert(key.clone(), cell.clone());
        inner.order.push_back(key);
        while inner.order.len() > MAX_REMEMBERED_RUNS {
            if let Some(old) = inner.order.pop_front() {
                inner.cells.remove(&old);
            }
        }
        cell
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cells
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_cell_per_run_and_the_cache_is_bounded() {
        let runs = GateRuns::new();
        let a = runs.cell("acme/shop", "1");
        assert!(Arc::ptr_eq(&a, &runs.cell("acme/shop", "1")));
        assert!(!Arc::ptr_eq(&a, &runs.cell("acme/web", "1")));
        for i in 0..MAX_REMEMBERED_RUNS + 10 {
            runs.cell("r", &i.to_string());
        }
        assert_eq!(runs.len(), MAX_REMEMBERED_RUNS);
    }
}
