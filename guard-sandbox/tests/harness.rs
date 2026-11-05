//! End-to-end harness tests against the core fakes (qa-coder-protocol.md,
//! "Testing"): red-first rejection, quality rejection, Coder diff rejection,
//! the human lane after the third attempt, and a tampered sealed copy.

use guard_core::ports::fakes::{
    FakeAgentRuntime, FakeModelGateway, FakeSandboxProvider, InMemoryEvidenceStore,
    InMemoryMemoryFacts,
};
use guard_core::ports::{
    AgentTask, AttemptPolicy, EvidenceError, EvidenceStore, ExecOutput, ExecRequest, ExtractedDiff,
    FactScope, FactSource, GatewayBinding, MemoryFactsWriter, MountMode, NewFact, ObjectRef, Role,
    RuntimeError, Sandbox, SandboxError, SandboxId, SandboxProvider, SandboxSpec, SealedSpec,
    TaskStatus,
};
use guard_sandbox::harness::quality::{QualityViolation, SpecQuality};
use guard_sandbox::harness::red_first::{FailedTest, SpecRun};
use guard_sandbox::harness::{
    AttemptOutcome, Harness, HarnessConfig, HarnessError, HostWorkspace, PreparedWorkspace,
    SpecRunner, TicketInput, Verdict,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const SPEC_PATH: &str = "tests/features/SHOP-1.spec.ts";
const SPEC: &[u8] = b"test('adds @ticket:SHOP-1', async ({ page }) => { await expect(page.getByTestId('total')).toHaveText('2'); });\n";

// ---------------------------------------------------------------- fakes

/// Wraps the core `FakeSandboxProvider` (which validates every spec) and
/// scripts, per created sandbox in order, its diff and the files it holds.
/// One created sandbox: its diff and the files it starts with.
type Plan = (ExtractedDiff, Vec<(String, Vec<u8>)>);

struct ScriptedSandbox {
    inner: FakeSandboxProvider,
    plans: Mutex<VecDeque<Plan>>,
    diffs: Mutex<BTreeMap<SandboxId, ExtractedDiff>>,
    created: Mutex<Vec<(Sandbox, SandboxSpec)>>,
}

impl ScriptedSandbox {
    fn new() -> Self {
        Self {
            inner: FakeSandboxProvider::new(),
            plans: Mutex::new(VecDeque::new()),
            diffs: Mutex::new(BTreeMap::new()),
            created: Mutex::new(vec![]),
        }
    }

    fn plan(self, paths: &[&str], files: &[(&str, &[u8])]) -> Self {
        self.plans.lock().unwrap().push_back((
            diff(paths),
            files
                .iter()
                .map(|(p, b)| (p.to_string(), b.to_vec()))
                .collect(),
        ));
        self
    }

    fn qa_writes(self, bytes: &[u8]) -> Self {
        let target = format!("/workspace/{SPEC_PATH}");
        self.plan(&[SPEC_PATH], &[(&target, bytes)])
    }

    fn coder_changes(self, paths: &[&str]) -> Self {
        self.plan(paths, &[])
    }

    fn created(&self) -> Vec<(Sandbox, SandboxSpec)> {
        self.created.lock().unwrap().clone()
    }
}

fn diff(paths: &[&str]) -> ExtractedDiff {
    ExtractedDiff {
        base_commit: BASE.into(),
        unified_diff: paths
            .iter()
            .map(|p| format!("diff --git a/{p} b/{p}\n--- a/{p}\n+++ b/{p}\n@@ -0,0 +1 @@\n+x\n"))
            .collect(),
        changed_paths: paths.iter().map(|s| s.to_string()).collect(),
    }
}

#[async_trait::async_trait]
impl SandboxProvider for ScriptedSandbox {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox, SandboxError> {
        let sb = self.inner.create(spec).await?;
        let (d, files) = self
            .plans
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted sandbox plan");
        for (p, b) in files {
            self.inner.put_file(&sb, &p, b).await?;
        }
        self.diffs.lock().unwrap().insert(sb.id.clone(), d);
        self.created
            .lock()
            .unwrap()
            .push((sb.clone(), spec.clone()));
        Ok(sb)
    }
    async fn put_file(&self, sb: &Sandbox, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        self.inner.put_file(sb, path, bytes).await
    }
    async fn read_file(&self, sb: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
        self.inner.read_file(sb, path).await
    }
    async fn exec(&self, sb: &Sandbox, req: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        self.inner.exec(sb, req).await
    }
    async fn extract_diff(&self, sb: &Sandbox, base: &str) -> Result<ExtractedDiff, SandboxError> {
        self.inner.extract_diff(sb, base).await?;
        Ok(self.diffs.lock().unwrap().get(&sb.id).cloned().unwrap())
    }
    async fn destroy(&self, sb: &Sandbox) -> Result<(), SandboxError> {
        self.inner.destroy(sb).await
    }
}

#[derive(Default)]
struct FakeWorkspace {
    prepared: Mutex<Vec<(Role, u32)>>,
    released: Mutex<u32>,
    staged: Mutex<Vec<Vec<u8>>>,
}

#[async_trait::async_trait]
impl HostWorkspace for FakeWorkspace {
    async fn prepare(
        &self,
        ticket: &str,
        role: Role,
        attempt: u32,
        base_commit: &str,
    ) -> Result<PreparedWorkspace, HarnessError> {
        assert_eq!((ticket, base_commit), ("SHOP-1", BASE));
        self.prepared.lock().unwrap().push((role, attempt));
        Ok(PreparedWorkspace {
            checkout: format!("/host/wt/{}-{attempt}", role.as_str()),
            task_dir: format!("/host/task/{}-{attempt}", role.as_str()),
            codegraph_index: Some("/host/codegraph".into()),
            skills_dir: None,
        })
    }
    async fn stage_sealed(
        &self,
        _ticket: &str,
        attempt: u32,
        _sealed: &SealedSpec,
        bytes: &[u8],
    ) -> Result<String, HarnessError> {
        self.staged.lock().unwrap().push(bytes.to_vec());
        Ok(format!("/host/sealed/{attempt}.spec.ts"))
    }
    async fn release(&self, _p: &PreparedWorkspace) -> Result<(), HarnessError> {
        *self.released.lock().unwrap() += 1;
        Ok(())
    }
}

#[derive(Default)]
struct ScriptedRunner {
    base: Mutex<VecDeque<SpecRun>>,
    gate: Mutex<VecDeque<SpecRun>>,
    gate_calls: Mutex<Vec<(ExtractedDiff, Vec<u8>)>>,
}

impl ScriptedRunner {
    fn base(self, run: SpecRun) -> Self {
        self.base.lock().unwrap().push_back(run);
        self
    }
    fn gate(self, run: SpecRun) -> Self {
        self.gate.lock().unwrap().push_back(run);
        self
    }
}

#[async_trait::async_trait]
impl SpecRunner for ScriptedRunner {
    async fn run_on_base(
        &self,
        base: &str,
        path: &str,
        bytes: &[u8],
    ) -> Result<SpecRun, HarnessError> {
        assert_eq!((base, path), (BASE, SPEC_PATH));
        assert!(!bytes.is_empty());
        Ok(self
            .base
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted base run"))
    }
    async fn run_gate(
        &self,
        base: &str,
        diff: &ExtractedDiff,
        sealed: &SealedSpec,
        bytes: &[u8],
    ) -> Result<SpecRun, HarnessError> {
        assert_eq!(base, BASE);
        assert!(sealed.verify(bytes), "the gate gets verified sealed bytes");
        self.gate_calls
            .lock()
            .unwrap()
            .push((diff.clone(), bytes.to_vec()));
        Ok(self
            .gate
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted gate run"))
    }
}

#[derive(Default)]
struct ScriptedQuality(Mutex<VecDeque<Vec<QualityViolation>>>);

impl ScriptedQuality {
    fn reject(self, id: &str) -> Self {
        self.0.lock().unwrap().push_back(vec![QualityViolation {
            id: id.into(),
            line: 1,
            message: format!("{id} violated"),
        }]);
        self
    }
}

#[async_trait::async_trait]
impl SpecQuality for ScriptedQuality {
    async fn check(
        &self,
        ticket: &str,
        path: &str,
        _b: &[u8],
    ) -> Result<Vec<QualityViolation>, HarnessError> {
        assert_eq!((ticket, path), ("SHOP-1", SPEC_PATH));
        Ok(self.0.lock().unwrap().pop_front().unwrap_or_default())
    }
}

/// Evidence store whose `get_object` returns tampered bytes from the `n`th read on.
struct TamperingStore {
    inner: InMemoryEvidenceStore,
    reads: Mutex<u32>,
    tamper_from: u32,
}

#[async_trait::async_trait]
impl EvidenceStore for TamperingStore {
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        ct: &str,
    ) -> Result<ObjectRef, EvidenceError> {
        self.inner.put_object(key, bytes, ct).await
    }
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, EvidenceError> {
        let n = {
            let mut n = self.reads.lock().unwrap();
            *n += 1;
            *n
        };
        let mut bytes = self.inner.get_object(key).await?;
        if n >= self.tamper_from {
            bytes.extend_from_slice(b"\ntest.skip('x', () => {});");
        }
        Ok(bytes)
    }
    fn presign_get(&self, key: &str, e: Duration) -> Result<String, EvidenceError> {
        self.inner.presign_get(key, e)
    }
}

// ---------------------------------------------------------------- runs

fn red() -> SpecRun {
    SpecRun {
        failed: 1,
        assertion_failures: 1,
        failures: vec![FailedTest {
            title: "adds @ticket:SHOP-1".into(),
            status: "failed".into(),
            message: "expect(locator).toHaveText(expected) Expected: \"2\" Received: \"1\"".into(),
            assertion: true,
        }],
        ..SpecRun::default()
    }
}

fn green() -> SpecRun {
    SpecRun {
        passed: 3,
        ..SpecRun::default()
    }
}

fn completed() -> Result<guard_core::ports::AgentOutcome, RuntimeError> {
    Ok(FakeAgentRuntime::completed("done", 0.25))
}

fn config() -> HarnessConfig {
    HarnessConfig {
        repo: "acme/shop".into(),
        tests_dir: "tests".into(),
        app_paths: vec!["src/**".into()],
        image: format!("guard/sandbox@sha256:{DIGEST}"),
        qa_model: "claude-sonnet".into(),
        coder_model: "gpt-coder".into(),
        budget_usd: 2.0,
        timeout_secs: 900,
        gateway: GatewayBinding {
            base_url: "http://gw.internal:4000".into(),
            key_env: "GUARD_GATEWAY_KEY".into(),
            via_gateway: true,
        },
        gateway_host: "gw.internal".into(),
        gateway_port: 4000,
        preview: Some(("preview.internal".into(), 8080)),
        package_mirror: None,
        qa_policy: AttemptPolicy::default(),
        coder_policy: AttemptPolicy::default(),
        tools: vec!["bash".into(), "edit".into()],
        failure_context_max_bytes: 4096,
    }
}

fn input() -> TicketInput {
    TicketInput {
        run_id: "run-7".into(),
        ticket: "SHOP-1".into(),
        base_commit: BASE.into(),
        summary: "Cart shows the item count".into(),
        acceptance_criteria: vec!["total updates".into()],
    }
}

struct World {
    runtime: Arc<FakeAgentRuntime>,
    sandbox: Arc<ScriptedSandbox>,
    gateway: Arc<FakeModelGateway>,
    evidence: Arc<dyn EvidenceStore>,
    workspace: Arc<FakeWorkspace>,
    runner: Arc<ScriptedRunner>,
    harness: Harness,
}

async fn world(
    runtime: FakeAgentRuntime,
    sandbox: ScriptedSandbox,
    runner: ScriptedRunner,
    quality: ScriptedQuality,
    evidence: Arc<dyn EvidenceStore>,
) -> World {
    let runtime = Arc::new(runtime);
    let sandbox = Arc::new(sandbox);
    let gateway = Arc::new(FakeModelGateway::new());
    let workspace = Arc::new(FakeWorkspace::default());
    let runner = Arc::new(runner);
    let facts = Arc::new(InMemoryMemoryFacts::new());
    facts
        .record_fact(&NewFact {
            repo: "acme/shop".into(),
            scope: FactScope::Repo,
            scope_key: None,
            topic_key: "conventions/testids".into(),
            body: "Use data-testid for every assertion target.".into(),
            commit_sha: BASE.into(),
            run_id: None,
            evidence_uri: None,
            source: FactSource::ApprovedDecision,
            valid_from: "2026-10-01T00:00:00Z".into(),
            review_after: None,
            referenced_paths: vec![],
        })
        .await
        .unwrap();
    let harness = Harness {
        config: config(),
        runtime: runtime.clone(),
        sandbox: sandbox.clone(),
        gateway: gateway.clone(),
        evidence: evidence.clone(),
        facts: facts.reader(),
        workspace: workspace.clone(),
        runner: runner.clone(),
        quality: Arc::new(quality),
    };
    World {
        runtime,
        sandbox,
        gateway,
        evidence,
        workspace,
        runner,
        harness,
    }
}

fn store() -> Arc<dyn EvidenceStore> {
    Arc::new(InMemoryEvidenceStore::new())
}

fn tasks(w: &World) -> Vec<AgentTask> {
    w.runtime.tasks().into_iter().map(|(_, t)| t).collect()
}

fn assert_all_cleaned_up(w: &World) {
    for (sb, _) in w.sandbox.created() {
        assert!(
            w.sandbox.inner.is_destroyed(&sb.id),
            "{} not destroyed",
            sb.id.0
        );
    }
    for i in 1..=w.sandbox.created().len() {
        assert!(
            !w.gateway.is_active(&format!("key-{i}")),
            "key-{i} not revoked"
        );
    }
    assert_eq!(
        *w.workspace.released.lock().unwrap() as usize,
        w.workspace.prepared.lock().unwrap().len()
    );
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn red_spec_is_sealed_and_a_green_gate_passes() {
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(completed())
            .with_outcome(completed()),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default().base(red()).gate(green()),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert_eq!(report.verdict, Verdict::Passed);
    let sealed = report.sealed.clone().unwrap();
    assert_eq!(sealed, SealedSpec::seal(SPEC_PATH, SPEC));
    assert_eq!(
        w.evidence
            .get_object("sealed/run-7/SHOP-1.spec.ts")
            .await
            .unwrap(),
        SPEC
    );
    let outcomes: Vec<(Role, u32, AttemptOutcome)> = report
        .attempts
        .iter()
        .map(|a| (a.role, a.attempt, a.outcome))
        .collect();
    assert_eq!(
        outcomes,
        [
            (Role::Qa, 1, AttemptOutcome::Passed),
            (Role::Coder, 1, AttemptOutcome::Passed)
        ]
    );
    assert_eq!(
        report.attempts[0].sealed_spec_sha256.as_deref(),
        Some(sealed.sha256.as_str())
    );
    assert_eq!(report.diff.unwrap().changed_paths, ["src/cart.ts"]);

    let t = tasks(&w);
    assert_eq!(t[0].role, Role::Qa);
    assert_eq!(t[0].allowed_paths.write, [SPEC_PATH]);
    assert!(t[0].sealed_spec.is_none());
    assert_eq!(t[1].role, Role::Coder);
    assert_eq!(t[1].sealed_spec.as_ref(), Some(&sealed));
    assert!(t[1].allowed_paths.deny.contains(&"tests/**".to_string()));
    assert!(t[1].failure_context.is_none());
    for task in &t {
        let brief = task.facts_brief.as_ref().expect("facts brief injected");
        assert!(brief.text.contains("data-testid"));
    }

    let created = w.sandbox.created();
    let coder_spec = &created[1].1;
    let sealed_mount = coder_spec
        .mounts
        .iter()
        .find(|m| m.target == format!("/workspace/{SPEC_PATH}"))
        .unwrap();
    assert_eq!(sealed_mount.mode, MountMode::ReadOnly);
    assert_eq!(sealed_mount.source, "/host/sealed/1.spec.ts");
    assert_eq!(w.workspace.staged.lock().unwrap()[0], SPEC);
    assert!(coder_spec.secrets.contains_key("GUARD_GATEWAY_KEY"));
    let qa_hosts: Vec<&str> = created[0]
        .1
        .egress
        .rules
        .iter()
        .map(|r| r.host.as_str())
        .collect();
    assert_eq!(qa_hosts, ["gw.internal", "preview.internal"]);
    let coder_hosts: Vec<&str> = coder_spec
        .egress
        .rules
        .iter()
        .map(|r| r.host.as_str())
        .collect();
    assert_eq!(
        coder_hosts,
        ["gw.internal"],
        "coder never reaches the preview"
    );
    assert_all_cleaned_up(&w);
}

#[tokio::test]
async fn a_spec_that_passes_on_base_is_red_first_rejected_and_never_sealed() {
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(completed())
            .with_outcome(completed())
            .with_outcome(completed()),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .qa_writes(SPEC)
            .qa_writes(SPEC),
        ScriptedRunner::default()
            .base(green())
            .base(green())
            .base(green()),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert!(
        matches!(report.verdict, Verdict::HumanLane { ref reason } if reason.contains("qa")),
        "{:?}",
        report.verdict
    );
    assert!(report.sealed.is_none());
    assert!(
        report
            .attempts
            .iter()
            .all(|a| a.outcome == AttemptOutcome::RedFirstPassed)
    );
    assert_eq!(report.attempts.len(), 3);
    assert!(
        w.evidence
            .get_object("sealed/run-7/SHOP-1.spec.ts")
            .await
            .is_err()
    );
    let t = tasks(&w);
    assert!(
        t[1].failure_context
            .as_deref()
            .unwrap()
            .contains("passes on the base commit")
    );
    assert_all_cleaned_up(&w);
}

#[tokio::test]
async fn a_focused_spec_is_quality_rejected_then_a_fixed_one_proceeds() {
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(completed())
            .with_outcome(completed())
            .with_outcome(completed()),
        ScriptedSandbox::new()
            .qa_writes(b"test.only('x @ticket:SHOP-1', async () => { expect(a).toBe(1); });")
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default().base(red()).gate(green()),
        ScriptedQuality::default().reject("no_focus"),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert_eq!(report.verdict, Verdict::Passed);
    assert_eq!(report.attempts[0].outcome, AttemptOutcome::QualityRejected);
    assert_eq!(report.attempts[0].quality, ["no_focus"]);
    let t = tasks(&w);
    assert!(
        t[1].failure_context
            .as_deref()
            .unwrap()
            .contains("no_focus")
    );
}

#[tokio::test]
async fn a_qa_diff_outside_its_spec_is_rejected() {
    let w = world(
        FakeAgentRuntime::new().with_outcome(completed()),
        ScriptedSandbox::new().plan(&[SPEC_PATH, "src/app.ts"], &[]),
        ScriptedRunner::default(),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let mut cfg = config();
    cfg.qa_policy = AttemptPolicy::new(1).unwrap();
    let harness = Harness {
        config: cfg,
        ..w.harness
    };
    let report = harness.run_ticket(&input()).await.unwrap();
    assert_eq!(report.attempts[0].outcome, AttemptOutcome::DiffRejected);
    assert!(report.attempts[0].detail.contains("src/app.ts"));
    assert!(matches!(report.verdict, Verdict::HumanLane { .. }));
}

#[tokio::test]
async fn a_coder_diff_touching_tests_counts_as_an_attempt_with_feedback() {
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(completed())
            .with_outcome(completed())
            .with_outcome(completed()),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts", "tests/journeys/checkout.spec.ts"])
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default().base(red()).gate(green()),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert_eq!(report.verdict, Verdict::Passed);
    let coder: Vec<_> = report
        .attempts
        .iter()
        .filter(|a| a.role == Role::Coder)
        .collect();
    assert_eq!(coder[0].outcome, AttemptOutcome::DiffRejected);
    assert_eq!(
        (coder[1].attempt, coder[1].outcome),
        (2, AttemptOutcome::Passed)
    );
    let fb = tasks(&w)[2].failure_context.clone().unwrap();
    assert!(fb.contains("tests/journeys/checkout.spec.ts"), "{fb}");
    assert_eq!(
        w.runner.gate_calls.lock().unwrap().len(),
        1,
        "a rejected diff never reaches the gate"
    );
}

#[tokio::test]
async fn three_failed_coder_attempts_go_to_the_human_lane_with_failing_output_as_feedback() {
    let w = world(
        (0..4).fold(FakeAgentRuntime::new(), |r, _| r.with_outcome(completed())),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts"])
            .coder_changes(&["src/cart.ts"])
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default()
            .base(red())
            .gate(red())
            .gate(red())
            .gate(red()),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert!(
        matches!(report.verdict, Verdict::HumanLane { ref reason } if reason.contains("3")),
        "{:?}",
        report.verdict
    );
    let coder: Vec<_> = report
        .attempts
        .iter()
        .filter(|a| a.role == Role::Coder)
        .collect();
    assert_eq!(coder.len(), 3);
    assert!(
        coder
            .iter()
            .all(|a| a.outcome == AttemptOutcome::GateFailed)
    );
    let t = tasks(&w);
    assert_eq!(t.len(), 4);
    assert_eq!(t[3].attempt, 3);
    let fb = t[2].failure_context.clone().unwrap();
    assert!(
        fb.contains("adds @ticket:SHOP-1 [failed]") && fb.contains("Received"),
        "{fb}"
    );
    assert!(report.attempts.iter().map(|a| a.cost_usd).sum::<f64>() > 0.9);
    assert_all_cleaned_up(&w);
}

#[tokio::test]
async fn a_tampered_sealed_copy_aborts_before_the_gate() {
    // Reads: 1 = before the coder attempt, 2 = at finalize (tampered).
    let evidence: Arc<dyn EvidenceStore> = Arc::new(TamperingStore {
        inner: InMemoryEvidenceStore::new(),
        reads: Mutex::new(0),
        tamper_from: 2,
    });
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(completed())
            .with_outcome(completed()),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default().base(red()).gate(green()),
        ScriptedQuality::default(),
        evidence,
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert!(
        matches!(report.verdict, Verdict::Aborted { ref reason } if reason.contains("hash")),
        "{:?}",
        report.verdict
    );
    assert!(
        w.runner.gate_calls.lock().unwrap().is_empty(),
        "the gate never ran"
    );
    assert_all_cleaned_up(&w);
}

#[tokio::test]
async fn a_runtime_error_is_recorded_retried_and_still_cleans_up() {
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(Err(RuntimeError::Crashed {
                exit_code: Some(1),
                stderr_tail: "boom".into(),
            }))
            .with_outcome(completed())
            .with_outcome(completed()),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default().base(red()).gate(green()),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert_eq!(report.attempts[0].outcome, AttemptOutcome::RuntimeError);
    assert!(report.attempts[0].detail.contains("boom"));
    assert_eq!(report.verdict, Verdict::Passed);
    assert_all_cleaned_up(&w);
}

#[tokio::test]
async fn an_exhausted_budget_goes_straight_to_the_human_lane() {
    let mut over = FakeAgentRuntime::completed("ran out", 2.0);
    over.result.status = TaskStatus::BudgetExceeded;
    let w = world(
        FakeAgentRuntime::new()
            .with_outcome(completed())
            .with_outcome(Ok(over)),
        ScriptedSandbox::new()
            .qa_writes(SPEC)
            .coder_changes(&["src/cart.ts"]),
        ScriptedRunner::default().base(red()),
        ScriptedQuality::default(),
        store(),
    )
    .await;
    let report = w.harness.run_ticket(&input()).await.unwrap();
    assert!(
        matches!(report.verdict, Verdict::HumanLane { ref reason } if reason.contains("budget")),
        "{:?}",
        report.verdict
    );
    assert_eq!(report.attempts.len(), 2);
    assert_all_cleaned_up(&w);
}

#[tokio::test]
async fn attempt_outcomes_use_the_contract_names() {
    let names: Vec<&str> = [
        AttemptOutcome::Passed,
        AttemptOutcome::GateFailed,
        AttemptOutcome::RedFirstPassed,
        AttemptOutcome::DiffRejected,
        AttemptOutcome::QualityRejected,
        AttemptOutcome::RuntimeError,
    ]
    .iter()
    .map(|o| o.as_str())
    .collect();
    assert_eq!(
        names,
        [
            "passed",
            "gate_failed",
            "red_first_passed",
            "diff_rejected",
            "quality_rejected",
            "runtime_error"
        ]
    );
}
