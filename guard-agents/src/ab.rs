//! The 4-arm token A/B harness (docs/contracts/tokens.md, "4-arm A/B
//! protocol"): every (task, arm, repetition) runs the real
//! [`TicketPipeline`] once, with
//!
//! - one key alias `<experiment>:<arm>:<task>:<rep>` for all its attempts,
//! - the facts brief and `guard_history` only in `memory` / `both`,
//! - `guard_map` only in `map` / `both`,
//!
//! then its `agent_runs` rows are written and its gateway spend is ingested,
//! so `guard_store::spend_report::{load_ab_inputs, compute_ab_report}` can
//! compute the pre-registered report from the two tables alone.
//!
//! Arms run in a seeded random order inside each (task, rep) block. A trial
//! whose pipeline fails for infrastructure reasons is retried once and then
//! kept as an unresolved trial (its runs stay; it is never dropped).
//!
//! Real numbers need real runtimes and a real gateway (provider credentials);
//! the harness itself is runtime-agnostic and is tested only with fakes.

use crate::lane::Orchestrator;
use crate::pipeline::seams::{ExperimentArm, TicketInput};
use crate::pipeline::{PipelineError, StageOutcome, TicketPipeline};
use crate::runs::RunContext;
use crate::scheduler::Slots;
use guard_store::spend_report::Arm;
use serde::Serialize;

/// `(memory, map)` of an arm.
pub fn toggles(arm: Arm) -> (bool, bool) {
    match arm {
        Arm::Baseline => (false, false),
        Arm::Memory => (true, false),
        Arm::Map => (false, true),
        Arm::Both => (true, true),
    }
}

/// `<experiment>:<arm>:<task>:<rep>`.
pub fn alias(experiment: &str, arm: Arm, task: &str, rep: u32) -> String {
    format!("{experiment}:{}:{task}:r{rep}", arm.as_str())
}

fn run_id_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The input of one trial: `task` with the experiment arm and its own run id.
pub fn trial_input(task: &TicketInput, experiment: &str, arm: Arm, rep: u32) -> TicketInput {
    let (memory, map) = toggles(arm);
    let mut run_id = format!(
        "{}-{}-{}-r{rep}",
        run_id_part(experiment),
        arm.as_str(),
        run_id_part(&task.ticket)
    );
    run_id.truncate(64);
    TicketInput {
        run_id,
        experiment: Some(ExperimentArm {
            alias: alias(experiment, arm, &task.ticket, rep),
            arm: arm.as_str().to_string(),
            memory,
            map,
        }),
        ..task.clone()
    }
}

/// Arms of one block in a seeded order (xorshift; deterministic per seed).
pub fn block_order(seed: u64, block: u64) -> Vec<Arm> {
    let mut arms = Arm::ALL.to_vec();
    let mut x = seed ^ block.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    for i in (1..arms.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        arms.swap(i, (x % (i as u64 + 1)) as usize);
    }
    arms
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrialRun {
    pub alias: String,
    pub arm: Arm,
    pub task: String,
    pub rep: u32,
    pub resolved: bool,
    pub cost_usd: f64,
    pub spend_records: u64,
    /// The infrastructure error that remained after the retry, if any.
    pub error: Option<String>,
}

pub struct AbPlan {
    pub experiment: String,
    pub tasks: Vec<TicketInput>,
    /// `k` (tokens.md: 3).
    pub reps: u32,
    pub seed: u64,
}

/// Runs every trial of `plan`. Only run-log errors abort.
pub async fn run_experiment(
    pipeline: &TicketPipeline,
    slots: &Slots,
    orchestrator: &Orchestrator,
    plan: &AbPlan,
) -> Result<Vec<TrialRun>, guard_core::ports::StoreError> {
    let mut out = Vec::new();
    let mut block = 0u64;
    for task in &plan.tasks {
        for rep in 1..=plan.reps {
            for arm in block_order(plan.seed, block) {
                let input = trial_input(task, &plan.experiment, arm, rep);
                out.push(run_trial(pipeline, slots, orchestrator, &input, arm, rep).await?);
            }
            block += 1;
        }
    }
    Ok(out)
}

async fn run_trial(
    pipeline: &TicketPipeline,
    slots: &Slots,
    orchestrator: &Orchestrator,
    input: &TicketInput,
    arm: Arm,
    rep: u32,
) -> Result<TrialRun, guard_core::ports::StoreError> {
    let alias = input
        .experiment
        .as_ref()
        .map(|e| e.alias.clone())
        .unwrap_or_default();
    let mut trial = TrialRun {
        alias: alias.clone(),
        arm,
        task: input.ticket.clone(),
        rep,
        resolved: false,
        cost_usd: 0.0,
        spend_records: 0,
        error: None,
    };
    let slot = slots.task_slot().await;
    let mut result: Result<_, PipelineError> = pipeline.run_ticket(input, &slot).await;
    if result.is_err() {
        result = pipeline.run_ticket(input, &slot).await;
    }
    let ctx = RunContext {
        run_id: input.run_id.clone(),
        task_id: None,
        repo: input.repo.clone(),
        ticket: input.ticket.clone(),
        base_commit: input.base_commit.clone(),
    };
    match result {
        Ok(report) => {
            trial.cost_usd = report.total_cost_usd();
            trial.resolved = matches!(
                report.coder.as_ref().map(|c| &c.outcome),
                Some(StageOutcome::Passed { .. })
            );
            for stage in std::iter::once(&report.qa).chain(report.coder.as_ref()) {
                crate::runs::record_stage(orchestrator.runs.as_ref(), &ctx, stage).await?;
            }
            let mut after = crate::lane::AfterStage::default();
            orchestrator.ingest_alias(&alias, &mut after).await;
            trial.spend_records = after.spend_records;
            if !after.warnings.is_empty() {
                trial.error = Some(after.warnings.join("; "));
            }
        }
        Err(e) => trial.error = Some(e.to_string()),
    }
    Ok(trial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_and_run_ids_follow_the_contract() {
        assert_eq!(alias("exp1", Arm::Both, "SHOP-1", 2), "exp1:both:SHOP-1:r2");
        let task = TicketInput {
            repo: "acme/shop".into(),
            ticket: "SHOP-1".into(),
            run_id: "x".into(),
            base_commit: "abc1234".into(),
            summary: String::new(),
            acceptance_criteria: vec![],
            app_paths: vec![],
            tests_dir: "tests".into(),
            labels: vec![],
            predicted_files: vec![],
            preview: None,
            package_mirror: None,
            experiment: None,
            areas: vec![],
            journeys: vec![],
            claim: 0,
        };
        let t = trial_input(&task, "exp1", Arm::Memory, 3);
        assert_eq!(t.run_id, "exp1-memory-SHOP-1-r3");
        let e = t.experiment.unwrap();
        assert_eq!(
            (e.alias.as_str(), e.memory, e.map),
            ("exp1:memory:SHOP-1:r3", true, false)
        );
    }

    #[test]
    fn block_orders_are_seeded_permutations() {
        let a = block_order(7, 0);
        assert_eq!(a, block_order(7, 0));
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, Arm::ALL.to_vec());
        assert!(
            (0..8).any(|b| block_order(7, b) != a),
            "orders vary across blocks"
        );
    }
}
