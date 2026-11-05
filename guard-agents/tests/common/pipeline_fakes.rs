//! Scripted seams for pipeline and worker tests. Everything is in memory.
#![allow(dead_code)]

use guard_agents::config::{AgentsConfig, LoadOptions};
use guard_agents::pipeline::seams::{
    GateResult, LayoutProvider, LayoutRequest, PrPublisher, PrRequest, PublishedPr, QualityReport,
    RedFirst, SeamError, TicketChecks, TicketInput,
};
use guard_agents::pipeline::{PipelineDeps, TicketPipeline};
use guard_agents::scheduler::{FixedSpend, Slots};
use guard_core::ports::fakes::{
    FakeAgentRuntime, FakeModelGateway, FakeSandboxProvider, InMemoryEvidenceStore,
    InMemoryMemoryFacts,
};
use guard_core::ports::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

pub const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
pub const SPEC_PATH: &str = "tests/features/SHOP-101.spec.ts";
pub const GOOD_SPEC: &[u8] =
    b"test('badge @ticket:SHOP-101', async ({ page }) => { await expect(page.getByText('Out of stock')).toBeVisible(); });\n";

pub fn config_text(extra_coder: bool, best_of_n: Option<&str>) -> String {
    let mut s = String::from(
        r#"version = 1
[gateway]
url = "http://llm-gateway:4000"
master_key_env = "LITELLM_MASTER_KEY"
rpm_limit = 60
[roles]
qa = "qa"
coder = "coder"
[agents.qa]
role = "qa"
runtime = "fake"
model = "anthropic/claude-sonnet-4-5"
budget_usd = 2.0
max_attempts = 2
[agents.coder]
role = "coder"
runtime = "fake"
model = "openai/gpt-5"
budget_usd = 5.0
max_attempts = 3
"#,
    );
    if extra_coder {
        s.push_str(
            r#"[agents.coder-b]
role = "coder"
runtime = "fake"
model = "anthropic/claude-sonnet-4-5"
budget_usd = 4.0
max_attempts = 2
"#,
        );
    }
    s.push_str(
        r#"[scheduler]
host_slots = 4
daily_usd_cap = 50.0
provider_slots = { openai = 2 }
image = "ghcr.io/acme/guard-sandbox@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
    );
    if let Some(label) = best_of_n {
        s.push_str(&format!(
            "[scheduler.best_of_n]\nagents = [\"coder\", \"coder-b\"]\nlabel = \"{label}\"\n"
        ));
    }
    s
}

pub fn config(extra_coder: bool, best_of_n: Option<&str>) -> AgentsConfig {
    AgentsConfig::parse(
        &config_text(extra_coder, best_of_n),
        &LoadOptions {
            allow_fake_runtime: true,
        },
    )
    .expect("test config")
}

pub fn input() -> TicketInput {
    TicketInput {
        repo: "acme/shop".into(),
        ticket: "SHOP-101".into(),
        run_id: "run-1".into(),
        base_commit: BASE.into(),
        summary: "Out-of-stock badge missing".into(),
        acceptance_criteria: vec!["A product with stock 0 shows 'Out of stock'".into()],
        app_paths: vec!["src/**".into()],
        tests_dir: "tests".into(),
        labels: vec![],
        predicted_files: vec!["src/product-card.tsx".into()],
        preview: Some(guard_agents::pipeline::seams::Endpoint {
            host: "preview-shop-101".into(),
            port: 3000,
        }),
        package_mirror: None,
        experiment: None,
        areas: vec![],
        journeys: vec![],
        claim: 0,
    }
}

pub fn diff(paths: &[&str], body: &str) -> ExtractedDiff {
    ExtractedDiff {
        base_commit: BASE.into(),
        unified_diff: body.into(),
        changed_paths: paths.iter().map(|s| s.to_string()).collect(),
    }
}

/// `n` added lines on `path`.
pub fn app_diff(path: &str, n: usize) -> ExtractedDiff {
    let mut body = format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n");
    for i in 0..n {
        body.push_str(&format!("+line {i}\n"));
    }
    diff(&[path], &body)
}

pub fn qa_diff() -> ExtractedDiff {
    diff(
        &[SPEC_PATH],
        "+++ b/tests/features/SHOP-101.spec.ts\n+spec\n",
    )
}

pub fn status_outcome(status: TaskStatus, cost: f64) -> AgentOutcome {
    let mut o = FakeAgentRuntime::completed("done", cost);
    o.result.status = status;
    if status != TaskStatus::Completed {
        o.result.error = Some(format!("{status:?}"));
    }
    o
}

// ---------------------------------------------------------------- sandbox

#[derive(Clone, Default)]
pub struct Script {
    pub diff: Option<ExtractedDiff>,
    pub files: Vec<(String, Vec<u8>)>,
}

/// `FakeSandboxProvider` plus per-(agent, role, attempt) diffs and files.
/// The fake layout puts the agent, role and attempt into the checkout path
/// (`/host/<agent>/<role>/<attempt>`), which is how a sandbox finds its script.
#[derive(Default)]
pub struct ScriptedSandbox {
    pub inner: FakeSandboxProvider,
    scripts: Mutex<BTreeMap<String, Script>>,
    by_sandbox: Mutex<BTreeMap<SandboxId, String>>,
    pub created: Mutex<Vec<(SandboxId, SandboxSpec)>>,
}

impl ScriptedSandbox {
    pub fn script(&self, agent: &str, role: Role, attempt: u32, script: Script) {
        self.scripts
            .lock()
            .unwrap()
            .insert(checkout(agent, role, attempt), script);
    }

    pub fn qa_writes(&self, attempt: u32, spec: &[u8]) {
        self.script(
            "qa",
            Role::Qa,
            attempt,
            Script {
                diff: Some(qa_diff()),
                files: vec![(format!("/workspace/{SPEC_PATH}"), spec.to_vec())],
            },
        );
    }

    pub fn coder_writes(&self, agent: &str, attempt: u32, d: ExtractedDiff) {
        self.script(
            agent,
            Role::Coder,
            attempt,
            Script {
                diff: Some(d),
                files: vec![],
            },
        );
    }

    pub fn specs(&self) -> Vec<(SandboxId, SandboxSpec)> {
        self.created.lock().unwrap().clone()
    }

    pub fn all_destroyed(&self) -> bool {
        self.specs()
            .iter()
            .all(|(id, _)| self.inner.is_destroyed(id))
    }

    fn script_of(&self, id: &SandboxId) -> Script {
        let key = self.by_sandbox.lock().unwrap().get(id).cloned();
        key.and_then(|k| self.scripts.lock().unwrap().get(&k).cloned())
            .unwrap_or_default()
    }
}

pub fn checkout(agent: &str, role: Role, attempt: u32) -> String {
    format!("/host/{agent}/{}/{attempt}", role.as_str())
}

#[async_trait::async_trait]
impl SandboxProvider for ScriptedSandbox {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox, SandboxError> {
        let sb = self.inner.create(spec).await?;
        self.by_sandbox
            .lock()
            .unwrap()
            .insert(sb.id.clone(), spec.layout.checkout.clone());
        self.created
            .lock()
            .unwrap()
            .push((sb.id.clone(), spec.clone()));
        Ok(sb)
    }
    async fn put_file(&self, s: &Sandbox, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
        self.inner.put_file(s, path, bytes).await
    }
    async fn read_file(&self, s: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
        if self.inner.is_destroyed(&s.id) {
            return Err(SandboxError::NotFound(s.id.0.clone()));
        }
        match self
            .script_of(&s.id)
            .files
            .into_iter()
            .find(|(p, _)| p == path)
        {
            Some((_, b)) => Ok(b),
            None => self.inner.read_file(s, path).await,
        }
    }
    async fn exec(&self, s: &Sandbox, r: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        self.inner.exec(s, r).await
    }
    async fn extract_diff(&self, s: &Sandbox, base: &str) -> Result<ExtractedDiff, SandboxError> {
        if self.inner.is_destroyed(&s.id) {
            return Err(SandboxError::NotFound(s.id.0.clone()));
        }
        Ok(self.script_of(&s.id).diff.unwrap_or(ExtractedDiff {
            base_commit: base.into(),
            unified_diff: String::new(),
            changed_paths: vec![],
        }))
    }
    async fn destroy(&self, s: &Sandbox) -> Result<(), SandboxError> {
        self.inner.destroy(s).await
    }
}

// ---------------------------------------------------------------- seams

#[derive(Default)]
pub struct FakeLayouts {
    pub sealed_copies: Mutex<Vec<(String, Vec<u8>)>>,
    /// Checkouts of released layouts.
    pub released: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl LayoutProvider for FakeLayouts {
    async fn layout(&self, req: &LayoutRequest<'_>) -> Result<SandboxLayout, SeamError> {
        let sealed = req.sealed.map(|(spec, bytes)| {
            let host = format!("/host/sealed/{}/{}", req.input.run_id, req.input.ticket);
            self.sealed_copies
                .lock()
                .unwrap()
                .push((host.clone(), bytes.to_vec()));
            (host, spec.path.clone())
        });
        Ok(SandboxLayout {
            run_id: req.input.run_id.clone(),
            ticket: req.input.ticket.clone(),
            attempt: req.attempt,
            image: req.image.to_string(),
            checkout: checkout(req.agent, req.role, req.attempt),
            task_dir: format!("/host/task/{}/{}", req.agent, req.attempt),
            codegraph_index: Some("/host/codegraph".into()),
            sealed_spec: sealed,
            tests_dir: req.input.tests_dir.clone(),
            skills_dir: Some(format!(
                "/host/.guard/agents/rendered/{}",
                req.runtime.as_str()
            )),
        })
    }

    async fn release(&self, layout: &SandboxLayout) -> Result<(), SeamError> {
        self.released.lock().unwrap().push(layout.checkout.clone());
        Ok(())
    }
}

/// Scripted orchestrator checks. Quality/red-first answers are consumed in
/// order (default: clean / failed). The gate passes when the diff contains
/// `FIX`, unless `gate_fails_always` is set.
#[derive(Default)]
pub struct FakeChecks {
    pub quality: Mutex<VecDeque<Vec<String>>>,
    pub red_first: Mutex<VecDeque<RedFirst>>,
    pub scan_findings: Mutex<Vec<String>>,
    pub gate_calls: Mutex<Vec<(ExtractedDiff, SealedSpec, Vec<u8>)>>,
    pub red_first_calls: Mutex<u32>,
    pub gate_fails_always: Mutex<bool>,
}

#[async_trait::async_trait]
impl TicketChecks for FakeChecks {
    async fn test_quality(
        &self,
        _input: &TicketInput,
        _path: &str,
        _bytes: &[u8],
    ) -> Result<QualityReport, SeamError> {
        Ok(QualityReport {
            failed: self.quality.lock().unwrap().pop_front().unwrap_or_default(),
        })
    }
    async fn red_first(
        &self,
        _input: &TicketInput,
        _path: &str,
        _bytes: &[u8],
    ) -> Result<RedFirst, SeamError> {
        *self.red_first_calls.lock().unwrap() += 1;
        Ok(self
            .red_first
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(RedFirst::Failed))
    }
    async fn scan_diff(
        &self,
        _input: &TicketInput,
        _diff: &ExtractedDiff,
    ) -> Result<Vec<String>, SeamError> {
        Ok(self.scan_findings.lock().unwrap().clone())
    }
    async fn gate(
        &self,
        _input: &TicketInput,
        diff: &ExtractedDiff,
        sealed: &SealedSpec,
        sealed_bytes: &[u8],
    ) -> Result<GateResult, SeamError> {
        self.gate_calls
            .lock()
            .unwrap()
            .push((diff.clone(), sealed.clone(), sealed_bytes.to_vec()));
        let passed = diff.unified_diff.contains("FIX") && !*self.gate_fails_always.lock().unwrap();
        Ok(GateResult {
            passed,
            summary: if passed { "green" } else { "red" }.into(),
            failure_context: (!passed).then(|| "SHOP-101 spec: expected badge, got none".into()),
        })
    }
}

#[derive(Default)]
pub struct FakePublisher {
    pub requests: Mutex<Vec<PrRequest>>,
}

#[async_trait::async_trait]
impl PrPublisher for FakePublisher {
    async fn publish(&self, req: &PrRequest) -> Result<PublishedPr, SeamError> {
        self.requests.lock().unwrap().push(req.clone());
        Ok(PublishedPr {
            url: format!("https://github.com/{}/pull/7", req.repo),
            number: Some(7),
        })
    }
}

// ---------------------------------------------------------------- world

pub struct World {
    pub pipeline: Arc<TicketPipeline>,
    pub slots: Arc<Slots>,
    pub spend: Arc<FixedSpend>,
    pub runtime: Arc<FakeAgentRuntime>,
    pub sandbox: Arc<ScriptedSandbox>,
    pub gateway: Arc<FakeModelGateway>,
    pub evidence: Arc<InMemoryEvidenceStore>,
    pub layouts: Arc<FakeLayouts>,
    pub checks: Arc<FakeChecks>,
    pub publisher: Arc<FakePublisher>,
    pub facts: Arc<InMemoryMemoryFacts>,
}

pub fn world(config: AgentsConfig, runtime: FakeAgentRuntime) -> World {
    world_with(config, runtime, None)
}

pub fn world_with(
    config: AgentsConfig,
    runtime: FakeAgentRuntime,
    aliases: Option<guard_agents::pipeline::SecretAliases>,
) -> World {
    let config = Arc::new(config);
    let spend = Arc::new(FixedSpend::new(0.0));
    let slots = Arc::new(Slots::new(&config.scheduler, spend.clone()));
    let runtime = Arc::new(runtime);
    let sandbox = Arc::new(ScriptedSandbox::default());
    let gateway = Arc::new(FakeModelGateway::new());
    let evidence = Arc::new(InMemoryEvidenceStore::new());
    let layouts = Arc::new(FakeLayouts::default());
    let checks = Arc::new(FakeChecks::default());
    let publisher = Arc::new(FakePublisher::default());
    let facts = Arc::new(InMemoryMemoryFacts::new());
    let runtimes: Vec<Arc<dyn AgentRuntime>> = vec![runtime.clone()];
    let pipeline = TicketPipeline::new(PipelineDeps {
        config,
        slots: slots.clone(),
        runtimes,
        sandbox: sandbox.clone(),
        gateway: gateway.clone(),
        evidence: evidence.clone(),
        layouts: layouts.clone(),
        checks: checks.clone(),
        publisher: publisher.clone(),
        facts: Some(facts.reader()),
    })
    .expect("pipeline");
    let pipeline = match aliases {
        Some(a) => pipeline.with_secret_aliases(a),
        None => pipeline,
    };
    World {
        pipeline: Arc::new(pipeline),
        slots,
        spend,
        runtime,
        sandbox,
        gateway,
        evidence,
        layouts,
        checks,
        publisher,
        facts,
    }
}

pub fn completed_runtime(n: usize) -> FakeAgentRuntime {
    (0..n).fold(FakeAgentRuntime::new(), |r, _| {
        r.with_outcome(Ok(FakeAgentRuntime::completed("done", 0.25)))
    })
}
