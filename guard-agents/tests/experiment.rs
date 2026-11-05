//! A/B arms (tokens.md "4-arm A/B protocol") and the per-attempt metrics the
//! `agent_runs` rows need (steps, memory reads, brief fact ids, usage).

mod common;

use common::pipeline_fakes::*;
use guard_agents::pipeline::StageOutcome;
use guard_agents::pipeline::seams::ExperimentArm;
use guard_core::ports::fakes::FakeAgentRuntime;
use guard_core::ports::*;

async fn seed_fact(w: &World) -> FactId {
    w.facts
        .record_fact(&NewFact {
            repo: "acme/shop".into(),
            scope: FactScope::Repo,
            scope_key: None,
            topic_key: "failure/x".into(),
            body: "x failed".into(),
            commit_sha: BASE.into(),
            run_id: None,
            evidence_uri: None,
            source: FactSource::GateResult,
            valid_from: "2026-10-01T00:00:00Z".into(),
            review_after: None,
            referenced_paths: vec!["src/product-card.tsx".into()],
        })
        .await
        .unwrap()
}

fn fixed() -> ExtractedDiff {
    let mut d = app_diff("src/product-card.tsx", 2);
    d.unified_diff.push_str(
        "+// FIX
",
    );
    d
}

fn arm(arm: &str, memory: bool, map: bool) -> ExperimentArm {
    ExperimentArm {
        alias: format!("exp1:{arm}:SHOP-101:r1"),
        arm: arm.into(),
        memory,
        map,
    }
}

fn tool_runtime() -> FakeAgentRuntime {
    let mut o = FakeAgentRuntime::completed("done", 0.25);
    o.result.steps = 4;
    o.events.push(RuntimeEvent {
        seq: 2,
        ts: "2026-11-01T00:00:01Z".into(),
        event: RuntimeEventKind::ToolCall {
            tool: "guard_history".into(),
            ok: true,
            duration_ms: 3,
        },
    });
    FakeAgentRuntime::new()
        .with_outcome(Ok(o.clone()))
        .with_outcome(Ok(o))
}

#[tokio::test]
async fn an_arm_uses_one_alias_and_toggles_memory_and_map() {
    let w = world(config(false, None), tool_runtime());
    let fact = seed_fact(&w).await;
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes("coder", 1, fixed());
    let mut input = input();
    input.experiment = Some(arm("map", false, true));
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_ticket(&input, &slot).await.unwrap();
    assert!(matches!(
        report.coder.as_ref().unwrap().outcome,
        StageOutcome::Passed { .. }
    ));
    let tasks: Vec<AgentTask> = w.runtime.tasks().into_iter().map(|(_, t)| t).collect();
    assert_eq!(tasks.len(), 2);
    for t in &tasks {
        assert!(t.facts_brief.is_none(), "memory arm off: no brief");
        assert!(
            !t.tools.iter().any(|x| x == "guard_history"),
            "{:?}",
            t.tools
        );
        assert!(t.tools.iter().any(|x| x == "guard_map"), "{:?}", t.tools);
    }
    let aliases: Vec<String> = w
        .pipeline
        .minted_key_requests()
        .iter()
        .map(|k| k.alias.clone())
        .collect();
    assert_eq!(aliases, vec!["exp1:map:SHOP-101:r1"; 2]);
    let all: Vec<_> = report
        .qa
        .attempts
        .iter()
        .chain(&report.coder.as_ref().unwrap().attempts)
        .collect();
    for a in all {
        assert_eq!(a.arm.as_deref(), Some("map"));
        assert_eq!(a.steps, Some(4));
        assert_eq!(a.memory_reads, 1);
        assert!(a.facts_brief_ids.is_empty());
    }
    let _ = fact;
}

#[tokio::test]
async fn the_memory_arm_injects_the_brief_and_records_its_fact_ids() {
    let w = world(config(false, None), tool_runtime());
    let fact = seed_fact(&w).await;
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes("coder", 1, fixed());
    let mut input = input();
    input.experiment = Some(arm("memory", true, false));
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_ticket(&input, &slot).await.unwrap();
    for (_, t) in w.runtime.tasks() {
        assert_eq!(t.facts_brief.as_ref().unwrap().fact_ids, vec![fact]);
        assert!(!t.tools.iter().any(|x| x == "guard_map"));
        assert!(t.tools.iter().any(|x| x == "guard_history"));
    }
    let coder = report.coder.unwrap();
    assert_eq!(coder.attempts[0].facts_brief_ids, vec![fact.0]);
    assert_eq!(
        coder.attempts[0].usage,
        Some(FakeAgentRuntime::completed("done", 0.25).result.usage)
    );
}

#[tokio::test]
async fn without_an_experiment_the_default_tools_and_brief_apply() {
    let w = world(config(false, None), completed_runtime(2));
    let fact = seed_fact(&w).await;
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes("coder", 1, fixed());
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_ticket(&input(), &slot).await.unwrap();
    let tasks: Vec<AgentTask> = w.runtime.tasks().into_iter().map(|(_, t)| t).collect();
    assert_eq!(tasks[0].tools, guard_agents::pipeline::QA_TOOLS);
    assert_eq!(tasks[1].tools, guard_agents::pipeline::CODER_TOOLS);
    assert_eq!(tasks[1].facts_brief.as_ref().unwrap().fact_ids, vec![fact]);
    assert_eq!(report.qa.attempts[0].arm, None);
    assert_eq!(report.qa.attempts[0].key_alias, "run-1:qa:1");
}

#[tokio::test]
async fn runtime_secret_aliases_carry_the_same_virtual_key() {
    let aliases: guard_agents::pipeline::SecretAliases = std::sync::Arc::new(|runtime, task| {
        assert_eq!(runtime, "fake");
        assert_eq!(task.gateway.key_env, "GUARD_GATEWAY_KEY");
        vec!["LITELLM_PROXY_API_KEY".to_string()]
    });
    let w = world_with(config(false, None), completed_runtime(1), Some(aliases));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let slot = w.slots.try_task_slot().unwrap();
    w.pipeline.run_qa(&input(), &slot).await.unwrap();
    let (_, spec) = &w.sandbox.specs()[0];
    let key = spec
        .secrets
        .get("GUARD_GATEWAY_KEY")
        .unwrap()
        .expose()
        .to_string();
    assert_eq!(
        spec.secrets.get("LITELLM_PROXY_API_KEY").unwrap().expose(),
        key
    );
}
