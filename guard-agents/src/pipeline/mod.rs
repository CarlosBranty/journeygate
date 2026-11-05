//! One lane-A ticket: QA attempt(s) → diff check → test quality → red-first
//! → seal → Coder attempt loop → gate → PR branch `agent/<KEY>`
//! (docs/contracts/qa-coder-protocol.md), only through ports
//! (`AgentRuntime`, `SandboxProvider`, `ModelGateway`, `EvidenceStore`,
//! `MemoryFactsReader`) and the orchestrator [`seams`].
//!
//! Every attempt gets a fresh sandbox and its own virtual key; both are torn
//! down whatever happens. The sealed spec lives only in orchestrator storage
//! (`sealed/<run_id>/<KEY>.spec.ts`) and is verified before every use.
//! Agents never write memory: the pipeline holds a reader only and returns
//! `memory_proposals` in the report for the orchestrator to enqueue.

pub mod seams;

use crate::config::{AgentConfig, AgentsConfig};
use crate::scheduler::{SchedulerError, Slots, TaskSlot};
use futures::future::join_all;
use guard_core::ports::{
    AgentOutcome, AgentRuntime, AgentTask, AllowedPaths, EgressAllowlist, EgressPurpose,
    EgressRule, EvidenceError, EvidenceStore, ExtractedDiff, FactsBrief, GatewayBinding,
    GatewayError, KeyRequest, MemoryFactsReader, MemoryProposal, ModelGateway, Role, RuntimeError,
    Sandbox, SandboxError, SandboxProvider, SandboxSpec, SealedSpec, Secret, StoreError,
    TaskStatus,
};
use guard_store::memory_brief::{BriefScope, build_facts_brief};
use guard_store::spend::{SpendSink, SpendWindow, ingest_spend};
use seams::{
    LayoutProvider, LayoutRequest, PrPublisher, PrRequest, PublishedPr, RedFirst, SeamError,
    TicketChecks, TicketInput,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Fixed tool sets, in a stable order (part of the cached prompt prefix, tokens.md).
pub const QA_TOOLS: &[&str] = &["guard_history", "guard_map", "playwright"];
pub const CODER_TOOLS: &[&str] = &["guard_test", "guard_failure", "guard_history", "guard_map"];
/// Copilot-hosted models (`via_gateway = false`). Host not yet verified
/// against a live Copilot CLI (agents-toml.md marks the exception explicit).
pub const COPILOT_EXCEPTION_HOST: &str = "api.githubcopilot.com";
/// Infrastructure retry delay of a failed task.
pub const INFRA_RETRY: Duration = Duration::from_secs(60);

/// `EvidenceStore` key of the sealed spec of a run.
pub fn sealed_key(run_id: &str, ticket: &str) -> String {
    format!("sealed/{run_id}/{ticket}.spec.ts")
}

/// Added plus removed lines of a unified diff (headers excluded).
pub fn changed_lines(unified_diff: &str) -> usize {
    unified_diff
        .lines()
        .filter(|l| {
            (l.starts_with('+') && !l.starts_with("+++"))
                || (l.starts_with('-') && !l.starts_with("---"))
        })
        .count()
}

// ---------------------------------------------------------------- errors

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PipelineError {
    /// Misconfiguration: never retried.
    #[error("pipeline misconfigured: {0}")]
    Config(String),
    #[error(transparent)]
    Scheduler(SchedulerError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{seam} failed: {error}")]
    Seam {
        seam: &'static str,
        error: SeamError,
    },
}

impl From<SchedulerError> for PipelineError {
    fn from(e: SchedulerError) -> Self {
        PipelineError::Scheduler(e)
    }
}

impl PipelineError {
    /// When the queue should retry the task: never for a misconfiguration,
    /// at the next UTC day for the daily cap, after [`INFRA_RETRY`] otherwise.
    pub fn retry_after(&self, now_unix_secs: u64) -> Option<Duration> {
        match self {
            PipelineError::Config(_) => None,
            PipelineError::Scheduler(SchedulerError::DailyCapReached { .. }) => {
                Some(crate::scheduler::until_next_utc_day(now_unix_secs))
            }
            _ => Some(INFRA_RETRY),
        }
    }
}

fn seam(seam: &'static str) -> impl FnOnce(SeamError) -> PipelineError {
    move |error| PipelineError::Seam { seam, error }
}

// ---------------------------------------------------------------- report

/// `agent_runs.outcome` (qa-coder-protocol.md "Outcomes").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Passed,
    GateFailed,
    RedFirstPassed,
    DiffRejected,
    QualityRejected,
    RuntimeError,
}

impl AttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptOutcome::Passed => "passed",
            AttemptOutcome::GateFailed => "gate_failed",
            AttemptOutcome::RedFirstPassed => "red_first_passed",
            AttemptOutcome::DiffRejected => "diff_rejected",
            AttemptOutcome::QualityRejected => "quality_rejected",
            AttemptOutcome::RuntimeError => "runtime_error",
        }
    }
}

/// One sandboxed attempt, shaped like an `agent_runs` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttemptRecord {
    pub role: Role,
    pub attempt: u32,
    pub agent: String,
    pub runtime: String,
    pub model: String,
    pub key_alias: String,
    pub sandbox_id: Option<String>,
    /// `result.json` status; `None` when the runtime returned an error.
    pub status: Option<TaskStatus>,
    /// `None` when the agent itself gave up (`failed` status).
    pub outcome: Option<AttemptOutcome>,
    pub cost_usd: f64,
    pub diff_paths: Vec<String>,
    pub changed_lines: usize,
    pub sealed_spec_sha256: Option<String>,
    pub detail: Option<String>,
    /// A/B arm (`agent_runs.arm`).
    pub arm: Option<String>,
    /// `result.json` `steps`.
    pub steps: Option<u32>,
    /// `guard_history` tool calls in `events.jsonl`.
    pub memory_reads: u32,
    /// Facts of the brief the attempt was given (`agent_runs.facts_brief_ids`).
    pub facts_brief_ids: Vec<i64>,
    pub usage: Option<guard_core::ports::Usage>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HumanLaneReason {
    AttemptsExhausted {
        role: Role,
        last_outcome: Option<AttemptOutcome>,
    },
    BudgetExceeded {
        agent: String,
    },
    TimedOut {
        agent: String,
    },
    Refused {
        agent: String,
    },
    /// The stored sealed spec no longer matches its digest.
    SealTampered {
        path: String,
    },
    DiffScanner {
        findings: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StageOutcome {
    /// QA: red-first held and the spec is sealed.
    Sealed(SealedSpec),
    /// Coder: green and published.
    Passed {
        pr: PublishedPr,
        agent: String,
        changed_lines: usize,
    },
    HumanLane(HumanLaneReason),
}

/// A memory fact an agent suggested, for the orchestrator's candidate queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProposedFact {
    /// `<runtime>:<role>` (`memory_fact_candidates.proposed_by`).
    pub proposed_by: String,
    pub run_id: String,
    pub proposal: MemoryProposal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StageReport {
    pub role: Role,
    pub outcome: StageOutcome,
    pub attempts: Vec<AttemptRecord>,
    pub memory_proposals: Vec<ProposedFact>,
}

impl StageReport {
    pub fn cost_usd(&self) -> f64 {
        self.attempts.iter().map(|a| a.cost_usd).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TicketReport {
    pub qa: StageReport,
    /// `None` when QA did not seal a spec.
    pub coder: Option<StageReport>,
}

impl TicketReport {
    pub fn total_cost_usd(&self) -> f64 {
        self.qa.cost_usd() + self.coder.as_ref().map_or(0.0, StageReport::cost_usd)
    }
}

// ---------------------------------------------------------------- pipeline

pub struct PipelineDeps {
    pub config: Arc<AgentsConfig>,
    pub slots: Arc<Slots>,
    /// Adapters by `AgentRuntime::name()`; every configured runtime needs one.
    pub runtimes: Vec<Arc<dyn AgentRuntime>>,
    pub sandbox: Arc<dyn SandboxProvider>,
    pub gateway: Arc<dyn ModelGateway>,
    pub evidence: Arc<dyn EvidenceStore>,
    pub layouts: Arc<dyn LayoutProvider>,
    pub checks: Arc<dyn TicketChecks>,
    pub publisher: Arc<dyn PrPublisher>,
    /// Read-only memory for the facts brief (`InMemoryMemoryFacts::reader`,
    /// `read_only(..)`): the pipeline cannot write facts.
    pub facts: Option<Arc<dyn MemoryFactsReader>>,
}

pub struct TicketPipeline {
    config: Arc<AgentsConfig>,
    slots: Arc<Slots>,
    runtimes: BTreeMap<String, Arc<dyn AgentRuntime>>,
    sandbox: Arc<dyn SandboxProvider>,
    gateway: Arc<dyn ModelGateway>,
    evidence: Arc<dyn EvidenceStore>,
    layouts: Arc<dyn LayoutProvider>,
    checks: Arc<dyn TicketChecks>,
    publisher: Arc<dyn PrPublisher>,
    facts: Option<Arc<dyn MemoryFactsReader>>,
    minted: Mutex<Vec<KeyRequest>>,
    secret_aliases: SecretAliases,
    spend: Option<Arc<dyn SpendSink>>,
}

/// Extra env var names that must carry the virtual key for a runtime
/// (`guard_runtimes::secret_env_aliases`), given the runtime name and task.
pub type SecretAliases = Arc<dyn Fn(&str, &AgentTask) -> Vec<String> + Send + Sync>;

struct AttemptSpec<'a> {
    input: &'a TicketInput,
    role: Role,
    agent: &'a str,
    attempt: u32,
    sealed: Option<(&'a SealedSpec, &'a [u8])>,
    failure_context: Option<String>,
    brief: Option<FactsBrief>,
    /// Best-of-N: `:<agent>` keeps key aliases unique per candidate.
    alias_suffix: bool,
}

struct AttemptRun {
    record: AttemptRecord,
    outcome: Result<AgentOutcome, RuntimeError>,
    diff: Option<ExtractedDiff>,
    spec_bytes: Option<Vec<u8>>,
}

/// What a judged attempt means for the loop.
enum Step {
    Retry(String),
    Stop(HumanLaneReason),
}

enum CandidateEnd {
    Green {
        diff: ExtractedDiff,
        changed_lines: usize,
        seq: u64,
        summary: String,
    },
    Human(HumanLaneReason),
    Exhausted(Option<AttemptOutcome>),
}

struct Candidate {
    agent: String,
    attempts: Vec<AttemptRecord>,
    proposals: Vec<ProposedFact>,
    end: CandidateEnd,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl TicketPipeline {
    pub fn new(deps: PipelineDeps) -> Result<Self, PipelineError> {
        let runtimes: BTreeMap<String, Arc<dyn AgentRuntime>> = deps
            .runtimes
            .into_iter()
            .map(|r| (r.name().to_string(), r))
            .collect();
        for (name, agent) in &deps.config.agents {
            if !runtimes.contains_key(agent.runtime.as_str()) {
                return Err(PipelineError::Config(format!(
                    "agents.{name}: no adapter for runtime {:?}",
                    agent.runtime.as_str()
                )));
            }
        }
        if deps.config.gateway_endpoint().is_none() {
            return Err(PipelineError::Config("gateway.url has no host".into()));
        }
        Ok(Self {
            config: deps.config,
            slots: deps.slots,
            runtimes,
            sandbox: deps.sandbox,
            gateway: deps.gateway,
            evidence: deps.evidence,
            layouts: deps.layouts,
            checks: deps.checks,
            publisher: deps.publisher,
            facts: deps.facts,
            minted: Mutex::new(Vec::new()),
            secret_aliases: Arc::new(|_, _| Vec::new()),
            spend: None,
        })
    }

    /// Ingests the gateway spend of every attempt's key right after the
    /// attempt (tokens.md "Spend ingestion"), so the daily USD cap checked
    /// before the next attempt sees it, even when the stage later fails.
    pub fn with_spend_sink(mut self, sink: Arc<dyn SpendSink>) -> Self {
        self.spend = Some(sink);
        self
    }

    /// Runtimes that read the key from a fixed variable get it there too
    /// (wire `guard_runtimes::secret_env_aliases`).
    pub fn with_secret_aliases(mut self, aliases: SecretAliases) -> Self {
        self.secret_aliases = aliases;
        self
    }

    /// Every key request minted so far (audit trail; tokens never kept).
    pub fn minted_key_requests(&self) -> Vec<KeyRequest> {
        lock(&self.minted).clone()
    }

    /// QA then, when sealed, the Coder stage.
    pub async fn run_ticket(
        &self,
        input: &TicketInput,
        slot: &TaskSlot,
    ) -> Result<TicketReport, PipelineError> {
        let qa = self.run_qa(input, slot).await?;
        let coder = match &qa.outcome {
            StageOutcome::Sealed(sealed) => Some(self.run_coder(input, sealed, slot).await?),
            _ => None,
        };
        Ok(TicketReport { qa, coder })
    }

    // ------------------------------------------------------------ QA

    /// The QA stage of `(ticket, qa)`. `_slot` is the host slot the caller holds.
    pub async fn run_qa(
        &self,
        input: &TicketInput,
        _slot: &TaskSlot,
    ) -> Result<StageReport, PipelineError> {
        let (name, agent) = self.config.role_agent(Role::Qa);
        let allowed = AllowedPaths::qa(&input.tests_dir, &input.ticket);
        let spec_path = allowed.write[0].clone();
        let brief = self.facts_brief(input).await?;
        let mut attempts = Vec::new();
        let mut proposals = Vec::new();
        let mut context = None;
        for attempt in 1..=agent.attempt_policy().max_attempts() {
            let mut run = self
                .run_attempt(AttemptSpec {
                    input,
                    role: Role::Qa,
                    agent: name,
                    attempt,
                    sealed: None,
                    failure_context: context.take(),
                    brief: brief.clone(),
                    alias_suffix: false,
                })
                .await?;
            proposals.extend(self.proposals_of(input, &run));
            let step = self.judge_qa(input, &allowed, &spec_path, &mut run).await;
            attempts.push(run.record);
            let outcome = match step? {
                Ok(sealed) => StageOutcome::Sealed(sealed),
                Err(Step::Retry(ctx)) => {
                    context = Some(ctx);
                    continue;
                }
                Err(Step::Stop(reason)) => StageOutcome::HumanLane(reason),
            };
            return Ok(StageReport {
                role: Role::Qa,
                outcome,
                attempts,
                memory_proposals: proposals,
            });
        }
        let last_outcome = attempts.last().and_then(|a| a.outcome);
        Ok(StageReport {
            role: Role::Qa,
            outcome: StageOutcome::HumanLane(HumanLaneReason::AttemptsExhausted {
                role: Role::Qa,
                last_outcome,
            }),
            attempts,
            memory_proposals: proposals,
        })
    }

    /// `Ok(Ok(sealed))`, `Ok(Err(step))`, or an infrastructure error.
    async fn judge_qa(
        &self,
        input: &TicketInput,
        allowed: &AllowedPaths,
        spec_path: &str,
        run: &mut AttemptRun,
    ) -> Result<Result<SealedSpec, Step>, PipelineError> {
        if let Err(step) = judge_status(run) {
            return Ok(Err(step));
        }
        let changed = match run.diff.as_ref().map(applied_paths).transpose() {
            Ok(c) => c.unwrap_or_default(),
            Err(msg) => {
                return Ok(Err(reject(
                    &mut run.record,
                    AttemptOutcome::DiffRejected,
                    msg,
                )));
            }
        };
        let violations = allowed
            .violations(&changed)
            .map_err(PipelineError::Config)?;
        let r = &mut run.record;
        if !violations.is_empty() {
            let msg = format!(
                "the diff may only touch {spec_path}; rejected: {}",
                violations.join(", ")
            );
            return Ok(Err(reject(r, AttemptOutcome::DiffRejected, msg)));
        }
        let Some(bytes) = run
            .spec_bytes
            .clone()
            .filter(|_| changed.iter().any(|p| p == spec_path))
        else {
            let msg = format!("QA did not write {spec_path}");
            return Ok(Err(reject(r, AttemptOutcome::DiffRejected, msg)));
        };
        let quality = self
            .checks
            .test_quality(input, spec_path, &bytes)
            .await
            .map_err(seam("test quality"))?;
        if !quality.failed.is_empty() {
            let msg = format!(
                "deterministic test-quality checks failed: {}",
                quality.failed.join(", ")
            );
            return Ok(Err(reject(r, AttemptOutcome::QualityRejected, msg)));
        }
        match self
            .checks
            .red_first(input, spec_path, &bytes)
            .await
            .map_err(seam("red-first"))?
        {
            RedFirst::Failed => {}
            RedFirst::Passed => {
                let msg = format!(
                    "{spec_path} passed on the base commit; it must fail first with an assertion"
                );
                return Ok(Err(reject(r, AttemptOutcome::RedFirstPassed, msg)));
            }
            RedFirst::Broken(why) => {
                let msg = format!("red-first did not fail on an assertion: {why}");
                return Ok(Err(reject(r, AttemptOutcome::QualityRejected, msg)));
            }
        }
        let sealed = SealedSpec::seal(spec_path, &bytes);
        self.evidence
            .put_object(
                &sealed_key(&input.run_id, &input.ticket),
                bytes,
                "text/plain; charset=utf-8",
            )
            .await?;
        r.outcome = Some(AttemptOutcome::Passed);
        r.sealed_spec_sha256 = Some(sealed.sha256.clone());
        Ok(Ok(sealed))
    }

    // ------------------------------------------------------------ Coder

    /// The Coder stage of `(ticket, coder)`: one candidate, or every
    /// `[scheduler.best_of_n]` agent when the ticket carries its label.
    pub async fn run_coder(
        &self,
        input: &TicketInput,
        sealed: &SealedSpec,
        _slot: &TaskSlot,
    ) -> Result<StageReport, PipelineError> {
        let human = |reason| StageReport {
            role: Role::Coder,
            outcome: StageOutcome::HumanLane(reason),
            attempts: vec![],
            memory_proposals: vec![],
        };
        let Some(bytes) = self.load_sealed(input, sealed).await? else {
            return Ok(human(HumanLaneReason::SealTampered {
                path: sealed.path.clone(),
            }));
        };
        let agents: Vec<String> = match &self.config.scheduler.best_of_n {
            Some(b) if b.applies_to(&input.labels) => b.agents.clone(),
            _ => vec![self.config.roles.coder.clone()],
        };
        let brief = self.facts_brief(input).await?;
        let best_of_n = agents.len() > 1;
        let seq = AtomicU64::new(0);
        let results = join_all(agents.iter().enumerate().map(|(i, agent)| {
            let (brief, bytes, seq) = (brief.clone(), &bytes, &seq);
            async move {
                // The task's own slot covers the first candidate.
                let _extra = if i > 0 {
                    Some(self.slots.task_slot().await)
                } else {
                    None
                };
                self.run_candidate(input, agent, sealed, bytes, brief, best_of_n, seq)
                    .await
            }
        }))
        .await;

        let mut candidates = Vec::new();
        let mut first_error = None;
        for r in results {
            match r {
                Ok(c) => candidates.push(c),
                Err(e) => {
                    first_error.get_or_insert(e);
                }
            }
        }
        let winner = candidates
            .iter()
            .filter_map(|c| match &c.end {
                CandidateEnd::Green {
                    changed_lines, seq, ..
                } => Some((*changed_lines, *seq, c)),
                _ => None,
            })
            .min_by_key(|(lines, seq, _)| (*lines, *seq))
            .map(|(_, _, c)| c);
        if winner.is_none() {
            if let Some(e) = first_error {
                return Err(e);
            }
        }
        let attempts: Vec<AttemptRecord> = candidates
            .iter()
            .flat_map(|c| c.attempts.iter().cloned())
            .collect();
        let memory_proposals: Vec<ProposedFact> = candidates
            .iter()
            .flat_map(|c| c.proposals.iter().cloned())
            .collect();
        let outcome = match winner {
            Some(c) => {
                let CandidateEnd::Green {
                    diff,
                    changed_lines,
                    summary,
                    ..
                } = &c.end
                else {
                    unreachable!("winner is green")
                };
                // Verified again right before it leaves orchestrator storage.
                let Some(bytes) = self.load_sealed(input, sealed).await? else {
                    return Ok(human(HumanLaneReason::SealTampered {
                        path: sealed.path.clone(),
                    }));
                };
                let request =
                    self.pr_request(input, sealed, bytes, diff, &c.agent, summary, &attempts);
                let pr = self
                    .publisher
                    .publish(&request)
                    .await
                    .map_err(seam("pr publisher"))?;
                StageOutcome::Passed {
                    pr,
                    agent: c.agent.clone(),
                    changed_lines: *changed_lines,
                }
            }
            None => StageOutcome::HumanLane(
                candidates
                    .iter()
                    .find_map(|c| match &c.end {
                        CandidateEnd::Human(r) => Some(r.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| HumanLaneReason::AttemptsExhausted {
                        role: Role::Coder,
                        last_outcome: candidates.iter().find_map(|c| match c.end {
                            CandidateEnd::Exhausted(o) => o,
                            _ => None,
                        }),
                    }),
            ),
        };
        Ok(StageReport {
            role: Role::Coder,
            outcome,
            attempts,
            memory_proposals,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_candidate(
        &self,
        input: &TicketInput,
        agent_name: &str,
        sealed: &SealedSpec,
        bytes: &[u8],
        brief: Option<FactsBrief>,
        best_of_n: bool,
        seq: &AtomicU64,
    ) -> Result<Candidate, PipelineError> {
        let agent = &self.config.agents[agent_name];
        let allowed = AllowedPaths::coder(&input.app_paths, &input.tests_dir, sealed);
        let mut c = Candidate {
            agent: agent_name.to_string(),
            attempts: vec![],
            proposals: vec![],
            end: CandidateEnd::Exhausted(None),
        };
        let mut context = None;
        for attempt in 1..=agent.attempt_policy().max_attempts() {
            let mut run = self
                .run_attempt(AttemptSpec {
                    input,
                    role: Role::Coder,
                    agent: agent_name,
                    attempt,
                    sealed: Some((sealed, bytes)),
                    failure_context: context.take(),
                    brief: brief.clone(),
                    alias_suffix: best_of_n,
                })
                .await?;
            c.proposals.extend(self.proposals_of(input, &run));
            let step = self.judge_coder(input, &allowed, sealed, &mut run).await;
            let summary = run
                .outcome
                .as_ref()
                .map(|o| o.result.summary.clone())
                .unwrap_or_default();
            c.attempts.push(run.record);
            match step? {
                Ok(diff) => {
                    c.end = CandidateEnd::Green {
                        changed_lines: changed_lines(&diff.unified_diff),
                        diff,
                        seq: seq.fetch_add(1, Ordering::SeqCst),
                        summary,
                    };
                    return Ok(c);
                }
                Err(Step::Retry(ctx)) => context = Some(ctx),
                Err(Step::Stop(reason)) => {
                    c.end = CandidateEnd::Human(reason);
                    return Ok(c);
                }
            }
        }
        c.end = CandidateEnd::Exhausted(c.attempts.last().and_then(|a| a.outcome));
        Ok(c)
    }

    async fn judge_coder(
        &self,
        input: &TicketInput,
        allowed: &AllowedPaths,
        sealed: &SealedSpec,
        run: &mut AttemptRun,
    ) -> Result<Result<ExtractedDiff, Step>, PipelineError> {
        if let Err(step) = judge_status(run) {
            return Ok(Err(step));
        }
        let diff = run.diff.clone().unwrap_or_else(|| ExtractedDiff {
            base_commit: input.base_commit.clone(),
            unified_diff: String::new(),
            changed_paths: vec![],
        });
        let r = &mut run.record;
        let changed = match applied_paths(&diff) {
            Ok(c) => c,
            Err(msg) => return Ok(Err(reject(r, AttemptOutcome::DiffRejected, msg))),
        };
        let violations = allowed
            .violations(&changed)
            .map_err(PipelineError::Config)?;
        if !violations.is_empty() {
            let msg = format!(
                "the diff touched paths the Coder may not change: {}",
                violations.join(", ")
            );
            return Ok(Err(reject(r, AttemptOutcome::DiffRejected, msg)));
        }
        if changed.is_empty() {
            let msg = "the attempt changed no files".to_string();
            return Ok(Err(reject(r, AttemptOutcome::DiffRejected, msg)));
        }
        let findings = self
            .checks
            .scan_diff(input, &diff)
            .await
            .map_err(seam("diff scanner"))?;
        if !findings.is_empty() {
            r.detail = Some(format!("diff scanner: {}", findings.join("; ")));
            return Ok(Err(Step::Stop(HumanLaneReason::DiffScanner { findings })));
        }
        let Some(bytes) = self.load_sealed(input, sealed).await? else {
            r.detail = Some("sealed spec digest mismatch".into());
            return Ok(Err(Step::Stop(HumanLaneReason::SealTampered {
                path: sealed.path.clone(),
            })));
        };
        let verdict = self
            .checks
            .gate(input, &diff, sealed, &bytes)
            .await
            .map_err(seam("gate"))?;
        let r = &mut run.record;
        r.sealed_spec_sha256 = Some(sealed.sha256.clone());
        if verdict.passed {
            r.outcome = Some(AttemptOutcome::Passed);
            r.detail = Some(verdict.summary);
            Ok(Ok(diff))
        } else {
            let ctx = verdict
                .failure_context
                .unwrap_or_else(|| verdict.summary.clone());
            r.detail = Some(verdict.summary);
            r.outcome = Some(AttemptOutcome::GateFailed);
            Ok(Err(Step::Retry(ctx)))
        }
    }

    // ------------------------------------------------------------ attempt

    /// Permit → key → sandbox → runtime → diff; the sandbox is destroyed and
    /// the key revoked whatever happens after they exist.
    async fn run_attempt(&self, a: AttemptSpec<'_>) -> Result<AttemptRun, PipelineError> {
        let agent = &self.config.agents[a.agent];
        let runtime = self.runtimes[agent.runtime.as_str()].clone();
        let _permit = self.slots.attempt_permit(&agent.model).await?;
        let mut alias = match &a.input.experiment {
            Some(e) => e.alias.clone(),
            None => format!("{}:{}:{}", a.input.run_id, a.role.as_str(), a.attempt),
        };
        if a.input.claim > 1 {
            alias = format!("{alias}:c{}", a.input.claim);
        }
        if a.alias_suffix {
            alias = format!("{alias}:{}", a.agent);
        }
        let metadata: BTreeMap<String, String> = [
            ("run_id", a.input.run_id.as_str()),
            ("ticket", a.input.ticket.as_str()),
            ("role", a.role.as_str()),
            ("repo", a.input.repo.as_str()),
            ("commit", a.input.base_commit.as_str()),
            ("agent", a.agent),
        ]
        .into_iter()
        .chain(a.input.experiment.as_ref().map(|e| ("arm", e.arm.as_str())))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let request = self.config.key_request(a.agent, &alias, metadata);
        let started_unix = now_unix();
        let key = self.gateway.mint_key(&request).await?;
        lock(&self.minted).push(request);

        let mut record = AttemptRecord {
            role: a.role,
            attempt: a.attempt,
            agent: a.agent.to_string(),
            runtime: runtime.name().to_string(),
            model: agent.model.clone(),
            key_alias: alias,
            sandbox_id: None,
            status: None,
            outcome: None,
            cost_usd: 0.0,
            diff_paths: vec![],
            changed_lines: 0,
            sealed_spec_sha256: None,
            detail: None,
            arm: a.input.experiment.as_ref().map(|e| e.arm.clone()),
            steps: None,
            memory_reads: 0,
            facts_brief_ids: a
                .brief
                .as_ref()
                .map(|b| b.fact_ids.iter().map(|f| f.0).collect())
                .unwrap_or_default(),
            usage: None,
        };
        let result = self
            .in_sandbox(&a, agent, runtime.as_ref(), &key.token, &mut record)
            .await;
        let mut revoked = self.gateway.revoke(&key.key_id).await;
        if matches!(&revoked, Err(e) if !matches!(e, GatewayError::NotFound(_))) {
            revoked = self.gateway.revoke(&key.key_id).await;
        }
        let ingested = self
            .ingest_attempt_spend(&record.key_alias, started_unix)
            .await;
        let mut run = result?;
        if let Err(e) = revoked {
            append_detail(&mut run.record, format!("key revoke failed: {e}"));
        }
        if let Err(e) = ingested {
            append_detail(&mut run.record, format!("spend ingestion failed: {e}"));
        }
        Ok(run)
    }

    async fn in_sandbox(
        &self,
        a: &AttemptSpec<'_>,
        agent: &AgentConfig,
        runtime: &dyn AgentRuntime,
        token: &Secret,
        record: &mut AttemptRecord,
    ) -> Result<AttemptRun, PipelineError> {
        let layout = self
            .layouts
            .layout(&LayoutRequest {
                input: a.input,
                role: a.role,
                agent: a.agent,
                runtime: agent.runtime,
                attempt: a.attempt,
                image: self.config.image_for(agent),
                sealed: a.sealed,
            })
            .await
            .map_err(seam("layout"))?;
        let aliases = (self.secret_aliases)(runtime.name(), &self.agent_task(a, agent));
        let host_layout = layout.clone();
        let inner = self
            .in_layout(a, agent, runtime, token, layout, aliases, record)
            .await;
        let released = self.layouts.release(&host_layout).await;
        let (outcome, diff, spec_bytes) = inner?;
        if let Err(e) = released {
            append_detail(record, format!("layout release failed: {e}"));
        }
        if let Ok(o) = &outcome {
            record.status = Some(o.result.status);
            record.cost_usd = o.result.cost_usd;
            record.steps = Some(o.result.steps);
            record.usage = Some(o.result.usage);
            record.memory_reads = o
                .events
                .iter()
                .filter(|e| {
                    matches!(&e.event, guard_core::ports::RuntimeEventKind::ToolCall { tool, .. }
                        if is_tool(tool, "guard_history"))
                })
                .count() as u32;
        }
        if let Some(d) = &diff {
            record.diff_paths = d.changed_paths.clone();
            record.changed_lines = changed_lines(&d.unified_diff);
        }
        Ok(AttemptRun {
            record: record.clone(),
            outcome,
            diff,
            spec_bytes,
        })
    }

    /// Sandbox create → drive → destroy inside one prepared layout.
    #[allow(clippy::too_many_arguments, clippy::type_complexity)]
    async fn in_layout(
        &self,
        a: &AttemptSpec<'_>,
        agent: &AgentConfig,
        runtime: &dyn AgentRuntime,
        token: &Secret,
        layout: guard_core::ports::SandboxLayout,
        aliases: Vec<String>,
        record: &mut AttemptRecord,
    ) -> Result<
        (
            Result<AgentOutcome, RuntimeError>,
            Option<ExtractedDiff>,
            Option<Vec<u8>>,
        ),
        PipelineError,
    > {
        let mut spec = self.sandbox_spec(a, agent, layout, token)?;
        for name in aliases {
            spec.secrets.insert(name, token.clone());
        }
        let sandbox = self.sandbox.create(&spec).await?;
        record.sandbox_id = Some(sandbox.id.0.clone());
        let inner = self.drive(a, agent, runtime, &sandbox).await;
        let destroyed = self.sandbox.destroy(&sandbox).await;
        let out = inner?;
        if let Err(e) = destroyed {
            append_detail(record, format!("sandbox destroy failed: {e}"));
        }
        Ok(out)
    }

    #[allow(clippy::type_complexity)]
    async fn drive(
        &self,
        a: &AttemptSpec<'_>,
        agent: &AgentConfig,
        runtime: &dyn AgentRuntime,
        sandbox: &Sandbox,
    ) -> Result<
        (
            Result<AgentOutcome, RuntimeError>,
            Option<ExtractedDiff>,
            Option<Vec<u8>>,
        ),
        PipelineError,
    > {
        let task = self.agent_task(a, agent);
        let outcome = runtime.run(sandbox, &task).await;
        let diff = match self
            .sandbox
            .extract_diff(sandbox, &a.input.base_commit)
            .await
        {
            Ok(d) => Some(d),
            // Without a diff a completed run cannot be judged: retry the task.
            Err(e) if outcome.is_ok() => return Err(e.into()),
            Err(_) => None,
        };
        let mut spec_bytes = None;
        if a.role == Role::Qa {
            let path = format!(
                "/workspace/{}",
                AllowedPaths::qa(&a.input.tests_dir, &a.input.ticket).write[0]
            );
            spec_bytes = match self.sandbox.read_file(sandbox, &path).await {
                Ok(b) => Some(b),
                Err(SandboxError::NotFound(_)) => None,
                Err(e) if outcome.is_ok() => return Err(e.into()),
                Err(_) => None,
            };
        }
        Ok((outcome, diff, spec_bytes))
    }

    fn sandbox_spec(
        &self,
        a: &AttemptSpec<'_>,
        agent: &AgentConfig,
        layout: guard_core::ports::SandboxLayout,
        token: &Secret,
    ) -> Result<SandboxSpec, PipelineError> {
        let (host, port) = self
            .config
            .gateway_endpoint()
            .ok_or_else(|| PipelineError::Config("gateway.url has no host".into()))?;
        let mut egress = EgressAllowlist::gateway_only(&host, port);
        let extra = match a.role {
            Role::Qa => a
                .input
                .preview
                .as_ref()
                .map(|e| (e, EgressPurpose::Preview)),
            Role::Coder => a
                .input
                .package_mirror
                .as_ref()
                .map(|e| (e, EgressPurpose::PackageMirror)),
        };
        if let Some((e, purpose)) = extra {
            egress = egress.with(EgressRule {
                host: e.host.clone(),
                port: e.port,
                purpose,
            });
        }
        let mut spec = SandboxSpec::for_role(a.role, layout, egress);
        spec.secrets
            .insert(self.config.gateway.key_env.clone(), token.clone());
        if !agent.via_gateway {
            let env = agent.copilot_token_env.as_deref().unwrap_or_default();
            let value = std::env::var(env).map_err(|_| {
                PipelineError::Config(format!(
                    "agents.{}: copilot exception needs {env} in the orchestrator env",
                    a.agent
                ))
            })?;
            spec.copilot_exception = true;
            spec.egress = spec.egress.with(EgressRule {
                host: COPILOT_EXCEPTION_HOST.into(),
                port: 443,
                purpose: EgressPurpose::CopilotException,
            });
            spec.secrets.insert(env.to_string(), Secret::new(value));
        }
        spec.limits.timeout = agent.timeout();
        Ok(spec)
    }

    fn agent_task(&self, a: &AttemptSpec<'_>, agent: &AgentConfig) -> AgentTask {
        let (allowed, sealed_spec, tools) = match (a.role, a.sealed) {
            (Role::Coder, Some((sealed, _))) => (
                AllowedPaths::coder(&a.input.app_paths, &a.input.tests_dir, sealed),
                Some(sealed.clone()),
                CODER_TOOLS,
            ),
            _ => (
                AllowedPaths::qa(&a.input.tests_dir, &a.input.ticket),
                None,
                QA_TOOLS,
            ),
        };
        AgentTask {
            version: guard_core::ports::agent::TASK_SCHEMA_VERSION,
            run_id: a.input.run_id.clone(),
            ticket: a.input.ticket.clone(),
            role: a.role,
            attempt: a.attempt,
            attempt_policy: agent.attempt_policy(),
            repo: a.input.repo.clone(),
            base_commit: a.input.base_commit.clone(),
            summary: a.input.summary.clone(),
            acceptance_criteria: a.input.acceptance_criteria.clone(),
            allowed_paths: allowed,
            sealed_spec,
            model: agent.model.clone(),
            budget_usd: agent.budget_usd,
            timeout_secs: agent.timeout_secs,
            gateway: GatewayBinding {
                base_url: self.config.gateway.url.clone(),
                key_env: self.config.gateway.key_env.clone(),
                via_gateway: agent.via_gateway,
            },
            tools: tools
                .iter()
                .filter(|t| match &a.input.experiment {
                    None => true,
                    Some(e) => match **t {
                        "guard_history" => e.memory,
                        "guard_map" => e.map,
                        _ => true,
                    },
                })
                .map(|s| s.to_string())
                .collect(),
            facts_brief: a.brief.clone(),
            failure_context: a.failure_context.clone(),
        }
    }

    // ------------------------------------------------------------ helpers

    /// The stored sealed bytes when they still match the digest, else `None`.
    async fn load_sealed(
        &self,
        input: &TicketInput,
        sealed: &SealedSpec,
    ) -> Result<Option<Vec<u8>>, PipelineError> {
        match self
            .evidence
            .get_object(&sealed_key(&input.run_id, &input.ticket))
            .await
        {
            Ok(bytes) => Ok(sealed.verify(&bytes).then_some(bytes)),
            Err(EvidenceError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Built once per stage so it is byte-identical across attempts.
    /// `None` without a reader or in an A/B arm with memory off.
    async fn facts_brief(&self, input: &TicketInput) -> Result<Option<FactsBrief>, PipelineError> {
        let Some(reader) = &self.facts else {
            return Ok(None);
        };
        if input.experiment.as_ref().is_some_and(|e| !e.memory) {
            return Ok(None);
        }
        // Repo-scope facts plus the ticket's areas and journeys, every page
        // (memory.md "Facts brief").
        let scope = BriefScope {
            repo: input.repo.clone(),
            areas: input.areas.clone(),
            journeys: input.journeys.clone(),
        };
        let brief = build_facts_brief(reader.as_ref(), &scope).await?;
        Ok((!brief.fact_ids.is_empty()).then_some(brief))
    }

    /// Spend of one attempt's key, from its mint time (minus a minute of
    /// clock skew) to now, so a stage crossing UTC midnight loses nothing.
    async fn ingest_attempt_spend(&self, alias: &str, started_unix: u64) -> Result<(), String> {
        let Some(sink) = &self.spend else {
            return Ok(());
        };
        let window = SpendWindow {
            key_alias: Some(alias.to_string()),
            start: crate::runs::gateway_time(started_unix.saturating_sub(60)),
            end: crate::runs::gateway_time(now_unix() + 60),
            page_size: 100,
        };
        ingest_spend(self.gateway.as_ref(), sink.as_ref(), &window)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn proposals_of(&self, input: &TicketInput, run: &AttemptRun) -> Vec<ProposedFact> {
        let Ok(o) = &run.outcome else { return vec![] };
        o.result
            .memory_proposals
            .iter()
            .map(|p| ProposedFact {
                proposed_by: format!("{}:{}", o.runtime, run.record.role.as_str()),
                run_id: input.run_id.clone(),
                proposal: p.clone(),
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn pr_request(
        &self,
        input: &TicketInput,
        sealed: &SealedSpec,
        sealed_bytes: Vec<u8>,
        diff: &ExtractedDiff,
        agent: &str,
        summary: &str,
        attempts: &[AttemptRecord],
    ) -> PrRequest {
        let cost: f64 = attempts.iter().map(|a| a.cost_usd).sum();
        let a = &self.config.agents[agent];
        let body = format!(
            "Agent PR for {ticket} (lane A).\n\n\
             {summary}\n\n\
             - Coder: `{agent}` ({runtime}, `{model}`), {n} attempt(s), ${cost:.4} total\n\
             - Sealed spec: `{path}` sha256 `{sha}` (overlaid by the orchestrator)\n\
             - Base commit: `{base}`\n\n\
             Human approval required before merge.\n",
            ticket = input.ticket,
            summary = if summary.is_empty() {
                &input.summary
            } else {
                summary
            },
            runtime = a.runtime.as_str(),
            model = a.model,
            n = attempts.iter().filter(|r| r.agent == agent).count(),
            path = sealed.path,
            sha = sealed.sha256,
            base = input.base_commit,
        );
        PrRequest {
            repo: input.repo.clone(),
            ticket: input.ticket.clone(),
            branch: format!("agent/{}", input.ticket),
            base_commit: input.base_commit.clone(),
            diff: diff.unified_diff.clone(),
            sealed_spec: sealed.clone(),
            sealed_bytes,
            title: format!("{}: {}", input.ticket, input.summary),
            body,
            summary: if summary.trim().is_empty() {
                input.summary.clone()
            } else {
                summary.to_string()
            },
            attempts: u32::try_from(attempts.len()).unwrap_or(u32::MAX),
            cost_usd: cost,
            coder: Some(format!("{agent} ({}, {})", a.runtime.as_str(), a.model)),
        }
    }
}

/// Runtime errors and non-completed statuses.
fn judge_status(run: &mut AttemptRun) -> Result<(), Step> {
    let r = &mut run.record;
    let o = match &run.outcome {
        // Killed at `timeout_secs`: step 9 sends it to the human lane.
        Err(e @ RuntimeError::Timeout { .. }) => {
            r.outcome = Some(AttemptOutcome::RuntimeError);
            r.detail = Some(e.to_string());
            return Err(Step::Stop(HumanLaneReason::TimedOut {
                agent: r.agent.clone(),
            }));
        }
        Err(e) => {
            r.outcome = Some(AttemptOutcome::RuntimeError);
            r.detail = Some(e.to_string());
            return Err(Step::Retry(format!("the previous attempt failed: {e}")));
        }
        Ok(o) => o,
    };
    let agent = r.agent.clone();
    let why = o.result.error.clone().unwrap_or_default();
    match o.result.status {
        TaskStatus::Completed => Ok(()),
        TaskStatus::BudgetExceeded => {
            r.detail = Some(format!("budget exceeded {why}").trim().to_string());
            Err(Step::Stop(HumanLaneReason::BudgetExceeded { agent }))
        }
        TaskStatus::TimedOut => {
            r.detail = Some(format!("timed out {why}").trim().to_string());
            Err(Step::Stop(HumanLaneReason::TimedOut { agent }))
        }
        TaskStatus::Refused => {
            r.detail = Some(format!("refused {why}").trim().to_string());
            Err(Step::Stop(HumanLaneReason::Refused { agent }))
        }
        TaskStatus::Failed => {
            r.detail = Some(format!("agent gave up {why}").trim().to_string());
            Err(Step::Retry(format!("the previous attempt gave up: {why}")))
        }
    }
}

fn reject(r: &mut AttemptRecord, outcome: AttemptOutcome, msg: String) -> Step {
    r.outcome = Some(outcome);
    r.detail = Some(msg.clone());
    Step::Retry(msg)
}

fn append_detail(r: &mut AttemptRecord, msg: String) {
    r.detail = Some(match r.detail.take() {
        Some(d) => format!("{d}; {msg}"),
        None => msg,
    });
}

/// `tool` names the guard MCP tool `name`, bare (Codex) or with the MCP
/// client prefix (`mcp__guard__guard_history` in Claude Code).
pub fn is_tool(tool: &str, name: &str) -> bool {
    tool == name || tool.strip_suffix(name).is_some_and(|p| p.ends_with("__"))
}

/// The repo-relative paths a `git apply -p1` of `diff.unified_diff` would
/// touch, united with `diff.changed_paths`. The applied patch, not the name
/// list, decides what lands on the host, so every header must carry the
/// `a/` / `b/` prefixes `-p1` strips (a sandbox `diff.noprefix` or
/// `diff.mnemonicPrefix` would otherwise shift paths by one component), and
/// a patch path missing from `changed_paths` rejects the diff.
pub fn applied_paths(diff: &ExtractedDiff) -> Result<Vec<String>, String> {
    let in_patch = patch_paths(&diff.unified_diff)?;
    let listed: BTreeSet<String> = diff.changed_paths.iter().cloned().collect();
    let unlisted: Vec<&str> = in_patch.difference(&listed).map(String::as_str).collect();
    if !unlisted.is_empty() {
        return Err(format!(
            "the patch touches paths its name list omits: {}",
            unlisted.join(", ")
        ));
    }
    Ok(listed.union(&in_patch).cloned().collect())
}

/// Paths of the header lines of a unified diff (see [`applied_paths`]).
pub fn patch_paths(diff: &str) -> Result<BTreeSet<String>, String> {
    let mut paths = BTreeSet::new();
    // Lines before a file's first hunk are its headers; hunk lines such as a
    // removed `-- comment` (`--- comment`) are content.
    let mut header = true;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            header = true;
            if rest.starts_with('"') {
                return Err(format!("quoted diff header: {line}"));
            }
            let rest = rest
                .strip_prefix("a/")
                .ok_or_else(|| format!("diff header without a/ b/ prefixes: {line}"))?;
            // `a/<p> b/<p>`: with --no-renames both sides are the same path.
            let half = rest.len().saturating_sub(3) / 2;
            match (rest.get(..half), rest.get(half..)) {
                (Some(a), Some(b)) if !a.is_empty() && b.strip_prefix(" b/") == Some(a) => {
                    paths.insert(a.to_string());
                }
                _ => return Err(format!("ambiguous diff header: {line}")),
            }
            continue;
        }
        if line.starts_with("@@") || line.starts_with("GIT binary patch") {
            header = false;
            continue;
        }
        if !header {
            continue;
        }
        for (marker, prefix) in [("--- ", "a/"), ("+++ ", "b/")] {
            if let Some(p) = line.strip_prefix(marker) {
                let p = p.trim_end_matches('\t');
                if p == "/dev/null" {
                    continue;
                }
                if p.starts_with('"') {
                    return Err(format!("quoted diff header: {line}"));
                }
                let path = p
                    .strip_prefix(prefix)
                    .ok_or_else(|| format!("diff header without the {prefix} prefix: {line}"))?;
                paths.insert(path.to_string());
            }
        }
        for marker in ["rename from ", "rename to ", "copy from ", "copy to "] {
            if let Some(p) = line.strip_prefix(marker) {
                paths.insert(p.to_string());
            }
        }
    }
    Ok(paths)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
