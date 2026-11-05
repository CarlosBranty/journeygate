//! Regression tests of review findings on the agent pipeline, worker and lane.

mod common;

use common::pipeline_fakes::*;
use guard_agents::config::{AgentsConfig, LoadOptions};
use guard_agents::lane::{LoopStep, Orchestrator, run_loop};
use guard_agents::pipeline::{AttemptOutcome, HumanLaneReason, PipelineError, StageOutcome};
use guard_agents::runs::MemoryRunLog;
use guard_agents::scheduler::{SchedulerError, Slots, SpendLedger};
use guard_agents::worker::{Disposition, Tick, Worker, enqueue_ticket};
use guard_core::ports::fakes::{FakeAgentRuntime, FakeModelGateway, InMemoryTaskQueue};
use guard_core::ports::*;
use guard_store::spend::MemorySpendSink;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

async fn seal(w: &World) -> SealedSpec {
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let slot = w.slots.try_task_slot().unwrap();
    match w.pipeline.run_qa(&input(), &slot).await.unwrap().outcome {
        StageOutcome::Sealed(s) => s,
        other => panic!("{other:?}"),
    }
}

fn fact(scope: FactScope, key: Option<&str>, topic: &str) -> NewFact {
    NewFact {
        repo: "acme/shop".into(),
        scope,
        scope_key: key.map(str::to_string),
        topic_key: topic.into(),
        body: format!("fact {topic}"),
        commit_sha: BASE.into(),
        run_id: None,
        evidence_uri: None,
        source: FactSource::GateResult,
        valid_from: "2026-10-01T00:00:00Z".into(),
        review_after: None,
        referenced_paths: vec![],
    }
}

// ------------------------------------------------------------ timeouts

#[tokio::test]
async fn a_runtime_killed_at_its_timeout_goes_to_the_human_lane_without_retry() {
    let w = world(
        config(false, None),
        FakeAgentRuntime::new()
            .with_outcome(Ok(FakeAgentRuntime::completed("qa", 0.1)))
            .with_outcome(Err(RuntimeError::Timeout { secs: 1800 }))
            .with_outcome(Ok(FakeAgentRuntime::completed("never", 0.1))),
    );
    let sealed = seal(&w).await;
    w.sandbox.coder_writes("coder", 2, app_diff("src/a.ts", 1));
    let slot = w.slots.try_task_slot().unwrap();
    let coder = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert_eq!(coder.attempts.len(), 1, "no retry after a timeout");
    assert_eq!(
        coder.attempts[0].outcome,
        Some(AttemptOutcome::RuntimeError)
    );
    assert!(matches!(
        coder.outcome,
        StageOutcome::HumanLane(HumanLaneReason::TimedOut { ref agent }) if agent == "coder"
    ));
}

// ------------------------------------------------------------ memory reads

#[tokio::test]
async fn mcp_prefixed_guard_history_calls_count_as_memory_reads() {
    let mut qa = FakeAgentRuntime::completed("qa", 0.1);
    for (i, tool) in [
        "mcp__guard__guard_history",
        "guard_history",
        "mcp__guard__guard_map",
    ]
    .iter()
    .enumerate()
    {
        qa.events.push(RuntimeEvent {
            seq: i as u64 + 2,
            ts: "2026-11-01T00:00:00Z".into(),
            event: RuntimeEventKind::ToolCall {
                tool: tool.to_string(),
                ok: true,
                duration_ms: 1,
            },
        });
    }
    let w = world(
        config(false, None),
        FakeAgentRuntime::new().with_outcome(Ok(qa)),
    );
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_qa(&input(), &slot).await.unwrap();
    assert_eq!(report.attempts[0].memory_reads, 2);
}

// ------------------------------------------------------------ facts brief

#[tokio::test]
async fn the_facts_brief_pages_and_covers_only_repo_area_and_journey_facts() {
    let w = world(config(false, None), completed_runtime(1));
    // A full page of facts of an unrelated area comes first by id.
    for i in 0..60 {
        w.facts
            .record_fact(&fact(
                FactScope::Area,
                Some("billing"),
                &format!("billing/f{i:02}"),
            ))
            .await
            .unwrap();
    }
    let repo = w
        .facts
        .record_fact(&fact(FactScope::Repo, None, "ticket/shop-101/sealed-spec"))
        .await
        .unwrap();
    let area = w
        .facts
        .record_fact(&fact(FactScope::Area, Some("catalog"), "catalog/badge"))
        .await
        .unwrap();
    let journey = w
        .facts
        .record_fact(&fact(
            FactScope::Journey,
            Some("checkout"),
            "checkout/flaky",
        ))
        .await
        .unwrap();
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let mut i = input();
    i.areas = vec!["catalog".into()];
    i.journeys = vec!["checkout".into()];
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_qa(&i, &slot).await.unwrap();
    let ids = &report.attempts[0].facts_brief_ids;
    for id in [repo, area, journey] {
        assert!(ids.contains(&id.0), "fact {} missing from {ids:?}", id.0);
    }
    assert_eq!(ids.len(), 3, "no unrelated area facts: {ids:?}");
}

// ------------------------------------------------------------ key aliases

#[tokio::test]
async fn the_fake_gateway_refuses_a_duplicate_live_alias() {
    let g = FakeModelGateway::new();
    let c = config(false, None);
    let req = c.key_request("qa", "run-1:qa:1", Default::default());
    g.mint_key(&req).await.unwrap();
    assert!(matches!(
        g.mint_key(&req).await,
        Err(GatewayError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn a_queue_retry_mints_keys_under_a_fresh_alias() {
    let w = world(config(false, None), completed_runtime(1));
    // A key of the first claim was never revoked (worker killed mid-attempt).
    let c = config(false, None);
    w.gateway
        .mint_key(&c.key_request("qa", "run-1:qa:1", Default::default()))
        .await
        .unwrap();
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let mut i = input();
    i.claim = 2;
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_qa(&i, &slot).await.unwrap();
    assert_eq!(report.attempts[0].key_alias, "run-1:qa:1:c2");
}

#[test]
fn virtual_keys_outlive_the_agent_timeout() {
    let text = config_text(false, None).replace(
        "max_attempts = 3\n",
        "max_attempts = 3\ntimeout_secs = 10800\n",
    );
    let c = AgentsConfig::parse(
        &text,
        &LoadOptions {
            allow_fake_runtime: true,
        },
    )
    .unwrap();
    let req = c.key_request("coder", "a", Default::default());
    assert!(
        req.duration >= Duration::from_secs(10800 + 600),
        "{:?}",
        req.duration
    );
    // The default (7200 s) still applies to a short agent.
    assert_eq!(
        c.key_request("qa", "b", Default::default()).duration,
        Duration::from_secs(7200)
    );
}

// ------------------------------------------------------------ spend

/// Today's spend = what has been ingested into the sink.
struct SinkLedger(Arc<MemorySpendSink>);

#[async_trait::async_trait]
impl SpendLedger for SinkLedger {
    async fn spent_today_usd(&self) -> Result<f64, StoreError> {
        Ok(self.0.records().iter().map(|r| r.spend_usd).sum())
    }
}

fn spend(alias: &str, id: &str, usd: f64) -> SpendRecord {
    SpendRecord {
        request_id: id.into(),
        key_alias: Some(alias.into()),
        model: "openai/gpt-5".into(),
        spend_usd: usd,
        prompt_tokens: 10,
        completion_tokens: 1,
        cached_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_hit: None,
        start_time: "2026-11-01T10:00:00Z".into(),
        end_time: "2026-11-01T10:00:01Z".into(),
        metadata: Default::default(),
    }
}

#[tokio::test]
async fn spend_is_ingested_after_every_attempt_so_the_cap_stops_the_next_one() {
    let text = config_text(false, None).replace("daily_usd_cap = 50.0", "daily_usd_cap = 1.0");
    let cfg = Arc::new(
        AgentsConfig::parse(
            &text,
            &LoadOptions {
                allow_fake_runtime: true,
            },
        )
        .unwrap(),
    );
    let w = world((*cfg).clone(), completed_runtime(4));
    let sealed = seal(&w).await;
    let sink = Arc::new(MemorySpendSink::default());
    let slots = Arc::new(Slots::new(
        &cfg.scheduler,
        Arc::new(SinkLedger(sink.clone())),
    ));
    let pipeline =
        guard_agents::pipeline::TicketPipeline::new(guard_agents::pipeline::PipelineDeps {
            config: cfg,
            slots: slots.clone(),
            runtimes: vec![w.runtime.clone()],
            sandbox: w.sandbox.clone(),
            gateway: w.gateway.clone(),
            evidence: w.evidence.clone(),
            layouts: w.layouts.clone(),
            checks: w.checks.clone(),
            publisher: w.publisher.clone(),
            facts: None,
        })
        .unwrap()
        .with_spend_sink(sink.clone());
    // Attempt 1 spends $2 at the gateway (gate fails: no FIX marker).
    w.gateway.push_spend(spend("run-1:coder:1", "r1", 2.0));
    w.sandbox.coder_writes("coder", 1, app_diff("src/a.ts", 1));
    let slot = slots.try_task_slot().unwrap();
    let r = pipeline.run_coder(&input(), &sealed, &slot).await;
    assert!(
        matches!(
            r,
            Err(PipelineError::Scheduler(
                SchedulerError::DailyCapReached { .. }
            ))
        ),
        "{r:?}"
    );
    assert_eq!(sink.len(), 1, "attempt 1's spend ingested right after it");
}

// ------------------------------------------------------------ diff headers

#[tokio::test]
async fn patch_headers_must_agree_with_changed_paths() {
    let w = world(config(false, None), completed_runtime(4));
    let sealed = seal(&w).await;
    // diff.noprefix: the listed path is allowed, the applied path is not.
    w.sandbox.coder_writes(
        "coder",
        1,
        diff(
            &["src/.github/workflows/x.yml"],
            "diff --git src/.github/workflows/x.yml src/.github/workflows/x.yml\n\
             new file mode 100644\n--- /dev/null\n+++ src/.github/workflows/x.yml\n\
             @@ -0,0 +1 @@\n+on: push # FIX\n",
        ),
    );
    // The patch touches a test the name list omits.
    w.sandbox.coder_writes(
        "coder",
        2,
        diff(
            &["src/a.ts"],
            "diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n+// FIX\n\
             diff --git a/tests/smoke/x.spec.ts b/tests/smoke/x.spec.ts\n\
             --- a/tests/smoke/x.spec.ts\n+++ b/tests/smoke/x.spec.ts\n@@ -1 +1 @@\n+x\n",
        ),
    );
    // A removed `-- comment` line is hunk content, not a header.
    w.sandbox.coder_writes(
        "coder",
        3,
        diff(
            &["src/a.sql"],
            "diff --git a/src/a.sql b/src/a.sql\n--- a/src/a.sql\n+++ b/src/a.sql\n\
             @@ -1 +1 @@\n--- old comment\n+-- FIX\n",
        ),
    );
    let slot = w.slots.try_task_slot().unwrap();
    let coder = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    let outcomes: Vec<_> = coder.attempts.iter().map(|a| a.outcome).collect();
    assert_eq!(
        outcomes,
        vec![
            Some(AttemptOutcome::DiffRejected),
            Some(AttemptOutcome::DiffRejected),
            Some(AttemptOutcome::Passed)
        ],
        "{:?}",
        coder.attempts.iter().map(|a| &a.detail).collect::<Vec<_>>()
    );
    let gate = w.checks.gate_calls.lock().unwrap().clone();
    assert_eq!(gate.len(), 1, "only the consistent diff reaches the gate");
}

// ------------------------------------------------------------ layouts

#[tokio::test]
async fn every_attempt_layout_is_released() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes("coder", 1, {
        let mut d = app_diff("src/a.ts", 1);
        d.unified_diff.push_str("+// FIX\n");
        d
    });
    let slot = w.slots.try_task_slot().unwrap();
    w.pipeline.run_ticket(&input(), &slot).await.unwrap();
    let released = w.layouts.released.lock().unwrap().clone();
    assert_eq!(released.len(), 2, "{released:?}");
}

// ------------------------------------------------------------ worker / lane

/// Spent under the cap for the worker's pre-claim check, over it afterwards.
struct FlipLedger(AtomicU32);

#[async_trait::async_trait]
impl SpendLedger for FlipLedger {
    async fn spent_today_usd(&self) -> Result<f64, StoreError> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(if n % 2 == 0 { 0.0 } else { 1000.0 })
    }
}

#[tokio::test]
async fn daily_cap_reschedules_do_not_use_up_queue_attempts() {
    let w = world(config(false, None), completed_runtime(4));
    let cfg = Arc::new(config(false, None));
    let slots = Arc::new(Slots::new(
        &cfg.scheduler,
        Arc::new(FlipLedger(AtomicU32::new(0))),
    ));
    let pipeline = Arc::new(
        guard_agents::pipeline::TicketPipeline::new(guard_agents::pipeline::PipelineDeps {
            config: cfg,
            slots: slots.clone(),
            runtimes: vec![w.runtime.clone()],
            sandbox: w.sandbox.clone(),
            gateway: w.gateway.clone(),
            evidence: w.evidence.clone(),
            layouts: w.layouts.clone(),
            checks: w.checks.clone(),
            publisher: w.publisher.clone(),
            facts: None,
        })
        .unwrap(),
    );
    let queue = Arc::new(InMemoryTaskQueue::new());
    let id = enqueue_ticket(queue.as_ref(), &input(), 0)
        .await
        .unwrap()
        .id;
    let worker = Worker::new(
        "w",
        queue.clone(),
        slots,
        pipeline,
        Duration::from_secs(300),
    );
    for _ in 0..5 {
        let Tick::Ran(r) = worker.tick().await.unwrap() else {
            panic!("idle")
        };
        assert!(
            matches!(r.disposition, Disposition::Retrying { .. }),
            "{:?}",
            r.disposition
        );
        // Make the rescheduled task claimable again right away.
        queue.make_due(id);
    }
    let row = queue.get(id).unwrap();
    assert_eq!(row.state, TaskState::Queued);
    assert_eq!(
        row.attempts, 0,
        "budget waits are not infrastructure retries"
    );
}

/// A queue whose claim fails a few times (Postgres blip).
struct FlakyQueue {
    inner: InMemoryTaskQueue,
    failures: AtomicU32,
}

#[async_trait::async_trait]
impl TaskQueue for FlakyQueue {
    async fn enqueue(&self, t: &NewTask) -> Result<TaskEnqueued, StoreError> {
        self.inner.enqueue(t).await
    }
    async fn claim(&self, w: &str, l: Duration) -> Result<Option<ClaimedTask>, StoreError> {
        if self.failures.load(Ordering::SeqCst) > 0 {
            self.failures.fetch_sub(1, Ordering::SeqCst);
            return Err(StoreError::Unavailable("connection reset".into()));
        }
        self.inner.claim(w, l).await
    }
    async fn conflicts(&self, f: &[String]) -> Result<Vec<TaskId>, StoreError> {
        self.inner.conflicts(f).await
    }
    async fn heartbeat(&self, id: TaskId, w: &str, l: Duration) -> Result<(), StoreError> {
        self.inner.heartbeat(id, w, l).await
    }
    async fn complete(&self, id: TaskId, w: &str) -> Result<(), StoreError> {
        self.inner.complete(id, w).await
    }
    async fn fail(
        &self,
        id: TaskId,
        w: &str,
        e: &str,
        r: Option<Duration>,
    ) -> Result<(), StoreError> {
        self.inner.fail(id, w, e, r).await
    }
    async fn reschedule(
        &self,
        id: TaskId,
        w: &str,
        e: &str,
        after: Duration,
    ) -> Result<(), StoreError> {
        self.inner.reschedule(id, w, e, after).await
    }
}

#[tokio::test]
async fn the_worker_loop_survives_queue_errors() {
    let w = world(config(false, None), completed_runtime(1));
    let queue = Arc::new(FlakyQueue {
        inner: InMemoryTaskQueue::new(),
        failures: AtomicU32::new(2),
    });
    let worker = Worker::new(
        "w",
        queue.clone(),
        w.slots.clone(),
        w.pipeline.clone(),
        Duration::from_secs(300),
    );
    let orchestrator = Orchestrator {
        runs: Arc::new(MemoryRunLog::default()),
        facts: None,
        gateway: w.gateway.clone(),
        spend: None,
        evidence_base: None,
    };
    let mut errors = 0;
    let mut idles = 0;
    tokio::time::timeout(
        Duration::from_secs(10),
        run_loop(&worker, &orchestrator, Duration::from_millis(1), |s| {
            match s {
                LoopStep::Error(_) => errors += 1,
                LoopStep::Idle(_) => idles += 1,
                LoopStep::Ran(..) => {}
            }
            idles >= 1
        }),
    )
    .await
    .expect("loop stops")
    .expect("loop ok");
    assert_eq!(errors, 2);
}

// ------------------------------------------------------------ protected files

#[test]
fn protected_manifests_and_configs_are_denied_at_any_depth() {
    let sealed = SealedSpec::seal("e2e/tests/features/SHOP-1.spec.ts", b"x");
    let allowed = AllowedPaths::coder(&["**".into()], "e2e/tests", &sealed);
    let paths: Vec<String> = [
        "package.json",
        "apps/shop/package.json",
        "apps/shop/pnpm-lock.yaml",
        "e2e/playwright.config.ts",
        "apps/shop/.github/workflows/x.yml",
        "apps/shop/src/ok.ts",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let v = allowed.violations(&paths).unwrap();
    assert_eq!(v, paths[..5].to_vec());
}
