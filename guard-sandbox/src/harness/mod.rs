//! The lane-A QA/Coder harness (docs/contracts/qa-coder-protocol.md).
//!
//! [`Harness::run_ticket`] is orchestrator code end to end; agents only edit
//! files inside their sandbox:
//!
//! 1. **QA attempts** (up to `qa_policy`): fresh sandbox, `AllowedPaths::qa`.
//!    Diff check → AST quality checks ([`quality`]) → red-first on a clean
//!    checkout of the base commit ([`red_first`]). The first spec that fails
//!    there by an assertion is **sealed**: SHA-256 of its exact bytes, bytes
//!    stored in the `EvidenceStore` at `sealed/<run_id>/<KEY>.spec.ts`.
//! 2. **Coder attempts** (up to `coder_policy`, at most 3): the sealed copy is
//!    read back and verified before every attempt, staged on the host and
//!    bind-mounted read-only; tests are hidden. Diff check
//!    (`AllowedPaths::coder`, [`diff`]) → finalize: the sealed copy is read
//!    and verified **again**, then the [`SpecRunner`] applies the diff on a
//!    clean checkout, overlays the sealed bytes and runs the gate. Failing
//!    test output becomes the next attempt's `failure_context`.
//! 3. A broken seal aborts the ticket ([`Verdict::Aborted`]); exhausted
//!    attempts, budget or wall clock go to the human lane.
//!
//! Every attempt mints its own virtual key and revokes it, and every sandbox
//! and host workspace is released, on success and on error alike.
//! Memory is read through `MemoryFactsReader` only: the harness cannot write
//! facts (agent `memory_proposals` are left for the orchestrator's candidate
//! queue, memory.md).

pub mod diff;
pub mod quality;
pub mod red_first;

use guard_core::ports::agent::TASK_SCHEMA_VERSION;
use guard_core::ports::memory_facts::FACTS_BRIEF_MAX_BYTES;
use guard_core::ports::sandbox::WORKSPACE_DIR;
use guard_core::ports::{
    AgentOutcome, AgentRuntime, AgentTask, AllowedPaths, AttemptPolicy, EgressAllowlist,
    EgressPurpose, EgressRule, EvidenceError, EvidenceStore, ExtractedDiff, FactsBrief, FactsQuery,
    GatewayBinding, GatewayError, KeyRequest, MemoryFactsReader, ModelGateway, Role, RuntimeError,
    SandboxError, SandboxLayout, SandboxProvider, SandboxSpec, SealedSpec, StoreError, TaskStatus,
};
use quality::SpecQuality;
use red_first::{RedFirst, SpecRun};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum HarnessError {
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("workspace: {0}")]
    Workspace(String),
    #[error("spec runner: {0}")]
    Runner(String),
    #[error("quality checker: {0}")]
    Quality(String),
    #[error("invalid harness input: {0}")]
    Invalid(String),
}

/// Host-side worktrees and task directories (orchestrator storage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspace {
    /// Standalone clone at the base commit, branch `agent/<KEY>`.
    pub checkout: String,
    /// Empty directory mounted at `/task`.
    pub task_dir: String,
    pub codegraph_index: Option<String>,
    pub skills_dir: Option<String>,
}

#[async_trait::async_trait]
pub trait HostWorkspace: Send + Sync {
    /// A fresh checkout of `base_commit` for one (role, attempt).
    async fn prepare(
        &self,
        ticket: &str,
        role: Role,
        attempt: u32,
        base_commit: &str,
    ) -> Result<PreparedWorkspace, HarnessError>;

    /// Writes the (already verified) sealed bytes to a host file the Coder
    /// sandbox can bind read-only, and returns its host path.
    async fn stage_sealed(
        &self,
        ticket: &str,
        attempt: u32,
        sealed: &SealedSpec,
        bytes: &[u8],
    ) -> Result<String, HarnessError>;

    async fn release(&self, prepared: &PreparedWorkspace) -> Result<(), HarnessError>;
}

/// Spec runs on clean checkouts, never on an agent's workspace.
#[async_trait::async_trait]
pub trait SpecRunner: Send + Sync {
    /// Red-first: only `spec_path` (holding `spec_bytes`) on `base_commit`.
    async fn run_on_base(
        &self,
        base_commit: &str,
        spec_path: &str,
        spec_bytes: &[u8],
    ) -> Result<SpecRun, HarnessError>;

    /// Finalize: `git apply --index` of `diff` on `base_commit`, then the
    /// sealed bytes written over `sealed.path`, then the gate selection
    /// (affected journeys + the sealed spec + smoke).
    async fn run_gate(
        &self,
        base_commit: &str,
        diff: &ExtractedDiff,
        sealed: &SealedSpec,
        sealed_bytes: &[u8],
    ) -> Result<SpecRun, HarnessError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct HarnessConfig {
    /// `owner/name`.
    pub repo: String,
    /// `[playwright].tests_dir`.
    pub tests_dir: String,
    /// Globs the Coder may write (`[agents]` / `target.toml` areas).
    pub app_paths: Vec<String>,
    /// `<repo>@sha256:<digest>`.
    pub image: String,
    pub qa_model: String,
    pub coder_model: String,
    /// Per attempt (the virtual key's `max_budget`).
    pub budget_usd: f64,
    /// Per attempt.
    pub timeout_secs: u64,
    pub gateway: GatewayBinding,
    pub gateway_host: String,
    pub gateway_port: u16,
    /// QA only.
    pub preview: Option<(String, u16)>,
    /// Coder only.
    pub package_mirror: Option<(String, u16)>,
    pub qa_policy: AttemptPolicy,
    pub coder_policy: AttemptPolicy,
    pub tools: Vec<String>,
    pub failure_context_max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketInput {
    pub run_id: String,
    pub ticket: String,
    pub base_commit: String,
    pub summary: String,
    pub acceptance_criteria: Vec<String>,
}

/// `agent_runs.outcome` (qa-coder-protocol.md "Outcomes").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// One `agent_runs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptRecord {
    pub role: Role,
    pub attempt: u32,
    pub sandbox_id: Option<String>,
    pub runtime: Option<String>,
    pub outcome: AttemptOutcome,
    /// `result.json` status, when there was a valid result.
    pub status: Option<TaskStatus>,
    pub detail: String,
    pub cost_usd: f64,
    pub changed_paths: Vec<String>,
    /// Quality check ids that failed.
    pub quality: Vec<String>,
    pub sealed_spec_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The gate passed: push `agent/<KEY>` and open the PR (human approval).
    Passed,
    /// Attempts, budget or time exhausted: the full report goes to a human.
    HumanLane { reason: String },
    /// An integrity failure (sealed spec hash mismatch): stop everything.
    Aborted { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TicketReport {
    pub run_id: String,
    pub ticket: String,
    pub verdict: Verdict,
    pub attempts: Vec<AttemptRecord>,
    pub sealed: Option<SealedSpec>,
    /// The Coder diff that passed the gate.
    pub diff: Option<ExtractedDiff>,
}

/// Everything the harness drives. Fields are public so the orchestrator (and
/// tests) can wire any adapters.
pub struct Harness {
    pub config: HarnessConfig,
    pub runtime: Arc<dyn AgentRuntime>,
    pub sandbox: Arc<dyn SandboxProvider>,
    pub gateway: Arc<dyn ModelGateway>,
    pub evidence: Arc<dyn EvidenceStore>,
    /// Read-only by type: the harness cannot record facts.
    pub facts: Arc<dyn MemoryFactsReader>,
    pub workspace: Arc<dyn HostWorkspace>,
    pub runner: Arc<dyn SpecRunner>,
    pub quality: Arc<dyn SpecQuality>,
}

/// What one agent attempt produced (before any check).
struct AgentAttempt {
    sandbox_id: String,
    outcome: Result<AgentOutcome, RuntimeError>,
    diff: Option<ExtractedDiff>,
    spec_bytes: Option<Vec<u8>>,
}

enum Exhausted {
    Budget,
    Time,
}

fn spec_path(tests_dir: &str, ticket: &str) -> String {
    format!("{}/features/{ticket}.spec.ts", tests_dir.trim_matches('/'))
}

pub fn sealed_key(run_id: &str, ticket: &str) -> String {
    format!("sealed/{run_id}/{ticket}.spec.ts")
}

impl Harness {
    pub async fn run_ticket(&self, input: &TicketInput) -> Result<TicketReport, HarnessError> {
        let path = spec_path(&self.config.tests_dir, &input.ticket);
        let brief = self.facts_brief().await;
        let mut report = TicketReport {
            run_id: input.run_id.clone(),
            ticket: input.ticket.clone(),
            verdict: Verdict::HumanLane {
                reason: String::new(),
            },
            attempts: vec![],
            sealed: None,
            diff: None,
        };

        // ------------------------------------------------ QA
        let mut feedback: Option<String> = None;
        let mut sealed: Option<SealedSpec> = None;
        for attempt in 1..=self.config.qa_policy.max_attempts() {
            let allowed = AllowedPaths::qa(&self.config.tests_dir, &input.ticket);
            let run = self
                .run_agent(
                    input,
                    Role::Qa,
                    attempt,
                    allowed.clone(),
                    None,
                    &brief,
                    feedback.take(),
                )
                .await?;
            let mut rec = record(Role::Qa, attempt, &run);
            let outcome = match &run.outcome {
                Err(e) => Err((AttemptOutcome::RuntimeError, e.to_string())),
                Ok(o) => match exhausted(o) {
                    Some(x) => {
                        rec.outcome = AttemptOutcome::RuntimeError;
                        rec.detail = format!("{:?}: {}", o.result.status, o.result.summary);
                        report.attempts.push(rec);
                        report.verdict = human_lane(Role::Qa, x);
                        return Ok(report);
                    }
                    None => self.check_qa(input, &path, &run, &mut rec).await?,
                },
            };
            match outcome {
                Ok(seal) => {
                    rec.outcome = AttemptOutcome::Passed;
                    rec.sealed_spec_sha256 = Some(seal.sha256.clone());
                    report.attempts.push(rec);
                    sealed = Some(seal);
                    break;
                }
                Err((o, detail)) => {
                    rec.outcome = o;
                    rec.detail = detail.clone();
                    report.attempts.push(rec);
                    feedback = Some(detail);
                }
            }
        }
        let Some(sealed) = sealed else {
            report.verdict = Verdict::HumanLane {
                reason: format!(
                    "qa: no red-first spec after {} attempt(s)",
                    self.config.qa_policy.max_attempts()
                ),
            };
            return Ok(report);
        };
        report.sealed = Some(sealed.clone());
        let key = sealed_key(&input.run_id, &input.ticket);

        // ------------------------------------------------ Coder
        let mut feedback: Option<String> = None;
        let max = self.config.coder_policy.max_attempts();
        for attempt in 1..=max {
            let bytes = match self.verified_sealed(&key, &sealed).await? {
                Ok(b) => b,
                Err(reason) => {
                    report.verdict = Verdict::Aborted { reason };
                    return Ok(report);
                }
            };
            let host = self
                .workspace
                .stage_sealed(&input.ticket, attempt, &sealed, &bytes)
                .await?;
            let allowed =
                AllowedPaths::coder(&self.config.app_paths, &self.config.tests_dir, &sealed);
            let run = self
                .run_agent(
                    input,
                    Role::Coder,
                    attempt,
                    allowed.clone(),
                    Some((sealed.clone(), host)),
                    &brief,
                    feedback.take(),
                )
                .await?;
            let mut rec = record(Role::Coder, attempt, &run);
            let o = match &run.outcome {
                Err(e) => {
                    rec.outcome = AttemptOutcome::RuntimeError;
                    rec.detail = e.to_string();
                    feedback = Some(format!("The previous attempt failed to run: {e}"));
                    report.attempts.push(rec);
                    continue;
                }
                Ok(o) => o,
            };
            if let Some(x) = exhausted(o) {
                rec.outcome = AttemptOutcome::RuntimeError;
                rec.detail = format!("{:?}: {}", o.result.status, o.result.summary);
                report.attempts.push(rec);
                report.verdict = human_lane(Role::Coder, x);
                return Ok(report);
            }
            let diff = run.diff.clone().expect("a diff for a completed run");
            if let Err(rej) = diff::check_diff(&allowed, &diff, &[]) {
                rec.outcome = AttemptOutcome::DiffRejected;
                rec.detail = rej.feedback();
                feedback = Some(rej.feedback());
                report.attempts.push(rec);
                continue;
            }
            // Finalize: the sealed copy from orchestrator storage, re-verified.
            let bytes = match self.verified_sealed(&key, &sealed).await? {
                Ok(b) => b,
                Err(reason) => {
                    rec.outcome = AttemptOutcome::GateFailed;
                    rec.detail = reason.clone();
                    report.attempts.push(rec);
                    report.verdict = Verdict::Aborted { reason };
                    return Ok(report);
                }
            };
            let gate = self
                .runner
                .run_gate(&input.base_commit, &diff, &sealed, &bytes)
                .await?;
            if gate.gate_passed() {
                rec.outcome = AttemptOutcome::Passed;
                rec.detail = format!("{} test(s) passed", gate.passed);
                report.attempts.push(rec);
                report.diff = Some(diff);
                report.verdict = Verdict::Passed;
                return Ok(report);
            }
            let ctx = gate.failure_context(self.config.failure_context_max_bytes);
            rec.outcome = AttemptOutcome::GateFailed;
            rec.detail = ctx.clone();
            report.attempts.push(rec);
            feedback = Some(ctx);
        }
        report.verdict = Verdict::HumanLane {
            reason: format!("coder: gate not green after {max} attempt(s)"),
        };
        Ok(report)
    }

    async fn facts_brief(&self) -> Option<FactsBrief> {
        let page = self
            .facts
            .list_facts(&FactsQuery::for_repo(&self.config.repo))
            .await
            .ok()?;
        let brief = FactsBrief::render(&page.facts, FACTS_BRIEF_MAX_BYTES);
        (!brief.fact_ids.is_empty()).then_some(brief)
    }

    /// `Ok(Ok(bytes))` when the stored copy matches the seal, `Ok(Err(reason))` when not.
    async fn verified_sealed(
        &self,
        key: &str,
        sealed: &SealedSpec,
    ) -> Result<Result<Vec<u8>, String>, HarnessError> {
        let bytes = self.evidence.get_object(key).await?;
        if sealed.verify(&bytes) {
            Ok(Ok(bytes))
        } else {
            Ok(Err(format!(
                "sealed spec {} hash mismatch: stored copy is not {}",
                sealed.path, sealed.sha256
            )))
        }
    }

    /// QA checks after a completed run; `Ok(sealed)` when the spec is sealed.
    async fn check_qa(
        &self,
        input: &TicketInput,
        path: &str,
        run: &AgentAttempt,
        rec: &mut AttemptRecord,
    ) -> Result<Result<SealedSpec, (AttemptOutcome, String)>, HarnessError> {
        let allowed = AllowedPaths::qa(&self.config.tests_dir, &input.ticket);
        let diff = run.diff.as_ref().expect("a diff for a completed run");
        if let Err(rej) = diff::check_diff(&allowed, diff, &[path.to_string()]) {
            return Ok(Err((AttemptOutcome::DiffRejected, rej.feedback())));
        }
        let Some(bytes) = run.spec_bytes.as_ref() else {
            return Ok(Err((
                AttemptOutcome::DiffRejected,
                format!("{path} was not found in the sandbox"),
            )));
        };
        let violations = self.quality.check(&input.ticket, path, bytes).await?;
        if !violations.is_empty() {
            rec.quality = violations.iter().map(|v| v.id.clone()).collect();
            rec.quality.dedup();
            let detail = format!(
                "The spec failed the test-quality checks:\n{}",
                violations
                    .iter()
                    .map(|v| format!("- {} (line {}): {}", v.id, v.line, v.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            return Ok(Err((AttemptOutcome::QualityRejected, detail)));
        }
        let base = self
            .runner
            .run_on_base(&input.base_commit, path, bytes)
            .await?;
        match base.red_first() {
            RedFirst::Red => {
                let seal = SealedSpec::seal(path, bytes);
                self.evidence
                    .put_object(
                        &sealed_key(&input.run_id, &input.ticket),
                        bytes.clone(),
                        "text/plain; charset=utf-8",
                    )
                    .await?;
                Ok(Ok(seal))
            }
            RedFirst::Passed => Ok(Err((
                AttemptOutcome::RedFirstPassed,
                format!(
                    "The spec passes on the base commit {}, so it proves nothing new. \
                     It must fail there with an assertion failure.",
                    input.base_commit
                ),
            ))),
            RedFirst::NotAnAssertionFailure(why) => Ok(Err((
                AttemptOutcome::QualityRejected,
                format!(
                    "red-first: the spec must fail on the base commit by an assertion, but {why}"
                ),
            ))),
        }
    }

    fn egress(&self, role: Role) -> EgressAllowlist {
        let mut e =
            EgressAllowlist::gateway_only(&self.config.gateway_host, self.config.gateway_port);
        let extra = match role {
            Role::Qa => self
                .config
                .preview
                .clone()
                .map(|p| (p, EgressPurpose::Preview)),
            Role::Coder => self
                .config
                .package_mirror
                .clone()
                .map(|p| (p, EgressPurpose::PackageMirror)),
        };
        if let Some(((host, port), purpose)) = extra {
            e = e.with(EgressRule {
                host,
                port,
                purpose,
            });
        }
        e
    }

    /// Prepare → mint key → create sandbox → run → diff (and the QA spec) →
    /// destroy → revoke → release. Cleanup runs whatever happened.
    #[allow(clippy::too_many_arguments)]
    async fn run_agent(
        &self,
        input: &TicketInput,
        role: Role,
        attempt: u32,
        allowed: AllowedPaths,
        sealed: Option<(SealedSpec, String)>,
        brief: &Option<FactsBrief>,
        failure_context: Option<String>,
    ) -> Result<AgentAttempt, HarnessError> {
        let c = &self.config;
        let model = match role {
            Role::Qa => c.qa_model.clone(),
            Role::Coder => c.coder_model.clone(),
        };
        let policy = match role {
            Role::Qa => c.qa_policy,
            Role::Coder => c.coder_policy,
        };
        let task = AgentTask {
            version: TASK_SCHEMA_VERSION,
            run_id: input.run_id.clone(),
            ticket: input.ticket.clone(),
            role,
            attempt,
            attempt_policy: policy,
            repo: c.repo.clone(),
            base_commit: input.base_commit.clone(),
            summary: input.summary.clone(),
            acceptance_criteria: input.acceptance_criteria.clone(),
            allowed_paths: allowed,
            sealed_spec: sealed.as_ref().map(|(s, _)| s.clone()),
            model: model.clone(),
            budget_usd: c.budget_usd,
            timeout_secs: c.timeout_secs,
            gateway: c.gateway.clone(),
            tools: c.tools.clone(),
            facts_brief: brief.clone(),
            failure_context,
        };
        task.validate().map_err(HarnessError::Invalid)?;

        let prepared = self
            .workspace
            .prepare(&input.ticket, role, attempt, &input.base_commit)
            .await?;
        let metadata: BTreeMap<String, String> = [
            ("run_id", input.run_id.as_str()),
            ("ticket", input.ticket.as_str()),
            ("role", role.as_str()),
            ("repo", c.repo.as_str()),
            ("commit", input.base_commit.as_str()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let key = match self
            .gateway
            .mint_key(&KeyRequest {
                alias: format!("{}:{}:{attempt}", input.run_id, role.as_str()),
                models: vec![model],
                max_budget_usd: c.budget_usd,
                rpm_limit: None,
                tpm_limit: None,
                max_parallel_requests: None,
                duration: Duration::from_secs(c.timeout_secs + 600),
                metadata,
            })
            .await
        {
            Ok(k) => k,
            Err(e) => {
                let _ = self.workspace.release(&prepared).await;
                return Err(e.into());
            }
        };

        let layout = SandboxLayout {
            run_id: input.run_id.clone(),
            ticket: input.ticket.clone(),
            attempt,
            image: c.image.clone(),
            checkout: prepared.checkout.clone(),
            task_dir: prepared.task_dir.clone(),
            codegraph_index: prepared.codegraph_index.clone(),
            sealed_spec: sealed
                .as_ref()
                .map(|(s, host)| (host.clone(), s.path.clone())),
            tests_dir: c.tests_dir.clone(),
            skills_dir: prepared.skills_dir.clone(),
        };
        let mut spec = SandboxSpec::for_role(role, layout, self.egress(role));
        spec.secrets
            .insert(c.gateway.key_env.clone(), key.token.clone());
        spec.limits.timeout = Duration::from_secs(c.timeout_secs + 300);

        let result = async {
            let sb = self.sandbox.create(&spec).await?;
            let inner = async {
                let outcome = self.runtime.run(&sb, &task).await;
                let mut diff = None;
                let mut spec_bytes = None;
                if outcome.is_ok() {
                    diff = Some(self.sandbox.extract_diff(&sb, &input.base_commit).await?);
                    if role == Role::Qa {
                        let p =
                            format!("{WORKSPACE_DIR}/{}", spec_path(&c.tests_dir, &input.ticket));
                        spec_bytes = match self.sandbox.read_file(&sb, &p).await {
                            Ok(b) => Some(b),
                            Err(SandboxError::NotFound(_)) => None,
                            Err(e) => return Err(HarnessError::from(e)),
                        };
                    }
                }
                Ok(AgentAttempt {
                    sandbox_id: sb.id.0.clone(),
                    outcome,
                    diff,
                    spec_bytes,
                })
            }
            .await;
            let destroyed = self.sandbox.destroy(&sb).await;
            let attempt = inner?;
            destroyed?;
            Ok::<AgentAttempt, HarnessError>(attempt)
        }
        .await;
        let revoked = self.gateway.revoke(&key.key_id).await;
        let released = self.workspace.release(&prepared).await;
        let attempt = result?;
        revoked?;
        released?;
        Ok(attempt)
    }
}

fn record(role: Role, attempt: u32, run: &AgentAttempt) -> AttemptRecord {
    let ok = run.outcome.as_ref().ok();
    AttemptRecord {
        role,
        attempt,
        sandbox_id: Some(run.sandbox_id.clone()),
        runtime: ok.map(|o| o.runtime.clone()),
        outcome: AttemptOutcome::RuntimeError,
        status: ok.map(|o| o.result.status),
        detail: String::new(),
        cost_usd: ok.map_or(0.0, |o| o.result.cost_usd),
        changed_paths: run
            .diff
            .as_ref()
            .map(|d| d.changed_paths.clone())
            .unwrap_or_default(),
        quality: vec![],
        sealed_spec_sha256: None,
    }
}

fn exhausted(o: &AgentOutcome) -> Option<Exhausted> {
    match o.result.status {
        TaskStatus::BudgetExceeded => Some(Exhausted::Budget),
        TaskStatus::TimedOut => Some(Exhausted::Time),
        _ => None,
    }
}

fn human_lane(role: Role, x: Exhausted) -> Verdict {
    let what = match x {
        Exhausted::Budget => "virtual key budget exhausted",
        Exhausted::Time => "timeout exhausted",
    };
    Verdict::HumanLane {
        reason: format!("{}: {what}", role.as_str()),
    }
}
