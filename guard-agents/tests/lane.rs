//! The orchestrator after each stage: agent_runs rows, spend ingestion,
//! proposals into the candidate queue, orchestrator-only memory facts.

mod common;

use common::pipeline_fakes::*;
use guard_agents::lane::{LoopStep, Orchestrator, step};
use guard_agents::runs::{MemoryRunLog, now_unix, rfc3339};
use guard_agents::worker::{Disposition, Worker, enqueue_ticket};
use guard_core::ports::fakes::{FakeAgentRuntime, InMemoryTaskQueue};
use guard_core::ports::*;
use guard_store::spend::MemorySpendSink;
use std::sync::Arc;
use std::time::Duration;

fn spend(alias: &str, id: &str, usd: f64) -> SpendRecord {
    let now = rfc3339(now_unix());
    SpendRecord {
        request_id: id.into(),
        key_alias: Some(alias.into()),
        model: "anthropic/claude-sonnet-4-5".into(),
        spend_usd: usd,
        prompt_tokens: 1200,
        completion_tokens: 80,
        cached_tokens: 1000,
        cache_creation_input_tokens: 0,
        cache_hit: None,
        start_time: now.clone(),
        end_time: now,
        metadata: Default::default(),
    }
}

#[tokio::test]
async fn the_orchestrator_records_runs_spend_candidates_and_its_own_facts() {
    let mut proposing = FakeAgentRuntime::completed("done", 0.25);
    proposing.result.memory_proposals = vec![MemoryProposal {
        topic_key: "hint/stock".into(),
        body: "Stock text lives in products/routes.mjs".into(),
        evidence: None,
    }];
    let runtime = FakeAgentRuntime::new()
        .with_outcome(Ok(proposing))
        .with_outcome(Ok(FakeAgentRuntime::completed("fixed", 0.5)));
    let w = world(config(false, None), runtime);
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes("coder", 1, {
        let mut d = app_diff("src/product-card.tsx", 2);
        d.unified_diff.push_str("+// FIX\n");
        d
    });
    w.gateway.push_spend(spend("run-1:qa:1", "req-qa", 0.01));
    w.gateway
        .push_spend(spend("run-1:coder:1", "req-coder", 0.02));
    let queue = Arc::new(InMemoryTaskQueue::new());
    enqueue_ticket(queue.as_ref(), &input(), 0).await.unwrap();
    let worker = Worker::new(
        "h:1",
        queue,
        w.slots.clone(),
        w.pipeline.clone(),
        Duration::from_secs(60),
    );
    let runs = Arc::new(MemoryRunLog::new());
    let sink = Arc::new(MemorySpendSink::default());
    let orchestrator = Orchestrator {
        runs: runs.clone(),
        facts: Some(w.facts.clone()),
        gateway: w.gateway.clone(),
        spend: Some(sink.clone()),
        evidence_base: Some("s3://guard-evidence".into()),
    };

    let LoopStep::Ran(qa, after) = step(&worker, &orchestrator).await.unwrap() else {
        panic!("qa ran")
    };
    assert!(matches!(qa.disposition, Disposition::Sealed { .. }));
    assert_eq!(after.runs_recorded, 1);
    assert_eq!(after.spend_records, 1);
    assert_eq!(after.candidates_enqueued, 1);
    assert_eq!(after.facts_recorded.len(), 1, "{after:?}");

    let LoopStep::Ran(coder, after) = step(&worker, &orchestrator).await.unwrap() else {
        panic!("coder ran")
    };
    assert!(matches!(coder.disposition, Disposition::Passed { .. }));
    assert_eq!(after.spend_records, 1);
    assert_eq!(after.facts_recorded.len(), 1);

    let rows = runs.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1.role, Role::Qa);
    assert_eq!(rows[1].1.role, Role::Coder);
    assert_eq!(rows[1].0.task_id, Some(coder.task));
    assert_eq!(sink.len(), 2);

    // Facts: only the orchestrator's observations; the proposal is a candidate.
    let reader = w.facts.reader();
    let page = reader
        .list_facts(&FactsQuery::for_repo("acme/shop"))
        .await
        .unwrap();
    let topics: Vec<&str> = page.facts.iter().map(|f| f.topic_key.as_str()).collect();
    assert_eq!(
        topics,
        vec!["ticket/shop-101/sealed-spec", "ticket/shop-101/agent-pr"]
    );
    let sealed = &page.facts[0];
    assert!(
        sealed.body.contains("fails with an assertion"),
        "{}",
        sealed.body
    );
    assert_eq!(
        sealed.evidence_uri.as_deref(),
        Some("s3://guard-evidence/sealed/run-1/SHOP-101.spec.ts")
    );
    assert!(page.facts[1].body.contains("human approval"));
    let candidates = w.facts.list_candidates("acme/shop").await.unwrap();
    assert_eq!(candidates.len(), 1);
}
