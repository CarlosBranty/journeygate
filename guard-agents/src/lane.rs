//! The orchestrator side of one finished stage (`guard serve` workers and
//! `guard agents run`): what happens after the [`Worker`] ran a task.
//!
//! For every [`Ran`] with a report, [`Orchestrator::after_stage`]:
//!
//! 1. writes one `agent_runs` row per attempt ([`RunLog`]);
//! 2. ingests the gateway spend logs of every attempt's key alias into
//!    `spend_records` (today's UTC window, idempotent by `request_id`);
//! 3. puts agent `memory_proposals` into the human-approval candidate queue
//!    (`memory_writers::enqueue_proposals`), never into facts;
//! 4. records the orchestrator's own observations as memory facts through
//!    the [`MemoryFactsWriter`] it alone holds: the sealed spec failed red
//!    on the base commit (`ticket/<key>/sealed-spec`), and the agent branch
//!    passed the guard gate with that spec (`ticket/<key>/agent-pr`).
//!
//! Agents never reach any of this: the pipeline holds a reader only.
//! Every agent PR still needs human approval; nothing here merges.

use crate::pipeline::seams::TicketInput;
use crate::pipeline::{AttemptOutcome, StageOutcome, StageReport, sealed_key};
use crate::runs::{RunContext, RunLog, now_unix, record_stage, rfc3339, utc_day_window};
use crate::worker::{Ran, Tick, Worker};
use guard_core::ports::{
    FactScope, FactSource, MemoryFactsWriter, ModelGateway, NewFact, Role, StoreError,
};
use guard_store::memory_writers::{ProposalContext, enqueue_proposals, observe};
use guard_store::spend::{SpendSink, SpendWindow, ingest_spend};
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// What [`Orchestrator::after_stage`] did, for logs and `guard agents run`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AfterStage {
    pub runs_recorded: usize,
    pub spend_records: u64,
    pub candidates_enqueued: usize,
    pub facts_recorded: Vec<i64>,
    /// Non-fatal problems (spend ingestion, a rejected proposal, ...).
    pub warnings: Vec<String>,
}

pub struct Orchestrator {
    pub runs: Arc<dyn RunLog>,
    /// Only the orchestrator holds a writer.
    pub facts: Option<Arc<dyn MemoryFactsWriter>>,
    pub gateway: Arc<dyn ModelGateway>,
    pub spend: Option<Arc<dyn SpendSink>>,
    /// `s3://<bucket>` (or `file://...`) prefix of evidence URIs.
    pub evidence_base: Option<String>,
}

fn topic_key(ticket: &str, leaf: &str) -> String {
    let t: String = ticket
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("ticket/{t}/{leaf}")
}

impl Orchestrator {
    /// Post-processing of one worker tick result. Infrastructure errors of
    /// the run log abort (the caller logs them); everything else is a warning.
    pub async fn after_stage(&self, ran: &Ran) -> Result<AfterStage, StoreError> {
        let mut out = AfterStage::default();
        let (Some(report), Some(input)) = (&ran.report, &ran.input) else {
            return Ok(out);
        };
        let ctx = RunContext {
            run_id: input.run_id.clone(),
            task_id: Some(ran.task),
            repo: input.repo.clone(),
            ticket: input.ticket.clone(),
            base_commit: input.base_commit.clone(),
        };
        out.runs_recorded = record_stage(self.runs.as_ref(), &ctx, report).await?;
        self.ingest(report, &mut out).await;
        if let Some(writer) = &self.facts {
            self.proposals(writer.as_ref(), input, report, &mut out)
                .await;
            if let Err(e) = self
                .observations(writer.as_ref(), input, report, &mut out)
                .await
            {
                out.warnings.push(format!("memory facts: {e}"));
            }
        }
        Ok(out)
    }

    async fn ingest(&self, report: &StageReport, out: &mut AfterStage) {
        let aliases: BTreeSet<&str> = report
            .attempts
            .iter()
            .map(|a| a.key_alias.as_str())
            .collect();
        for alias in aliases {
            self.ingest_alias(alias, out).await;
        }
    }

    /// Ingests today's (UTC) gateway spend of one key alias.
    pub async fn ingest_alias(&self, alias: &str, out: &mut AfterStage) {
        let Some(sink) = &self.spend else { return };
        let (start, end) = utc_day_window(now_unix());
        let window = SpendWindow {
            key_alias: Some(alias.to_string()),
            start,
            end,
            page_size: 100,
        };
        match ingest_spend(self.gateway.as_ref(), sink.as_ref(), &window).await {
            Ok(i) => {
                out.spend_records += i.records;
                for (id, why) in i.rejected {
                    out.warnings.push(format!("spend row {id} rejected: {why}"));
                }
            }
            Err(e) => out
                .warnings
                .push(format!("spend ingestion of {alias}: {e}")),
        }
    }

    async fn proposals(
        &self,
        writer: &dyn MemoryFactsWriter,
        input: &TicketInput,
        report: &StageReport,
        out: &mut AfterStage,
    ) {
        let now = rfc3339(now_unix());
        for p in &report.memory_proposals {
            let ctx = ProposalContext {
                repo: input.repo.clone(),
                scope: FactScope::Repo,
                scope_key: None,
                commit_sha: input.base_commit.clone(),
                run_id: Some(p.run_id.clone()),
                valid_from: now.clone(),
                proposed_by: p.proposed_by.clone(),
            };
            match enqueue_proposals(writer, &ctx, std::slice::from_ref(&p.proposal)).await {
                Ok(o) => {
                    out.candidates_enqueued += o.enqueued.len();
                    for (topic, why) in o.rejected {
                        out.warnings
                            .push(format!("proposal {topic} rejected: {why}"));
                    }
                }
                Err(e) => out.warnings.push(format!("proposal queue: {e}")),
            }
        }
    }

    async fn observations(
        &self,
        writer: &dyn MemoryFactsWriter,
        input: &TicketInput,
        report: &StageReport,
        out: &mut AfterStage,
    ) -> Result<(), StoreError> {
        let now = rfc3339(now_unix());
        let evidence = |key: &str| self.evidence_base.as_ref().map(|b| format!("{b}/{key}"));
        let fact = match (&report.role, &report.outcome) {
            (Role::Qa, StageOutcome::Sealed(sealed)) => NewFact {
                repo: input.repo.clone(),
                scope: FactScope::Repo,
                scope_key: None,
                topic_key: topic_key(&input.ticket, "sealed-spec"),
                body: format!(
                    "{}: the sealed spec {} (sha256 {}) fails with an assertion on base {} \
                     (red-first, guard run on a clean checkout); it is the acceptance test of the ticket.",
                    input.ticket,
                    sealed.path,
                    &sealed.sha256[..12],
                    short(&input.base_commit)
                ),
                commit_sha: input.base_commit.clone(),
                run_id: Some(input.run_id.clone()),
                evidence_uri: evidence(&sealed_key(&input.run_id, &input.ticket)),
                source: FactSource::GateResult,
                valid_from: now,
                review_after: None,
                referenced_paths: vec![sealed.path.clone()],
            },
            (Role::Coder, StageOutcome::Passed { pr, agent, .. }) => {
                let winner = report
                    .attempts
                    .iter()
                    .rev()
                    .find(|a| a.agent == *agent && a.outcome == Some(AttemptOutcome::Passed));
                let mut paths = winner.map(|a| a.diff_paths.clone()).unwrap_or_default();
                paths.sort();
                paths.dedup();
                NewFact {
                    repo: input.repo.clone(),
                    scope: FactScope::Repo,
                    scope_key: None,
                    topic_key: topic_key(&input.ticket, "agent-pr"),
                    body: format!(
                        "{}: agent branch agent/{} ({}) passed the guard gate with the sealed spec \
                         after {} coder attempt(s), touching {}; awaiting human approval.",
                        input.ticket,
                        input.ticket,
                        pr.url,
                        report.attempts.iter().filter(|a| a.agent == *agent).count(),
                        if paths.is_empty() {
                            "no files".into()
                        } else {
                            paths.join(", ")
                        }
                    ),
                    commit_sha: input.base_commit.clone(),
                    run_id: Some(input.run_id.clone()),
                    evidence_uri: None,
                    source: FactSource::GateResult,
                    valid_from: now,
                    review_after: None,
                    referenced_paths: paths,
                }
            }
            _ => return Ok(()),
        };
        let id = observe(writer, &fact).await?;
        out.facts_recorded.push(id.0);
        Ok(())
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// One step of a worker loop with post-processing.
#[derive(Debug, Clone, PartialEq)]
pub enum LoopStep {
    Idle(crate::worker::Idle),
    Ran(Box<Ran>, AfterStage),
    /// A queue or store error (Postgres blip, lost lease): [`run_loop`]
    /// backs off and keeps going.
    Error(String),
}

/// `tick` + `after_stage`. Queue errors are `Err`; run-log errors become a warning.
pub async fn step(worker: &Worker, orchestrator: &Orchestrator) -> Result<LoopStep, StoreError> {
    match worker.tick().await? {
        Tick::Idle(i) => Ok(LoopStep::Idle(i)),
        Tick::Ran(ran) => {
            let after = match orchestrator.after_stage(&ran).await {
                Ok(a) => a,
                Err(e) => AfterStage {
                    warnings: vec![format!("agent_runs: {e}")],
                    ..AfterStage::default()
                },
            };
            Ok(LoopStep::Ran(ran, after))
        }
    }
}

/// Longest back-off of [`run_loop`] after consecutive errors.
pub const MAX_ERROR_BACKOFF: Duration = Duration::from_secs(60);

/// Runs `step` until `stop` returns true for a result, sleeping `idle`
/// between idle ticks (`guard serve` passes a never-true `stop`). A step
/// error never ends the loop: it is reported as [`LoopStep::Error`] and the
/// loop sleeps `idle`, doubling per consecutive error up to
/// [`MAX_ERROR_BACKOFF`]. The lease of a task whose `complete`/`fail`
/// failed expires and the queue hands it out again.
pub async fn run_loop<F>(
    worker: &Worker,
    orchestrator: &Orchestrator,
    idle: Duration,
    mut on_step: F,
) -> Result<(), StoreError>
where
    F: FnMut(&LoopStep) -> bool,
{
    let mut errors: u32 = 0;
    loop {
        let s = match step(worker, orchestrator).await {
            Ok(s) => {
                errors = 0;
                s
            }
            Err(e) => {
                errors = errors.saturating_add(1);
                LoopStep::Error(e.to_string())
            }
        };
        let stop = on_step(&s);
        if stop {
            return Ok(());
        }
        match s {
            LoopStep::Idle(_) => tokio::time::sleep(idle).await,
            LoopStep::Error(_) => {
                let factor = 1u32 << errors.min(16).saturating_sub(1);
                tokio::time::sleep(idle.saturating_mul(factor).min(MAX_ERROR_BACKOFF)).await;
            }
            LoopStep::Ran(..) => {}
        }
    }
}
