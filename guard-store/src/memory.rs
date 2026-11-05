//! `MemoryStore`: the in-memory twin of [`crate::PgStore`]. Same ordering,
//! normalization, validation and job-lease rules (the shared suites in
//! `tests/` run against both), for CI-native mode and server tests.

use crate::flaky::{FlakyHistory, FlakyQuery, FlakyStat, aggregate};
use crate::jobs::{
    Enqueued, Job, JobId, JobQueue, JobStatus, NewJob, clamp_delay, lease_expired_error, lease_lost,
};
use crate::util::{check_decision, format_ts, normalize_label, parse_ts, to_micros};
use guard_core::ports::*;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;
use time::OffsetDateTime;

#[derive(Default)]
pub struct MemoryStore {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next_id: i64,
    decisions: Vec<(DecisionId, OffsetDateTime, TriageDecision)>,
    labels: Vec<(i64, OffsetDateTime, HumanLabel)>,
    gates: Vec<GateRow>,
    /// Bumped on every gate write: orders `received_at` ties like Postgres' id.
    gate_seq: u64,
    jobs: BTreeMap<i64, JobRow>,
}

struct GateRow {
    id: GateResultId,
    seq: u64,
    received_at: OffsetDateTime,
    report: GateReport,
    outcome: ReportOutcome,
}

struct JobRow {
    job: Job,
    run_after: OffsetDateTime,
    locked_until: Option<OffsetDateTime>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl State {
    fn next(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    fn sorted_gates(&self) -> Vec<&GateRow> {
        let mut gates: Vec<&GateRow> = self.gates.iter().collect();
        gates.sort_by(|a, b| b.received_at.cmp(&a.received_at).then(b.seq.cmp(&a.seq)));
        gates
    }
}

fn now() -> OffsetDateTime {
    to_micros(OffsetDateTime::now_utc())
}

fn stored(g: &GateRow) -> StoredGateResult {
    StoredGateResult {
        id: g.id,
        received_at: format_ts(g.received_at),
        report: g.report.clone(),
        outcome: g.outcome.clone(),
    }
}

fn plus(ts: OffsetDateTime, d: Duration) -> OffsetDateTime {
    let d = time::Duration::try_from(clamp_delay(d)).unwrap_or(time::Duration::ZERO);
    ts.checked_add(d).unwrap_or(ts)
}

#[async_trait::async_trait]
impl DecisionLog for MemoryStore {
    async fn record_decision(&self, decision: &TriageDecision) -> Result<DecisionId, StoreError> {
        let at = check_decision(decision)?;
        let mut s = self.lock();
        let id = DecisionId(s.next());
        s.decisions.push((id, at, decision.clone()));
        Ok(id)
    }

    async fn record_human_label(&self, label: &HumanLabel) -> Result<(), StoreError> {
        let (normalized, at) = normalize_label(label)?;
        let mut s = self.lock();
        let id = s.next();
        s.labels.push((id, at, normalized));
        Ok(())
    }

    async fn list_for_calibration(
        &self,
        query: &CalibrationQuery,
    ) -> Result<Vec<CalibrationRow>, StoreError> {
        let since = query
            .since
            .as_deref()
            .map(|v| parse_ts("since", v))
            .transpose()?;
        let limit = if query.limit == 0 {
            1000
        } else {
            query.limit as usize
        };
        let s = self.lock();
        // Latest decision per ticket by (decided_at, id).
        let mut latest: BTreeMap<&str, &(DecisionId, OffsetDateTime, TriageDecision)> =
            BTreeMap::new();
        for row in &s.decisions {
            let key = row.2.ticket_key.as_str();
            match latest.get(key) {
                Some(cur) if (cur.1, cur.0) >= (row.1, row.0) => {}
                _ => {
                    latest.insert(key, row);
                }
            }
        }
        let mut rows: Vec<_> = latest
            .into_values()
            .filter(|(_, at, d)| {
                since.is_none_or(|since| *at >= since) && query.mode.is_none_or(|m| m == d.mode)
            })
            .filter_map(|(id, at, d)| {
                let human = s
                    .labels
                    .iter()
                    .filter(|(_, _, l)| l.ticket_key == d.ticket_key)
                    .max_by_key(|(lid, lat, _)| (*lat, *lid))
                    .map(|(_, _, l)| l.clone());
                if query.labeled_only && human.is_none() {
                    return None;
                }
                Some((
                    *at,
                    CalibrationRow {
                        decision_id: *id,
                        decision: d.clone(),
                        human,
                    },
                ))
            })
            .collect();
        rows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.decision_id.cmp(&a.1.decision_id)));
        Ok(rows.into_iter().take(limit).map(|(_, r)| r).collect())
    }
}

#[async_trait::async_trait]
impl GateHistory for MemoryStore {
    async fn record_gate_result(
        &self,
        report: &GateReport,
        outcome: &ReportOutcome,
    ) -> Result<GateResultId, StoreError> {
        let mut s = self.lock();
        s.gate_seq += 1;
        let seq = s.gate_seq;
        let received_at = now();
        let repo = report.repo.as_deref().unwrap_or("");
        if let Some(g) = s.gates.iter_mut().find(|g| {
            g.report.repo.as_deref().unwrap_or("") == repo && g.report.run_id == report.run_id
        }) {
            g.report = report.clone();
            g.outcome = outcome.clone();
            g.received_at = received_at;
            g.seq = seq;
            return Ok(g.id);
        }
        let id = GateResultId(s.next());
        s.gates.push(GateRow {
            id,
            seq,
            received_at,
            report: report.clone(),
            outcome: outcome.clone(),
        });
        Ok(id)
    }

    async fn recent_gate_results(&self, limit: u32) -> Result<Vec<StoredGateResult>, StoreError> {
        let s = self.lock();
        Ok(s.sorted_gates()
            .into_iter()
            .take(limit as usize)
            .map(stored)
            .collect())
    }

    async fn latest_for_pr(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<Option<StoredGateResult>, StoreError> {
        let s = self.lock();
        Ok(s.sorted_gates()
            .into_iter()
            .find(|g| {
                g.report.repo.as_deref().unwrap_or("") == repo
                    && g.report.pr.as_ref().is_some_and(|p| p.number == number)
            })
            .map(stored))
    }
}

#[async_trait::async_trait]
impl FlakyHistory for MemoryStore {
    async fn flaky_stats(&self, query: &FlakyQuery) -> Result<Vec<FlakyStat>, StoreError> {
        let s = self.lock();
        let window = query.effective_window() as usize;
        let gates: Vec<&GateRow> = s
            .sorted_gates()
            .into_iter()
            .filter(|g| {
                query
                    .repo
                    .as_deref()
                    .is_none_or(|r| g.report.repo.as_deref().unwrap_or("") == r)
            })
            .take(window)
            .collect();
        Ok(aggregate(gates.iter().flat_map(|g| {
            g.report
                .tests
                .iter()
                .map(|t| (t.test_id.as_str(), t.journey.as_deref(), t.status))
        })))
    }
}

#[async_trait::async_trait]
impl JobQueue for MemoryStore {
    async fn enqueue(&self, job: &NewJob) -> Result<Enqueued, StoreError> {
        job.validate()?;
        let mut s = self.lock();
        if let Some(key) = &job.dedupe_key
            && let Some(existing) = s.jobs.values().find(|r| {
                r.job.queue == job.queue
                    && r.job.dedupe_key.as_ref() == Some(key)
                    && r.job.status.is_active()
            })
        {
            return Ok(Enqueued {
                id: existing.job.id,
                created: false,
            });
        }
        let id = JobId(s.next());
        s.jobs.insert(
            id.0,
            JobRow {
                job: Job {
                    id,
                    queue: job.queue.clone(),
                    dedupe_key: job.dedupe_key.clone(),
                    payload: job.payload.clone(),
                    priority: job.priority,
                    status: JobStatus::Queued,
                    attempts: 0,
                    max_attempts: job.max_attempts,
                    locked_by: None,
                    last_error: None,
                },
                run_after: now(),
                locked_until: None,
            },
        );
        Ok(Enqueued { id, created: true })
    }

    async fn claim(
        &self,
        queue: &str,
        worker: &str,
        lease: Duration,
    ) -> Result<Option<Job>, StoreError> {
        let now = now();
        let mut s = self.lock();
        let expired = |r: &JobRow| {
            r.job.status == JobStatus::Running && r.locked_until.is_some_and(|u| u <= now)
        };
        // Expired leases that used every attempt go dead first.
        for r in s.jobs.values_mut() {
            if r.job.queue == queue && expired(r) && r.job.attempts >= r.job.max_attempts {
                r.job.status = JobStatus::Dead;
                r.job.last_error = Some(lease_expired_error(r.job.attempts));
                r.job.locked_by = None;
                r.locked_until = None;
            }
        }
        let best = s
            .jobs
            .values()
            .filter(|r| {
                r.job.queue == queue
                    && ((r.job.status == JobStatus::Queued && r.run_after <= now) || expired(r))
            })
            .min_by_key(|r| (std::cmp::Reverse(r.job.priority), r.run_after, r.job.id))
            .map(|r| r.job.id);
        let Some(id) = best else { return Ok(None) };
        let r = s.jobs.get_mut(&id.0).expect("job just found");
        r.job.status = JobStatus::Running;
        r.job.attempts += 1;
        r.job.locked_by = Some(worker.to_string());
        r.locked_until = Some(plus(now, lease));
        Ok(Some(r.job.clone()))
    }

    async fn heartbeat(&self, id: JobId, worker: &str, lease: Duration) -> Result<(), StoreError> {
        let mut s = self.lock();
        let r = held(&mut s, id, worker)?;
        r.locked_until = Some(plus(now(), lease));
        Ok(())
    }

    async fn complete(&self, id: JobId, worker: &str) -> Result<(), StoreError> {
        let mut s = self.lock();
        let r = held(&mut s, id, worker)?;
        r.job.status = JobStatus::Succeeded;
        r.job.locked_by = None;
        r.locked_until = None;
        Ok(())
    }

    async fn fail(
        &self,
        id: JobId,
        worker: &str,
        error: &str,
        retry_after: Duration,
    ) -> Result<(), StoreError> {
        let mut s = self.lock();
        let r = held(&mut s, id, worker)?;
        r.job.last_error = Some(error.to_string());
        r.job.locked_by = None;
        r.locked_until = None;
        if r.job.attempts >= r.job.max_attempts {
            r.job.status = JobStatus::Dead;
        } else {
            r.job.status = JobStatus::Queued;
            r.run_after = plus(now(), retry_after);
        }
        Ok(())
    }

    async fn get(&self, id: JobId) -> Result<Option<Job>, StoreError> {
        Ok(self.lock().jobs.get(&id.0).map(|r| r.job.clone()))
    }
}

fn held<'a>(s: &'a mut State, id: JobId, worker: &str) -> Result<&'a mut JobRow, StoreError> {
    match s.jobs.get_mut(&id.0) {
        Some(r)
            if r.job.status == JobStatus::Running && r.job.locked_by.as_deref() == Some(worker) =>
        {
            Ok(r)
        }
        _ => Err(lease_lost(id, worker)),
    }
}
