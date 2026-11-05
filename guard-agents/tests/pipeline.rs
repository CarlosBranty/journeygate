//! One ticket through QA → red-first → seal → Coder attempts → PR, with
//! fakes for every port and seam (qa-coder-protocol.md "Testing").

mod common;

use common::pipeline_fakes::*;
use guard_agents::pipeline::seams::RedFirst;
use guard_agents::pipeline::{
    AttemptOutcome, HumanLaneReason, PipelineError, StageOutcome, sealed_key,
};
use guard_agents::scheduler::SchedulerError;
use guard_core::ports::fakes::FakeAgentRuntime;
use guard_core::ports::*;

async fn seal_ok(w: &World) -> SealedSpec {
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let slot = w.slots.try_task_slot().unwrap();
    let qa = w.pipeline.run_qa(&input(), &slot).await.unwrap();
    match qa.outcome {
        StageOutcome::Sealed(s) => s,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn happy_path_seals_the_spec_and_opens_the_pr() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.coder_writes(
        "coder",
        1,
        app_diff("src/product-card.tsx", 3).with_marker(),
    );
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_ticket(&input(), &slot).await.unwrap();

    // QA: one attempt, spec sealed with the exact bytes, stored outside the sandbox.
    let sealed = match &report.qa.outcome {
        StageOutcome::Sealed(s) => s.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(sealed, SealedSpec::seal(SPEC_PATH, GOOD_SPEC));
    let (stored, _) = w.evidence.object(&sealed_key("run-1", "SHOP-101")).unwrap();
    assert_eq!(stored, GOOD_SPEC);
    assert_eq!(report.qa.attempts.len(), 1);
    assert_eq!(report.qa.attempts[0].outcome, Some(AttemptOutcome::Passed));
    assert_eq!(
        report.qa.attempts[0].sealed_spec_sha256.as_deref(),
        Some(sealed.sha256.as_str())
    );

    // Coder: green on attempt 1, PR on agent/<KEY> with the sealed spec overlaid.
    let coder = report.coder.as_ref().unwrap();
    match &coder.outcome {
        StageOutcome::Passed {
            pr,
            agent,
            changed_lines,
        } => {
            assert_eq!(pr.number, Some(7));
            assert_eq!(agent, "coder");
            assert_eq!(*changed_lines, 4);
        }
        other => panic!("{other:?}"),
    }
    let prs = w.publisher.requests.lock().unwrap().clone();
    assert_eq!(prs.len(), 1);
    assert_eq!(prs[0].branch, "agent/SHOP-101");
    assert_eq!(prs[0].sealed_spec, sealed);
    assert_eq!(prs[0].sealed_bytes, GOOD_SPEC);
    assert!(prs[0].title.starts_with("SHOP-101: "));
    assert!(prs[0].body.contains(&sealed.sha256));
    assert!(prs[0].body.to_lowercase().contains("human approval"));
    // What the real code-host publisher puts in its PR table.
    assert_eq!(prs[0].ticket, "SHOP-101");
    assert_eq!(prs[0].attempts, 1);
    assert!(prs[0].cost_usd > 0.0 && prs[0].cost_usd < report.total_cost_usd());
    let coder_desc = prs[0].coder.as_deref().unwrap();
    assert!(coder_desc.starts_with("coder ("), "{coder_desc}");
    assert!(!prs[0].summary.trim().is_empty());

    // The gate saw the coder diff and the verified sealed bytes.
    let gate = w.checks.gate_calls.lock().unwrap().clone();
    assert_eq!(gate.len(), 1);
    assert_eq!(gate[0].2, GOOD_SPEC);
    // The coder sandbox got the sealed copy from orchestrator storage.
    assert_eq!(w.layouts.sealed_copies.lock().unwrap()[0].1, GOOD_SPEC);

    // Every sandbox destroyed, every key revoked.
    assert!(w.sandbox.all_destroyed());
    assert!(!w.gateway.is_active("key-1") && !w.gateway.is_active("key-2"));
    assert!(report.total_cost_usd() > 0.49 && report.total_cost_usd() < 0.51);
}

trait Marker {
    fn with_marker(self) -> Self;
}
impl Marker for ExtractedDiff {
    fn with_marker(mut self) -> Self {
        self.unified_diff.push_str("+// FIX\n");
        self
    }
}

#[tokio::test]
async fn tasks_and_sandboxes_follow_the_contract() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox
        .coder_writes("coder", 1, app_diff("src/a.ts", 1).with_marker());
    let slot = w.slots.try_task_slot().unwrap();
    w.pipeline.run_ticket(&input(), &slot).await.unwrap();

    let tasks = w.runtime.tasks();
    assert_eq!(tasks.len(), 2);
    let (_, qa) = &tasks[0];
    assert_eq!(qa.role, Role::Qa);
    assert_eq!(qa.allowed_paths, AllowedPaths::qa("tests", "SHOP-101"));
    assert!(qa.sealed_spec.is_none());
    assert_eq!(qa.gateway.key_env, "GUARD_GATEWAY_KEY");
    assert_eq!(qa.gateway.base_url, "http://llm-gateway:4000");
    assert_eq!(qa.model, "anthropic/claude-sonnet-4-5");
    let (_, coder) = &tasks[1];
    assert_eq!(coder.role, Role::Coder);
    assert!(coder.allowed_paths.deny.contains(&"tests/**".to_string()));
    assert!(coder.allowed_paths.deny.contains(&SPEC_PATH.to_string()));
    assert_eq!(coder.allowed_paths.write, vec!["src/**".to_string()]);
    assert_eq!(coder.attempt_policy.max_attempts(), 3);
    // No credential ever appears in task.json.
    for (_, t) in &tasks {
        let json = serde_json::to_string(t).unwrap();
        assert!(!json.contains("sk-fake"), "{json}");
    }

    let specs = w.sandbox.specs();
    assert_eq!(specs.len(), 2);
    let (_, qa_spec) = &specs[0];
    assert_eq!(qa_spec.role, Role::Qa);
    assert!(
        qa_spec
            .egress
            .rules
            .iter()
            .any(|r| r.purpose == EgressPurpose::Preview && r.host == "preview-shop-101")
    );
    assert_eq!(
        qa_spec.secrets.get("GUARD_GATEWAY_KEY").map(|s| s.expose()),
        Some("sk-fake-1")
    );
    let (_, coder_spec) = &specs[1];
    assert!(
        coder_spec
            .egress
            .rules
            .iter()
            .all(|r| r.purpose != EgressPurpose::Preview)
    );
    assert_eq!(coder_spec.limits.timeout.as_secs(), 1800);
    assert_eq!(
        coder_spec.layout.skills_dir.as_deref(),
        Some("/host/.guard/agents/rendered/fake")
    );
}

#[tokio::test]
async fn keys_carry_alias_models_budget_and_metadata() {
    // Hold the coder sandbox open to inspect the live key: a runtime error
    // happens after minting, and the key must still be revoked.
    let w = world(
        config(false, None),
        FakeAgentRuntime::new()
            .with_outcome(Ok(FakeAgentRuntime::completed("qa", 0.1)))
            .with_outcome(Err(RuntimeError::Crashed {
                exit_code: Some(1),
                stderr_tail: "boom".into(),
            })),
    );
    let sealed = seal_ok(&w).await;
    let slot = w.slots.try_task_slot().unwrap();
    let _ = w.pipeline.run_coder(&input(), &sealed, &slot).await;
    let report = w.pipeline.minted_key_requests();
    assert_eq!(report[0].alias, "run-1:qa:1");
    assert_eq!(report[1].alias, "run-1:coder:1");
    assert_eq!(report[1].models, vec!["openai/gpt-5".to_string()]);
    assert_eq!(report[1].max_budget_usd, 5.0);
    assert_eq!(report[1].rpm_limit, Some(60));
    assert_eq!(report[1].metadata["ticket"], "SHOP-101");
    assert_eq!(report[1].metadata["role"], "coder");
    assert_eq!(report[1].metadata["commit"], BASE);
    for i in 1..=4 {
        assert!(!w.gateway.is_active(&format!("key-{i}")), "key-{i} revoked");
    }
}

#[tokio::test]
async fn red_first_pass_goes_back_to_qa_then_human_lane() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, GOOD_SPEC);
    w.sandbox.qa_writes(2, GOOD_SPEC);
    w.checks
        .red_first
        .lock()
        .unwrap()
        .extend([RedFirst::Passed, RedFirst::Passed]);
    let slot = w.slots.try_task_slot().unwrap();
    let qa = w.pipeline.run_qa(&input(), &slot).await.unwrap();
    assert_eq!(
        qa.attempts.iter().map(|a| a.outcome).collect::<Vec<_>>(),
        vec![Some(AttemptOutcome::RedFirstPassed); 2]
    );
    assert!(matches!(
        qa.outcome,
        StageOutcome::HumanLane(HumanLaneReason::AttemptsExhausted { role: Role::Qa, .. })
    ));
    assert!(w.evidence.keys().is_empty(), "nothing sealed");
    // Attempt 2 is told why attempt 1 was rejected.
    let ctx = w.runtime.tasks()[1].1.failure_context.clone().unwrap();
    assert!(ctx.contains("passed on the base commit"), "{ctx}");
}

#[tokio::test]
async fn quality_rejection_retries_qa() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.qa_writes(1, b"test.only('x', async () => {})");
    w.sandbox.qa_writes(2, GOOD_SPEC);
    w.checks
        .quality
        .lock()
        .unwrap()
        .push_back(vec!["no_focus".into(), "has_expect".into()]);
    let slot = w.slots.try_task_slot().unwrap();
    let qa = w.pipeline.run_qa(&input(), &slot).await.unwrap();
    assert_eq!(
        qa.attempts[0].outcome,
        Some(AttemptOutcome::QualityRejected)
    );
    assert!(
        qa.attempts[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("no_focus")
    );
    assert!(matches!(qa.outcome, StageOutcome::Sealed(_)));
    assert_eq!(
        *w.checks.red_first_calls.lock().unwrap(),
        1,
        "only the good spec"
    );
}

#[tokio::test]
async fn qa_diff_outside_the_spec_is_rejected() {
    let w = world(config(false, None), completed_runtime(2));
    w.sandbox.script(
        "qa",
        Role::Qa,
        1,
        Script {
            diff: Some(diff(&[SPEC_PATH, "src/cheat.ts"], "")),
            files: vec![(format!("/workspace/{SPEC_PATH}"), GOOD_SPEC.to_vec())],
        },
    );
    w.sandbox.qa_writes(2, GOOD_SPEC);
    let slot = w.slots.try_task_slot().unwrap();
    let qa = w.pipeline.run_qa(&input(), &slot).await.unwrap();
    assert_eq!(qa.attempts[0].outcome, Some(AttemptOutcome::DiffRejected));
    assert!(
        qa.attempts[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("src/cheat.ts")
    );
    assert!(matches!(qa.outcome, StageOutcome::Sealed(_)));
}

#[tokio::test]
async fn coder_diff_touching_tests_is_rejected_and_counts_as_an_attempt() {
    let w = world(config(false, None), completed_runtime(3));
    let sealed = seal_ok(&w).await;
    w.sandbox.coder_writes(
        "coder",
        1,
        diff(
            &["src/a.ts", "tests/journeys/checkout.spec.ts"],
            "+// FIX\n",
        ),
    );
    w.sandbox
        .coder_writes("coder", 2, app_diff("src/a.ts", 2).with_marker());
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert_eq!(
        report.attempts[0].outcome,
        Some(AttemptOutcome::DiffRejected)
    );
    assert_eq!(report.attempts[1].outcome, Some(AttemptOutcome::Passed));
    assert_eq!(report.attempts[1].attempt, 2);
    assert_eq!(
        w.checks.gate_calls.lock().unwrap().len(),
        1,
        "rejected diff never gated"
    );
}

#[tokio::test]
async fn third_failed_attempt_goes_to_the_human_lane() {
    let w = world(config(false, None), completed_runtime(4));
    // A fact in memory: the brief is part of the deterministic prefix.
    use guard_core::ports::MemoryFactsWriter;
    w.facts
        .record_fact(&NewFact {
            repo: "acme/shop".into(),
            scope: FactScope::Repo,
            scope_key: None,
            topic_key: "catalog/stock-badge".into(),
            body: "Stock badge reads product.stock".into(),
            commit_sha: BASE.into(),
            run_id: None,
            evidence_uri: None,
            source: FactSource::GateResult,
            valid_from: "2026-10-01T00:00:00Z".into(),
            review_after: None,
            referenced_paths: vec![],
        })
        .await
        .unwrap();
    let sealed = seal_ok(&w).await;
    for n in 1..=3 {
        w.sandbox
            .coder_writes("coder", n, app_diff("src/a.ts", n as usize));
    }
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert_eq!(report.attempts.len(), 3);
    assert!(
        report
            .attempts
            .iter()
            .all(|a| a.outcome == Some(AttemptOutcome::GateFailed))
    );
    assert!(matches!(
        report.outcome,
        StageOutcome::HumanLane(HumanLaneReason::AttemptsExhausted {
            role: Role::Coder,
            last_outcome: Some(AttemptOutcome::GateFailed)
        })
    ));
    assert!(w.publisher.requests.lock().unwrap().is_empty());

    let coder_tasks: Vec<AgentTask> = w
        .runtime
        .tasks()
        .into_iter()
        .map(|(_, t)| t)
        .filter(|t| t.role == Role::Coder)
        .collect();
    assert_eq!(coder_tasks[0].failure_context, None);
    for t in &coder_tasks[1..] {
        let ctx = t.failure_context.as_deref().unwrap();
        assert!(ctx.contains("expected badge"), "{ctx}");
    }
    // Deterministic prefix: tools and facts brief identical across attempts.
    let brief = coder_tasks[0]
        .facts_brief
        .clone()
        .expect("brief from memory");
    assert!(brief.text.contains("product.stock"));
    for t in &coder_tasks {
        assert_eq!(t.tools, coder_tasks[0].tools);
        assert_eq!(t.facts_brief.as_ref(), Some(&brief));
    }
}

#[tokio::test]
async fn tampered_sealed_copy_aborts_before_the_gate() {
    let w = world(config(false, None), completed_runtime(2));
    let sealed = seal_ok(&w).await;
    use guard_core::ports::EvidenceStore;
    w.evidence
        .put_object(
            &sealed_key("run-1", "SHOP-101"),
            b"test('x', () => {})".to_vec(),
            "text/plain",
        )
        .await
        .unwrap();
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert!(matches!(
        report.outcome,
        StageOutcome::HumanLane(HumanLaneReason::SealTampered { .. })
    ));
    assert!(w.checks.gate_calls.lock().unwrap().is_empty());
    assert_eq!(w.runtime.tasks().len(), 1, "no coder sandbox started");
}

#[tokio::test]
async fn budget_exhaustion_goes_straight_to_the_human_lane() {
    let w = world(
        config(false, None),
        FakeAgentRuntime::new()
            .with_outcome(Ok(FakeAgentRuntime::completed("qa", 0.1)))
            .with_outcome(Ok(status_outcome(TaskStatus::BudgetExceeded, 5.0))),
    );
    let sealed = seal_ok(&w).await;
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert_eq!(report.attempts.len(), 1);
    assert_eq!(report.attempts[0].status, Some(TaskStatus::BudgetExceeded));
    assert!(matches!(
        report.outcome,
        StageOutcome::HumanLane(HumanLaneReason::BudgetExceeded { .. })
    ));
}

#[tokio::test]
async fn runtime_errors_are_recorded_and_retried() {
    let w = world(
        config(false, None),
        FakeAgentRuntime::new()
            .with_outcome(Ok(FakeAgentRuntime::completed("qa", 0.1)))
            .with_outcome(Err(RuntimeError::Protocol("no result.json".into())))
            .with_outcome(Ok(FakeAgentRuntime::completed("fixed", 0.2))),
    );
    let sealed = seal_ok(&w).await;
    w.sandbox
        .coder_writes("coder", 2, app_diff("src/a.ts", 1).with_marker());
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert_eq!(
        report.attempts[0].outcome,
        Some(AttemptOutcome::RuntimeError)
    );
    assert!(
        report.attempts[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("result.json")
    );
    assert!(matches!(report.outcome, StageOutcome::Passed { .. }));
    assert!(w.sandbox.all_destroyed());
}

#[tokio::test]
async fn diff_scanner_findings_go_to_the_human_lane() {
    let w = world(config(false, None), completed_runtime(2));
    let sealed = seal_ok(&w).await;
    w.sandbox
        .coder_writes("coder", 1, app_diff("src/a.ts", 1).with_marker());
    w.checks
        .scan_findings
        .lock()
        .unwrap()
        .push("navigator.webdriver in src/a.ts".into());
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    match report.outcome {
        StageOutcome::HumanLane(HumanLaneReason::DiffScanner { findings }) => {
            assert_eq!(findings.len(), 1)
        }
        other => panic!("{other:?}"),
    }
    assert!(w.checks.gate_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn best_of_n_publishes_only_the_smallest_green_diff() {
    let w = world(config(true, Some("agent-best-of-n")), completed_runtime(3));
    let sealed = seal_ok(&w).await;
    w.sandbox
        .coder_writes("coder", 1, app_diff("src/a.ts", 9).with_marker());
    w.sandbox
        .coder_writes("coder-b", 1, app_diff("src/a.ts", 2).with_marker());
    let mut inp = input();
    inp.labels = vec!["agent-best-of-n".into()];
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_coder(&inp, &sealed, &slot).await.unwrap();
    match &report.outcome {
        StageOutcome::Passed {
            agent,
            changed_lines,
            ..
        } => {
            assert_eq!(agent, "coder-b");
            assert_eq!(*changed_lines, 3);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(report.attempts.len(), 2, "one attempt per candidate");
    let prs = w.publisher.requests.lock().unwrap().clone();
    assert_eq!(prs.len(), 1);
    assert!(prs[0].diff.contains("line 1") && !prs[0].diff.contains("line 8"));
    // Distinct key aliases per candidate.
    let aliases: Vec<String> = w
        .pipeline
        .minted_key_requests()
        .into_iter()
        .map(|k| k.alias)
        .collect();
    assert!(aliases.contains(&"run-1:coder:1:coder".to_string()));
    assert!(aliases.contains(&"run-1:coder:1:coder-b".to_string()));
    assert!(w.sandbox.all_destroyed());
    assert_eq!(
        w.slots.free_host_slots(),
        3,
        "extra candidate slot released"
    );
}

#[tokio::test]
async fn best_of_n_needs_the_label() {
    let w = world(config(true, Some("agent-best-of-n")), completed_runtime(2));
    let sealed = seal_ok(&w).await;
    w.sandbox
        .coder_writes("coder", 1, app_diff("src/a.ts", 1).with_marker());
    let slot = w.slots.try_task_slot().unwrap();
    let report = w
        .pipeline
        .run_coder(&input(), &sealed, &slot)
        .await
        .unwrap();
    assert_eq!(report.attempts.len(), 1);
    assert_eq!(report.attempts[0].agent, "coder");
}

#[tokio::test]
async fn daily_cap_stops_before_minting_a_key() {
    let w = world(config(false, None), completed_runtime(1));
    w.spend.set(50.0);
    let slot = w.slots.try_task_slot().unwrap();
    let err = w.pipeline.run_qa(&input(), &slot).await.unwrap_err();
    assert!(matches!(
        err,
        PipelineError::Scheduler(SchedulerError::DailyCapReached { .. })
    ));
    assert!(err.retry_after(0).is_some());
    assert!(w.pipeline.minted_key_requests().is_empty());
    assert!(w.runtime.tasks().is_empty());
}

#[tokio::test]
async fn memory_proposals_are_returned_never_written() {
    let mut qa = FakeAgentRuntime::completed("qa", 0.1);
    qa.result.memory_proposals = vec![MemoryProposal {
        topic_key: "catalog/badge".into(),
        body: "badge uses stock".into(),
        evidence: None,
    }];
    let w = world(
        config(false, None),
        FakeAgentRuntime::new().with_outcome(Ok(qa)),
    );
    w.sandbox.qa_writes(1, GOOD_SPEC);
    let slot = w.slots.try_task_slot().unwrap();
    let report = w.pipeline.run_qa(&input(), &slot).await.unwrap();
    assert_eq!(report.memory_proposals.len(), 1);
    assert_eq!(report.memory_proposals[0].proposed_by, "fake:qa");
    use guard_core::ports::MemoryFactsWriter;
    assert!(
        w.facts
            .list_candidates("acme/shop")
            .await
            .unwrap()
            .is_empty()
    );
    use guard_core::ports::MemoryFactsReader;
    let page = w
        .facts
        .list_facts(&FactsQuery::for_repo("acme/shop"))
        .await
        .unwrap();
    assert!(page.facts.is_empty());
}

#[tokio::test]
async fn unknown_runtime_is_a_construction_error() {
    let w = world(config(false, None), FakeAgentRuntime::new());
    let deps = guard_agents::pipeline::PipelineDeps {
        config: std::sync::Arc::new(config(false, None)),
        slots: w.slots.clone(),
        runtimes: vec![],
        sandbox: w.sandbox.clone(),
        gateway: w.gateway.clone(),
        evidence: w.evidence.clone(),
        layouts: w.layouts.clone(),
        checks: w.checks.clone(),
        publisher: w.publisher.clone(),
        facts: None,
    };
    match guard_agents::pipeline::TicketPipeline::new(deps) {
        Err(PipelineError::Config(m)) => assert!(m.contains("fake"), "{m}"),
        Err(e) => panic!("{e}"),
        Ok(_) => panic!("accepted a config whose runtime is missing"),
    }
}
