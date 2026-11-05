//! Fakes of the Phase 4 ports: agent runtime, sandbox provider, model
//! gateway, task queue and memory facts. Deterministic, no I/O, no network.

use super::lock;
use crate::ports::agent::*;
use crate::ports::decision_log::StoreError;
use crate::ports::memory_facts::*;
use crate::ports::model_gateway::*;
use crate::ports::sandbox::*;
use crate::ports::secret::Secret;
use crate::ports::task_queue::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const EPOCH: &str = "1970-01-01T00:00:00Z";

// ---------------------------------------------------------------- runtime

/// Scripted `AgentRuntime` named `fake`. Outcomes are consumed in order; once
/// exhausted, `run` fails with `RuntimeError::Unavailable`. Every task is
/// validated first (`RuntimeError::InvalidTask`) and recorded.
#[derive(Default)]
pub struct FakeAgentRuntime {
    outcomes: Mutex<VecDeque<Result<AgentOutcome, RuntimeError>>>,
    tasks: Mutex<Vec<(SandboxId, AgentTask)>>,
}

impl FakeAgentRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_outcome(self, outcome: Result<AgentOutcome, RuntimeError>) -> Self {
        lock(&self.outcomes).push_back(outcome);
        self
    }

    /// A `completed` outcome with one model-call event costing `cost_usd`.
    pub fn completed(summary: &str, cost_usd: f64) -> AgentOutcome {
        let usage = Usage {
            input_tokens: 1000,
            output_tokens: 100,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        AgentOutcome {
            runtime: "fake".into(),
            result: TaskResult {
                version: TASK_SCHEMA_VERSION,
                status: TaskStatus::Completed,
                summary: summary.into(),
                usage,
                cost_usd,
                steps: 1,
                memory_proposals: vec![],
                error: None,
            },
            events: vec![RuntimeEvent {
                seq: 1,
                ts: EPOCH.into(),
                event: RuntimeEventKind::ModelCall {
                    model: "fake/model".into(),
                    usage,
                    cost_usd,
                },
            }],
            exit_code: Some(0),
            duration_ms: 0,
        }
    }

    /// Every task `run` received, with the sandbox it ran in.
    pub fn tasks(&self) -> Vec<(SandboxId, AgentTask)> {
        lock(&self.tasks).clone()
    }
}

#[async_trait::async_trait]
impl AgentRuntime for FakeAgentRuntime {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn run(&self, sandbox: &Sandbox, task: &AgentTask) -> Result<AgentOutcome, RuntimeError> {
        task.validate().map_err(RuntimeError::InvalidTask)?;
        lock(&self.tasks).push((sandbox.id.clone(), task.clone()));
        lock(&self.outcomes).pop_front().unwrap_or_else(|| {
            Err(RuntimeError::Unavailable(
                "FakeAgentRuntime: no scripted outcome".into(),
            ))
        })
    }
}

// ---------------------------------------------------------------- sandbox

#[derive(Default)]
struct SandboxState {
    next: u64,
    specs: BTreeMap<SandboxId, SandboxSpec>,
    files: BTreeMap<SandboxId, BTreeMap<String, Vec<u8>>>,
    exec_log: BTreeMap<SandboxId, Vec<Vec<String>>>,
    destroyed: BTreeSet<SandboxId>,
}

/// In-memory `SandboxProvider`. Ids are `fake-sb-1`, `fake-sb-2`, ...
/// `exec` answers with the output scripted for `argv[0]` (default exit 0,
/// empty output); `extract_diff` returns the configured diff (default empty).
#[derive(Default)]
pub struct FakeSandboxProvider {
    state: Mutex<SandboxState>,
    exec_outputs: BTreeMap<String, ExecOutput>,
    diff: Option<ExtractedDiff>,
}

impl FakeSandboxProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_exec(mut self, program: &str, output: ExecOutput) -> Self {
        self.exec_outputs.insert(program.to_string(), output);
        self
    }

    pub fn with_diff(mut self, diff: ExtractedDiff) -> Self {
        self.diff = Some(diff);
        self
    }

    pub fn spec(&self, id: &SandboxId) -> Option<SandboxSpec> {
        lock(&self.state).specs.get(id).cloned()
    }

    pub fn exec_log(&self, id: &SandboxId) -> Vec<Vec<String>> {
        lock(&self.state)
            .exec_log
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn is_destroyed(&self, id: &SandboxId) -> bool {
        lock(&self.state).destroyed.contains(id)
    }

    fn live(s: &SandboxState, id: &SandboxId) -> Result<(), SandboxError> {
        if s.specs.contains_key(id) && !s.destroyed.contains(id) {
            Ok(())
        } else {
            Err(SandboxError::NotFound(id.0.clone()))
        }
    }
}

#[async_trait::async_trait]
impl SandboxProvider for FakeSandboxProvider {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox, SandboxError> {
        spec.validate().map_err(SandboxError::InvalidSpec)?;
        let mut s = lock(&self.state);
        s.next += 1;
        let id = SandboxId(format!("fake-sb-{}", s.next));
        s.specs.insert(id.clone(), spec.clone());
        Ok(Sandbox {
            id,
            role: spec.role,
            run_id: spec.layout.run_id.clone(),
            ticket: spec.layout.ticket.clone(),
            attempt: spec.layout.attempt,
        })
    }

    async fn put_file(
        &self,
        sandbox: &Sandbox,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SandboxError> {
        let mut s = lock(&self.state);
        Self::live(&s, &sandbox.id)?;
        s.files
            .entry(sandbox.id.clone())
            .or_default()
            .insert(path.to_string(), bytes);
        Ok(())
    }

    async fn read_file(&self, sandbox: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
        let s = lock(&self.state);
        Self::live(&s, &sandbox.id)?;
        s.files
            .get(&sandbox.id)
            .and_then(|f| f.get(path))
            .cloned()
            .ok_or_else(|| SandboxError::NotFound(path.to_string()))
    }

    async fn exec(
        &self,
        sandbox: &Sandbox,
        request: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
        let mut s = lock(&self.state);
        Self::live(&s, &sandbox.id)?;
        if request.argv.is_empty() {
            return Err(SandboxError::Exec("empty argv".into()));
        }
        s.exec_log
            .entry(sandbox.id.clone())
            .or_default()
            .push(request.argv.clone());
        Ok(self
            .exec_outputs
            .get(&request.argv[0])
            .cloned()
            .unwrap_or_default())
    }

    async fn extract_diff(
        &self,
        sandbox: &Sandbox,
        base_commit: &str,
    ) -> Result<ExtractedDiff, SandboxError> {
        let s = lock(&self.state);
        Self::live(&s, &sandbox.id)?;
        Ok(self.diff.clone().unwrap_or_else(|| ExtractedDiff {
            base_commit: base_commit.to_string(),
            unified_diff: String::new(),
            changed_paths: vec![],
        }))
    }

    async fn destroy(&self, sandbox: &Sandbox) -> Result<(), SandboxError> {
        let mut s = lock(&self.state);
        if !s.specs.contains_key(&sandbox.id) {
            return Err(SandboxError::NotFound(sandbox.id.0.clone()));
        }
        s.destroyed.insert(sandbox.id.clone());
        s.files.remove(&sandbox.id);
        Ok(())
    }
}

// ---------------------------------------------------------------- gateway

#[derive(Default)]
struct GatewayState {
    next: u64,
    active: BTreeMap<String, KeyRequest>,
    created_at: BTreeMap<String, String>,
    spend: Vec<SpendRecord>,
}

/// In-memory LiteLLM. Tokens are `sk-fake-<n>`, key ids `key-<n>`. Spend rows
/// are seeded with [`FakeModelGateway::push_spend`] and paged in insertion order,
/// filtered by `key_alias` (dates are not filtered).
#[derive(Default)]
pub struct FakeModelGateway {
    state: Mutex<GatewayState>,
}

impl FakeModelGateway {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_spend(&self, record: SpendRecord) {
        lock(&self.state).spend.push(record);
    }

    pub fn is_active(&self, key_id: &str) -> bool {
        lock(&self.state).active.contains_key(key_id)
    }

    /// What [`ModelGateway::list_keys`] reports as the key's `created_at`
    /// (unset by default: the fake has no clock).
    pub fn set_created_at(&self, key_id: &str, rfc3339: &str) {
        lock(&self.state)
            .created_at
            .insert(key_id.to_string(), rfc3339.to_string());
    }

    /// The request a still-active key was minted with.
    pub fn minted(&self, key_id: &str) -> Option<KeyRequest> {
        lock(&self.state).active.get(key_id).cloned()
    }
}

#[async_trait::async_trait]
impl ModelGateway for FakeModelGateway {
    async fn mint_key(&self, request: &KeyRequest) -> Result<VirtualKey, GatewayError> {
        request.validate().map_err(GatewayError::InvalidRequest)?;
        let mut s = lock(&self.state);
        // LiteLLM refuses a duplicate `key_alias`; the port says aliases are unique.
        if s.active.values().any(|r| r.alias == request.alias) {
            return Err(GatewayError::InvalidRequest(format!(
                "key_alias {:?} already exists",
                request.alias
            )));
        }
        s.next += 1;
        let key_id = format!("key-{}", s.next);
        s.active.insert(key_id.clone(), request.clone());
        Ok(VirtualKey {
            token: Secret::new(format!("sk-fake-{}", s.next)),
            key_id,
            alias: request.alias.clone(),
            models: request.models.clone(),
            max_budget_usd: request.max_budget_usd,
            expires_at: None,
        })
    }

    async fn revoke(&self, key_id: &str) -> Result<(), GatewayError> {
        lock(&self.state)
            .active
            .remove(key_id)
            .map(|_| ())
            .ok_or_else(|| GatewayError::NotFound(key_id.to_string()))
    }

    async fn list_keys(&self, alias_prefix: &str) -> Result<Vec<KeyInfo>, GatewayError> {
        let s = lock(&self.state);
        let mut keys: Vec<KeyInfo> = s
            .active
            .iter()
            .filter(|(_, r)| r.alias.starts_with(alias_prefix))
            .map(|(id, r)| KeyInfo {
                key_id: id.clone(),
                alias: Some(r.alias.clone()),
                created_at: s.created_at.get(id).cloned(),
                expires_at: None,
                metadata: r.metadata.clone(),
            })
            .collect();
        keys.sort_by(|a, b| (&a.alias, &a.key_id).cmp(&(&b.alias, &b.key_id)));
        Ok(keys)
    }

    async fn spend_logs(&self, query: &SpendQuery) -> Result<SpendPage, GatewayError> {
        query.validate().map_err(GatewayError::InvalidRequest)?;
        let s = lock(&self.state);
        let rows: Vec<&SpendRecord> = s
            .spend
            .iter()
            .filter(|r| query.key_alias.is_none() || r.key_alias == query.key_alias)
            .collect();
        let size = query.page_size as usize;
        let total_pages = rows.len().div_ceil(size) as u32;
        let records = rows
            .into_iter()
            .skip((query.page as usize - 1) * size)
            .take(size)
            .cloned()
            .collect();
        Ok(SpendPage {
            records,
            page: query.page,
            total_pages,
        })
    }
}

// ---------------------------------------------------------------- queue

struct QueueRow {
    task: QueuedTask,
    run_after: Instant,
    locked_until: Option<Instant>,
}

/// In-memory `TaskQueue` with the shared semantics of `ports::task_queue`.
/// [`InMemoryTaskQueue::expire_leases`] simulates every lease running out.
#[derive(Default)]
pub struct InMemoryTaskQueue {
    rows: Mutex<Vec<QueueRow>>,
}

impl InMemoryTaskQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, id: TaskId) -> Option<QueuedTask> {
        lock(&self.rows)
            .iter()
            .find(|r| r.task.id == id)
            .map(|r| r.task.clone())
    }

    /// Makes a queued task claimable now (skips its `run_after` delay).
    pub fn make_due(&self, id: TaskId) {
        for r in lock(&self.rows).iter_mut().filter(|r| r.task.id == id) {
            r.run_after = Instant::now();
        }
    }

    pub fn expire_leases(&self) {
        let past = Instant::now();
        for r in lock(&self.rows).iter_mut() {
            if r.locked_until.is_some() {
                r.locked_until = Some(past);
            }
        }
    }

    fn live_lease(r: &QueueRow, now: Instant) -> bool {
        r.task.state == TaskState::Running && r.locked_until.is_some_and(|t| t > now)
    }

    fn holder<'a>(
        rows: &'a mut [QueueRow],
        id: TaskId,
        worker: &str,
    ) -> Result<&'a mut QueueRow, StoreError> {
        rows.iter_mut()
            .find(|r| {
                r.task.id == id
                    && r.task.state == TaskState::Running
                    && r.task.locked_by.as_deref() == Some(worker)
            })
            .ok_or_else(|| StoreError::Invalid(format!("task {} is not leased by {worker}", id.0)))
    }
}

fn overlaps(a: &[String], b: &[String]) -> bool {
    a.iter().any(|f| b.contains(f))
}

#[async_trait::async_trait]
impl TaskQueue for InMemoryTaskQueue {
    async fn enqueue(&self, task: &NewTask) -> Result<TaskEnqueued, StoreError> {
        task.validate()?;
        let mut rows = lock(&self.rows);
        if let Some(r) = rows.iter().find(|r| {
            r.task.state.is_active()
                && r.task.repo == task.repo
                && r.task.ticket == task.ticket
                && r.task.role == task.role
        }) {
            return Ok(TaskEnqueued {
                id: r.task.id,
                created: false,
            });
        }
        let id = TaskId(rows.len() as i64 + 1);
        rows.push(QueueRow {
            task: QueuedTask {
                id,
                repo: task.repo.clone(),
                ticket: task.ticket.clone(),
                role: task.role,
                priority: task.priority,
                predicted_files: task.predicted_files.clone(),
                payload: task.payload.clone(),
                state: TaskState::Queued,
                attempts: 0,
                max_attempts: task.max_attempts,
                locked_by: None,
                last_error: None,
            },
            run_after: Instant::now(),
            locked_until: None,
        });
        Ok(TaskEnqueued { id, created: true })
    }

    async fn claim(
        &self,
        worker: &str,
        lease: Duration,
    ) -> Result<Option<ClaimedTask>, StoreError> {
        let now = Instant::now();
        let mut rows = lock(&self.rows);
        // Expired leases at the attempt limit end failed.
        for r in rows.iter_mut() {
            if r.task.state == TaskState::Running
                && !Self::live_lease(r, now)
                && r.task.attempts >= r.task.max_attempts
            {
                r.task.state = TaskState::Failed;
                r.task.locked_by = None;
                r.locked_until = None;
                r.task.last_error =
                    Some(format!("lease expired after {} attempts", r.task.attempts));
            }
        }
        let running: Vec<(TaskId, Vec<String>)> = rows
            .iter()
            .filter(|r| Self::live_lease(r, now))
            .map(|r| (r.task.id, r.task.predicted_files.clone()))
            .collect();
        let best = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| match r.task.state {
                TaskState::Queued => r.run_after <= now,
                TaskState::Running => !Self::live_lease(r, now),
                _ => false,
            })
            .filter(|(_, r)| {
                !running
                    .iter()
                    .any(|(id, files)| *id != r.task.id && overlaps(files, &r.task.predicted_files))
            })
            .min_by(|(_, a), (_, b)| {
                b.task
                    .priority
                    .cmp(&a.task.priority)
                    .then(a.run_after.cmp(&b.run_after))
                    .then(a.task.id.cmp(&b.task.id))
            })
            .map(|(i, _)| i);
        let Some(i) = best else { return Ok(None) };
        let r = &mut rows[i];
        r.task.state = TaskState::Running;
        r.task.attempts += 1;
        r.task.locked_by = Some(worker.to_string());
        r.locked_until = Some(now + lease);
        Ok(Some(ClaimedTask {
            task: r.task.clone(),
            lease,
        }))
    }

    async fn conflicts(&self, files: &[String]) -> Result<Vec<TaskId>, StoreError> {
        let now = Instant::now();
        Ok(lock(&self.rows)
            .iter()
            .filter(|r| Self::live_lease(r, now) && overlaps(&r.task.predicted_files, files))
            .map(|r| r.task.id)
            .collect())
    }

    async fn heartbeat(&self, id: TaskId, worker: &str, lease: Duration) -> Result<(), StoreError> {
        let mut rows = lock(&self.rows);
        Self::holder(&mut rows, id, worker)?.locked_until = Some(Instant::now() + lease);
        Ok(())
    }

    async fn complete(&self, id: TaskId, worker: &str) -> Result<(), StoreError> {
        let mut rows = lock(&self.rows);
        let r = Self::holder(&mut rows, id, worker)?;
        r.task.state = TaskState::Succeeded;
        r.task.locked_by = None;
        r.locked_until = None;
        Ok(())
    }

    async fn fail(
        &self,
        id: TaskId,
        worker: &str,
        error: &str,
        retry_after: Option<Duration>,
    ) -> Result<(), StoreError> {
        let mut rows = lock(&self.rows);
        let r = Self::holder(&mut rows, id, worker)?;
        r.task.last_error = Some(error.to_string());
        r.task.locked_by = None;
        r.locked_until = None;
        match retry_after {
            Some(d) if r.task.attempts < r.task.max_attempts => {
                r.task.state = TaskState::Queued;
                r.run_after = Instant::now() + d;
            }
            _ => r.task.state = TaskState::Failed,
        }
        Ok(())
    }

    async fn reschedule(
        &self,
        id: TaskId,
        worker: &str,
        reason: &str,
        after: Duration,
    ) -> Result<(), StoreError> {
        let mut rows = lock(&self.rows);
        let r = Self::holder(&mut rows, id, worker)?;
        r.task.last_error = Some(reason.to_string());
        r.task.locked_by = None;
        r.locked_until = None;
        r.task.attempts = r.task.attempts.saturating_sub(1);
        r.task.state = TaskState::Queued;
        r.run_after = Instant::now() + after;
        Ok(())
    }
}

// ---------------------------------------------------------------- memory

#[derive(Default)]
struct FactsState {
    facts: Vec<MemoryFact>,
    candidates: Vec<(PendingCandidate, Option<bool>)>,
}

/// In-memory `MemoryFactsWriter` (+ reader). `recorded_at`/`created_at` are
/// the fixed epoch. Hand [`InMemoryMemoryFacts::reader`] to anything that
/// must not write.
#[derive(Default)]
pub struct InMemoryMemoryFacts {
    state: Mutex<FactsState>,
}

impl InMemoryMemoryFacts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reader(self: &Arc<Self>) -> Arc<dyn MemoryFactsReader> {
        read_only(self.clone())
    }

    fn insert(s: &mut FactsState, fact: &NewFact, source: FactSource) -> FactId {
        if let Some(same) = s.facts.iter().find(|f| {
            f.status != FactStatus::Invalidated
                && f.repo == fact.repo
                && f.scope == fact.scope
                && f.scope_key == fact.scope_key
                && f.topic_key == fact.topic_key
                && f.body == fact.body
                && f.commit_sha == fact.commit_sha
        }) {
            return same.id;
        }
        let id = FactId(s.facts.len() as i64 + 1);
        let mut supersedes = None;
        for old in s.facts.iter_mut().filter(|f| {
            f.status != FactStatus::Invalidated
                && f.repo == fact.repo
                && f.scope == fact.scope
                && f.scope_key == fact.scope_key
                && f.topic_key == fact.topic_key
        }) {
            old.status = FactStatus::Invalidated;
            old.invalid_at = Some(fact.valid_from.clone());
            old.review_reason = Some(format!("superseded by {}", id.0));
            supersedes = Some(old.id);
        }
        s.facts.push(MemoryFact {
            id,
            repo: fact.repo.clone(),
            scope: fact.scope,
            scope_key: fact.scope_key.clone(),
            topic_key: fact.topic_key.clone(),
            body: fact.body.clone(),
            commit_sha: fact.commit_sha.clone(),
            run_id: fact.run_id.clone(),
            evidence_uri: fact.evidence_uri.clone(),
            source,
            valid_from: fact.valid_from.clone(),
            invalid_at: None,
            recorded_at: EPOCH.into(),
            review_after: fact.review_after.clone(),
            status: FactStatus::Active,
            review_reason: None,
            supersedes,
            referenced_paths: fact.referenced_paths.clone(),
        });
        id
    }

    fn fact_mut(s: &mut FactsState, id: FactId) -> Result<&mut MemoryFact, StoreError> {
        s.facts
            .iter_mut()
            .find(|f| f.id == id)
            .ok_or_else(|| StoreError::Invalid(format!("fact {} not found", id.0)))
    }

    fn pending(
        s: &mut FactsState,
        id: CandidateId,
    ) -> Result<&mut (PendingCandidate, Option<bool>), StoreError> {
        s.candidates
            .iter_mut()
            .find(|(c, decided)| c.id == id && decided.is_none())
            .ok_or_else(|| StoreError::Invalid(format!("candidate {} not pending", id.0)))
    }
}

#[async_trait::async_trait]
impl MemoryFactsReader for InMemoryMemoryFacts {
    async fn list_facts(&self, query: &FactsQuery) -> Result<FactsPage, StoreError> {
        query.validate()?;
        let after = match &query.cursor {
            None => 0,
            Some(c) => c
                .parse::<i64>()
                .map_err(|_| StoreError::Invalid(format!("bad cursor {c:?}")))?,
        };
        let statuses = query.effective_statuses();
        let s = lock(&self.state);
        let mut matched: Vec<MemoryFact> = s
            .facts
            .iter()
            .filter(|f| f.id.0 > after && f.repo == query.repo)
            .filter(|f| statuses.contains(&f.status))
            .filter(|f| query.scope.is_none_or(|sc| sc == f.scope))
            .filter(|f| query.scope_key.is_none() || query.scope_key == f.scope_key)
            .filter(|f| {
                query
                    .topic_prefix
                    .as_deref()
                    .is_none_or(|p| f.topic_key.starts_with(p))
            })
            .filter(|f| {
                query.as_of.as_deref().is_none_or(|t| {
                    f.valid_from.as_str() <= t && f.invalid_at.as_deref().is_none_or(|end| t < end)
                })
            })
            .take(query.limit as usize + 1)
            .cloned()
            .collect();
        let next_cursor = (matched.len() > query.limit as usize).then(|| {
            matched.truncate(query.limit as usize);
            matched
                .last()
                .map(|f| f.id.0.to_string())
                .unwrap_or_default()
        });
        Ok(FactsPage {
            facts: matched,
            next_cursor,
        })
    }

    async fn get_fact(&self, id: FactId) -> Result<Option<MemoryFact>, StoreError> {
        Ok(lock(&self.state).facts.iter().find(|f| f.id == id).cloned())
    }

    async fn topic_history(
        &self,
        repo: &str,
        topic_key: &str,
    ) -> Result<Vec<MemoryFact>, StoreError> {
        Ok(lock(&self.state)
            .facts
            .iter()
            .filter(|f| f.repo == repo && f.topic_key == topic_key)
            .cloned()
            .collect())
    }
}

#[async_trait::async_trait]
impl MemoryFactsWriter for InMemoryMemoryFacts {
    async fn record_fact(&self, fact: &NewFact) -> Result<FactId, StoreError> {
        fact.validate().map_err(StoreError::Invalid)?;
        Ok(Self::insert(&mut lock(&self.state), fact, fact.source))
    }

    async fn mark_needs_review(&self, id: FactId, reason: &str) -> Result<(), StoreError> {
        let mut s = lock(&self.state);
        let f = Self::fact_mut(&mut s, id)?;
        match f.status {
            FactStatus::Active => {
                f.status = FactStatus::NeedsReview;
                f.review_reason = Some(reason.to_string());
                Ok(())
            }
            FactStatus::NeedsReview => Ok(()),
            FactStatus::Invalidated => {
                Err(StoreError::Invalid(format!("fact {} is invalidated", id.0)))
            }
        }
    }

    async fn confirm(&self, id: FactId, review_after: Option<&str>) -> Result<(), StoreError> {
        let mut s = lock(&self.state);
        let f = Self::fact_mut(&mut s, id)?;
        if f.status == FactStatus::Invalidated {
            return Err(StoreError::Invalid(format!("fact {} is invalidated", id.0)));
        }
        f.status = FactStatus::Active;
        f.review_reason = None;
        if let Some(t) = review_after {
            f.review_after = Some(t.to_string());
        }
        Ok(())
    }

    async fn invalidate(
        &self,
        id: FactId,
        invalid_at: &str,
        reason: &str,
    ) -> Result<(), StoreError> {
        let mut s = lock(&self.state);
        let f = Self::fact_mut(&mut s, id)?;
        f.status = FactStatus::Invalidated;
        f.invalid_at = Some(invalid_at.to_string());
        f.review_reason = Some(reason.to_string());
        Ok(())
    }

    async fn enqueue_candidate(
        &self,
        candidate: &FactCandidate,
    ) -> Result<CandidateId, StoreError> {
        candidate.fact.validate().map_err(StoreError::Invalid)?;
        let mut s = lock(&self.state);
        let id = CandidateId(s.candidates.len() as i64 + 1);
        s.candidates.push((
            PendingCandidate {
                id,
                candidate: candidate.clone(),
                created_at: EPOCH.into(),
            },
            None,
        ));
        Ok(id)
    }

    async fn list_candidates(&self, repo: &str) -> Result<Vec<PendingCandidate>, StoreError> {
        Ok(lock(&self.state)
            .candidates
            .iter()
            .filter(|(c, decided)| decided.is_none() && c.candidate.fact.repo == repo)
            .map(|(c, _)| c.clone())
            .collect())
    }

    async fn approve_candidate(
        &self,
        id: CandidateId,
        _approver: &str,
    ) -> Result<FactId, StoreError> {
        let mut s = lock(&self.state);
        let entry = Self::pending(&mut s, id)?;
        entry.1 = Some(true);
        let fact = entry.0.candidate.fact.clone();
        Ok(Self::insert(&mut s, &fact, FactSource::ApprovedCandidate))
    }

    async fn reject_candidate(
        &self,
        id: CandidateId,
        _approver: &str,
        _reason: &str,
    ) -> Result<(), StoreError> {
        Self::pending(&mut lock(&self.state), id)?.1 = Some(false);
        Ok(())
    }
}
