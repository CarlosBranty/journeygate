//! The 4-arm token A/B harness end to end ON FAKES ONLY: real
//! `TicketPipeline` + `ab::run_experiment` + `PgRunLog` + spend ingestion
//! into live Postgres, then `load_ab_inputs` + `compute_ab_report` with the
//! pre-registered thresholds. The runtime is a deterministic fake whose
//! token use drops when the brief / `guard_map` are present, and its spend
//! rows come from `FakeModelGateway`. The numbers prove the plumbing, not
//! any real saving: real numbers need provider credentials.

mod common;

use common::pipeline_fakes::*;
use guard_agents::ab::{AbPlan, run_experiment};
use guard_agents::lane::Orchestrator;
use guard_agents::pipeline::seams::TicketInput;
use guard_agents::pipeline::{PipelineDeps, TicketPipeline};
use guard_agents::runs::{PgRunLog, now_unix, rfc3339};
use guard_agents::scheduler::{FixedSpend, Slots};
use guard_core::ports::fakes::{
    FakeAgentRuntime, FakeModelGateway, FakeSandboxProvider, InMemoryEvidenceStore,
};
use guard_core::ports::*;
use guard_store::spend::PgSpendStore;
use guard_store::spend_report::{
    Arm, Preregistration, Verdict, compute_ab_report, load_ab_inputs, render_markdown,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A fake runtime with arm-dependent token use; logs one gateway spend row
/// per attempt under the trial's alias. No model is called.
struct MeteredRuntime {
    gateway: Arc<FakeModelGateway>,
    seq: AtomicU64,
}

/// `exp-<arm>-<ticket>-r<rep>` → `exp:<arm>:<ticket>:r<rep>`.
fn alias_of(run_id: &str) -> String {
    let (exp, rest) = run_id.split_once('-').unwrap();
    let (arm, rest) = rest.split_once('-').unwrap();
    let (ticket, rep) = rest.rsplit_once('-').unwrap();
    format!("{exp}:{arm}:{ticket}:{rep}")
}

#[async_trait::async_trait]
impl AgentRuntime for MeteredRuntime {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn run(&self, _: &Sandbox, task: &AgentTask) -> Result<AgentOutcome, RuntimeError> {
        let memory = task.facts_brief.is_some();
        let map = task.tools.iter().any(|t| t == "guard_map");
        // Deterministic per task: some tasks are "hard" (more tokens).
        let hard = task.ticket.bytes().map(u64::from).sum::<u64>() % 3 == 0;
        let mut prompt = if hard { 60_000.0 } else { 40_000.0 };
        if memory {
            prompt *= 0.75;
        }
        if map {
            prompt *= 0.88;
        }
        let prompt = prompt as u64;
        let cost = prompt as f64 * 3e-6 + 2_000.0 * 15e-6;
        let n = self.seq.fetch_add(1, Ordering::SeqCst);
        let now = rfc3339(now_unix());
        self.gateway.push_spend(SpendRecord {
            request_id: format!("ab-req-{n}"),
            key_alias: Some(alias_of(&task.run_id)),
            model: task.model.clone(),
            spend_usd: cost,
            prompt_tokens: prompt,
            completion_tokens: 2_000,
            cached_tokens: prompt / 2,
            cache_creation_input_tokens: 0,
            cache_hit: None,
            start_time: now.clone(),
            end_time: now,
            metadata: Default::default(),
        });
        let mut o = FakeAgentRuntime::completed("done", cost);
        o.result.steps = if memory || map { 6 } else { 9 };
        if memory {
            o.events.push(RuntimeEvent {
                seq: 2,
                ts: "2026-11-01T00:00:01Z".into(),
                event: RuntimeEventKind::ToolCall {
                    tool: "guard_history".into(),
                    ok: true,
                    duration_ms: 2,
                },
            });
        }
        Ok(o)
    }
}

/// QA writes `tests/features/<KEY>.spec.ts`; the Coder fixes `src/<key>.ts`.
#[derive(Default)]
struct TicketSandbox {
    inner: FakeSandboxProvider,
    specs: Mutex<Vec<(SandboxId, SandboxSpec)>>,
}

impl TicketSandbox {
    fn spec_of(&self, id: &SandboxId) -> SandboxSpec {
        self.specs
            .lock()
            .unwrap()
            .iter()
            .find(|(i, _)| i == id)
            .unwrap()
            .1
            .clone()
    }
}

#[async_trait::async_trait]
impl SandboxProvider for TicketSandbox {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox, SandboxError> {
        let sb = self.inner.create(spec).await?;
        self.specs
            .lock()
            .unwrap()
            .push((sb.id.clone(), spec.clone()));
        Ok(sb)
    }
    async fn put_file(&self, s: &Sandbox, p: &str, b: Vec<u8>) -> Result<(), SandboxError> {
        self.inner.put_file(s, p, b).await
    }
    async fn read_file(&self, s: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
        let spec = self.spec_of(&s.id);
        let ticket = &spec.layout.ticket;
        if path == format!("/workspace/tests/features/{ticket}.spec.ts") {
            return Ok(format!(
                "test('fix @ticket:{ticket}', async ({{ page }}) => {{ await expect(page.getByText('ok')).toBeVisible(); }});\n"
            )
            .into_bytes());
        }
        self.inner.read_file(s, path).await
    }
    async fn exec(&self, s: &Sandbox, r: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        self.inner.exec(s, r).await
    }
    async fn extract_diff(&self, s: &Sandbox, base: &str) -> Result<ExtractedDiff, SandboxError> {
        let spec = self.spec_of(&s.id);
        let ticket = &spec.layout.ticket;
        let path = match spec.role {
            Role::Qa => format!("tests/features/{ticket}.spec.ts"),
            Role::Coder => format!("src/{}.ts", ticket.to_lowercase()),
        };
        Ok(ExtractedDiff {
            base_commit: base.into(),
            unified_diff: format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n+// FIX\n"),
            changed_paths: vec![path],
        })
    }
    async fn destroy(&self, s: &Sandbox) -> Result<(), SandboxError> {
        self.inner.destroy(s).await
    }
}

/// Each task belongs to one of the seeded areas: the facts brief carries only
/// the repo facts plus the ticket's areas and journeys.
fn task(n: usize) -> TicketInput {
    TicketInput {
        ticket: format!("SHOP-{}", 200 + n),
        areas: vec![["catalog", "checkout", "account"][n % 3].into()],
        ..input()
    }
}

#[tokio::test]
#[ignore = "live Postgres: DATABASE_URL"]
async fn four_arm_ab_runs_end_to_end_on_fakes() {
    let Some(pg) = common::pg().await else { return };
    let config = Arc::new(config(false, None));
    let slots = Arc::new(Slots::new(
        &config.scheduler,
        Arc::new(FixedSpend::new(0.0)),
    ));
    let gateway = Arc::new(FakeModelGateway::new());
    let facts = Arc::new(guard_store::PgMemoryFacts::new(pg.pool.clone()));
    for (i, area) in ["catalog", "checkout", "account"].iter().enumerate() {
        facts
            .record_fact(&NewFact {
                repo: "acme/shop".into(),
                scope: FactScope::Area,
                scope_key: Some((*area).into()),
                topic_key: format!("fix/{area}-{i}"),
                body: format!("{area} logic lives in src/"),
                commit_sha: BASE.into(),
                run_id: None,
                evidence_uri: None,
                source: FactSource::FixCommit,
                valid_from: "2026-10-01T00:00:00Z".into(),
                review_after: None,
                referenced_paths: vec![format!("src/shop-{}.ts", 200 + i)],
            })
            .await
            .unwrap();
    }
    let runtime: Arc<dyn AgentRuntime> = Arc::new(MeteredRuntime {
        gateway: gateway.clone(),
        seq: AtomicU64::new(0),
    });
    let pipeline = TicketPipeline::new(PipelineDeps {
        config,
        slots: slots.clone(),
        runtimes: vec![runtime],
        sandbox: Arc::new(TicketSandbox::default()),
        gateway: gateway.clone(),
        evidence: Arc::new(InMemoryEvidenceStore::new()),
        layouts: Arc::new(FakeLayouts::default()),
        checks: Arc::new(FakeChecks::default()),
        publisher: Arc::new(FakePublisher::default()),
        facts: Some(read_only(facts.clone())),
    })
    .unwrap();
    let orchestrator = Orchestrator {
        runs: Arc::new(PgRunLog::new(pg.pool.clone())),
        facts: None,
        gateway: gateway.clone(),
        spend: Some(Arc::new(PgSpendStore::new(pg.pool.clone()))),
        evidence_base: None,
    };
    let plan = AbPlan {
        experiment: "fakeab1".into(),
        tasks: (0..30).map(task).collect(),
        reps: 3,
        seed: 42,
    };
    let trials = run_experiment(&pipeline, &slots, &orchestrator, &plan)
        .await
        .unwrap();
    assert_eq!(trials.len(), 30 * 3 * 4);
    assert!(
        trials.iter().all(|t| t.resolved && t.error.is_none()),
        "{:?}",
        trials.iter().find(|t| t.error.is_some())
    );
    assert!(
        trials.iter().all(|t| t.spend_records == 2),
        "qa + coder spend rows per trial"
    );

    let inputs = load_ab_inputs(&pg.pool, "fakeab1").await.unwrap();
    assert_eq!(inputs.runs.len(), 30 * 3 * 4 * 2);
    let report = compute_ab_report(&inputs, &Preregistration::default());
    let md = render_markdown(&report);
    println!("ON FAKES ONLY (no provider called; real numbers need credentials):\n{md}");
    assert_eq!(report.unattributed_runs, 0);
    assert_eq!(report.trials.len(), 360);
    let verdict = |arm: Arm| {
        report
            .comparisons
            .iter()
            .find(|c| c.arm == arm)
            .unwrap()
            .verdict
    };
    // The fake cuts prompt tokens by 25 % (memory), 12 % (map), 34 % (both):
    // with 30 paired tasks the pre-registered rule gives memory and both a win,
    // map (< 15 %) no win.
    assert_eq!(verdict(Arm::Memory), Verdict::Wins);
    assert_eq!(verdict(Arm::Map), Verdict::DoesNotWin);
    assert_eq!(verdict(Arm::Both), Verdict::Wins);
    let memory = report.arms.iter().find(|a| a.arm == Arm::Memory).unwrap();
    println!("memory arm cited-in-diff: {:?}", memory.cited_in_diff_share);

    // Fewer than 30 tasks: inconclusive by the pre-registered rule.
    let small = AbPlan {
        experiment: "fakeab2".into(),
        tasks: (0..5).map(task).collect(),
        reps: 3,
        seed: 42,
    };
    run_experiment(&pipeline, &slots, &orchestrator, &small)
        .await
        .unwrap();
    let report = compute_ab_report(
        &load_ab_inputs(&pg.pool, "fakeab2").await.unwrap(),
        &Preregistration::default(),
    );
    assert!(
        report
            .comparisons
            .iter()
            .all(|c| c.verdict == Verdict::Inconclusive)
    );
    pg.cleanup().await;
}
