//! Contract tests of the Phase 4 ports (agent lane, sandbox, gateway, task
//! queue, memory facts): the pure invariants in `ports/` and the fakes.

use super::*;
use crate::ports::agent::*;
use crate::ports::memory_facts::*;
use crate::ports::model_gateway::*;
use crate::ports::sandbox::*;
use crate::ports::secret::Secret;
use crate::ports::task_queue::*;
use std::sync::Arc;
use std::time::Duration;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

// ---------------------------------------------------------------- sealing

#[test]
fn sealed_spec_hashes_with_sha256_and_verifies() {
    let spec = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"abc");
    assert_eq!(
        spec.sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(spec.verify(b"abc"));
    assert!(!spec.verify(b"abd"));
    assert!(SealedSpec::is_valid_digest(&spec.sha256));
    assert!(!SealedSpec::is_valid_digest("ABC"));
}

// ---------------------------------------------------------------- attempts

#[test]
fn attempt_policy_caps_at_three() {
    assert_eq!(AttemptPolicy::default().max_attempts(), MAX_ATTEMPTS);
    assert!(AttemptPolicy::new(0).is_err());
    assert!(AttemptPolicy::new(4).is_err());
    let p = AttemptPolicy::new(2).unwrap();
    assert!(p.allows(1) && p.allows(2));
    assert!(!p.allows(0) && !p.allows(3));
    let err = serde_json::from_str::<AttemptPolicy>(r#"{"max_attempts": 5}"#);
    assert!(err.is_err(), "deserialization enforces the cap");
    let ok: AttemptPolicy = serde_json::from_str(r#"{"max_attempts": 3}"#).unwrap();
    assert_eq!(ok.max_attempts(), 3);
}

// ---------------------------------------------------------------- paths

#[test]
fn coder_paths_deny_tests_sealed_spec_and_manifests() {
    let sealed = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"x");
    let paths = AllowedPaths::coder(&["src/**".into()], "tests", &sealed);
    let v = paths
        .violations(&[
            "src/cart/badge.ts".into(),
            "tests/journeys/checkout.spec.ts".into(),
            "tests/features/SHOP-1.spec.ts".into(),
            "package.json".into(),
            "README.md".into(),
            "../etc/passwd".into(),
            "/abs/path".into(),
        ])
        .unwrap();
    assert_eq!(
        v,
        vec![
            "tests/journeys/checkout.spec.ts",
            "tests/features/SHOP-1.spec.ts",
            "package.json",
            "README.md",
            "../etc/passwd",
            "/abs/path",
        ]
    );
}

#[test]
fn qa_paths_allow_only_the_ticket_spec() {
    let paths = AllowedPaths::qa("tests", "SHOP-1");
    assert_eq!(paths.write, vec!["tests/features/SHOP-1.spec.ts"]);
    let v = paths
        .violations(&[
            "tests/features/SHOP-1.spec.ts".into(),
            "tests/features/SHOP-2.spec.ts".into(),
            "src/a.ts".into(),
        ])
        .unwrap();
    assert_eq!(v, vec!["tests/features/SHOP-2.spec.ts", "src/a.ts"]);
}

#[test]
fn invalid_glob_is_an_error() {
    let paths = AllowedPaths {
        write: vec!["src/[".into()],
        deny: vec![],
    };
    assert!(paths.violations(&["src/a".into()]).is_err());
}

// ---------------------------------------------------------------- task

fn coder_task() -> AgentTask {
    let sealed = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"spec");
    AgentTask {
        version: TASK_SCHEMA_VERSION,
        run_id: "run-1".into(),
        ticket: "SHOP-1".into(),
        role: Role::Coder,
        attempt: 1,
        attempt_policy: AttemptPolicy::default(),
        repo: "acme/shop".into(),
        base_commit: SHA.into(),
        summary: "Fix stock badge".into(),
        acceptance_criteria: vec!["Badge shows out of stock".into()],
        allowed_paths: AllowedPaths::coder(&["src/**".into()], "tests", &sealed),
        sealed_spec: Some(sealed),
        model: "anthropic/claude-sonnet-4-5".into(),
        budget_usd: 5.0,
        timeout_secs: 1800,
        gateway: GatewayBinding {
            base_url: "http://llm-gateway:4000".into(),
            key_env: "GUARD_GATEWAY_KEY".into(),
            via_gateway: true,
        },
        tools: vec!["guard_test".into(), "guard_history".into()],
        facts_brief: None,
        failure_context: None,
    }
}

#[test]
fn agent_task_validation() {
    assert_eq!(coder_task().validate(), Ok(()));

    let mut t = coder_task();
    t.sealed_spec = None;
    assert!(t.validate().unwrap_err().contains("sealed_spec"));

    let mut t = coder_task();
    t.attempt = 4;
    assert!(t.validate().unwrap_err().contains("attempt"));

    let mut t = coder_task();
    t.budget_usd = f64::NAN;
    assert!(t.validate().unwrap_err().contains("budget_usd"));

    let mut t = coder_task();
    t.allowed_paths.deny.clear();
    assert!(t.validate().unwrap_err().contains("sealed spec"));

    let mut t = coder_task();
    t.role = Role::Qa;
    assert!(t.validate().unwrap_err().contains("sealed_spec"));

    let mut t = coder_task();
    t.gateway.key_env = "sk-live-123".into();
    assert!(t.validate().unwrap_err().contains("key_env"));
}

#[test]
fn task_json_example_round_trips() {
    let raw = include_str!("../../../../schemas/examples/agent-task.example.json");
    let task: AgentTask = serde_json::from_str(raw).expect("example parses as AgentTask");
    assert_eq!(task.validate(), Ok(()));
    let back = serde_json::to_value(&task).unwrap();
    let orig: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(back, orig);
}

#[test]
fn result_and_events_examples_round_trip() {
    let raw = include_str!("../../../../schemas/examples/agent-result.example.json");
    let result: TaskResult = serde_json::from_str(raw).unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    let orig: serde_json::Value = serde_json::from_str(raw).unwrap();
    assert_eq!(serde_json::to_value(&result).unwrap(), orig);

    let raw = include_str!("../../../../schemas/examples/agent-events.example.jsonl");
    let events: Vec<RuntimeEvent> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("event line parses"))
        .collect();
    assert!(events.len() >= 3);
    for (line, event) in raw.lines().filter(|l| !l.trim().is_empty()).zip(&events) {
        let orig: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), orig);
    }
    let usage = Usage::sum(events.iter().filter_map(RuntimeEvent::usage));
    assert!(usage.input_tokens > 0);
}

// ---------------------------------------------------------------- sandbox

fn coder_spec() -> SandboxSpec {
    SandboxSpec::for_role(
        Role::Coder,
        SandboxLayout {
            run_id: "run-1".into(),
            ticket: "SHOP-1".into(),
            attempt: 1,
            image: format!("ghcr.io/acme/guard-sandbox@sha256:{}", "a".repeat(64)),
            checkout: "/srv/runs/run-1/worktree".into(),
            task_dir: "/srv/runs/run-1/task".into(),
            codegraph_index: Some("/srv/runs/run-1/codegraph".into()),
            sealed_spec: Some((
                "/srv/sealed/SHOP-1.spec.ts".into(),
                "tests/features/SHOP-1.spec.ts".into(),
            )),
            tests_dir: "tests".into(),
            skills_dir: Some("/srv/agents/rendered/claude-code".into()),
        },
        EgressAllowlist::gateway_only("llm-gateway", 4000),
    )
}

#[test]
fn role_mounts_follow_the_permission_matrix() {
    let coder = coder_spec();
    assert_eq!(coder.validate(), Ok(()));
    let mode = |spec: &SandboxSpec, target: &str| {
        spec.mounts
            .iter()
            .find(|m| m.target == target)
            .map(|m| m.mode)
    };
    assert_eq!(mode(&coder, WORKSPACE_DIR), Some(MountMode::ReadWrite));
    assert_eq!(mode(&coder, "/workspace/tests"), Some(MountMode::Hidden));
    assert_eq!(
        mode(&coder, "/workspace/tests/features/SHOP-1.spec.ts"),
        Some(MountMode::ReadOnly)
    );
    assert_eq!(mode(&coder, CODEGRAPH_DIR), Some(MountMode::ReadOnly));
    assert_eq!(
        coder.env.get("CODEGRAPH_TELEMETRY").map(String::as_str),
        Some("0")
    );
    assert_eq!(coder.env.get("DO_NOT_TRACK").map(String::as_str), Some("1"));

    let mut qa_layout = coder_spec().layout;
    qa_layout.sealed_spec = None;
    let qa = SandboxSpec::for_role(
        Role::Qa,
        qa_layout,
        EgressAllowlist::gateway_only("llm-gateway", 4000).with(EgressRule {
            host: "preview.internal".into(),
            port: 443,
            purpose: EgressPurpose::Preview,
        }),
    );
    assert_eq!(qa.validate(), Ok(()));
    assert_eq!(mode(&qa, WORKSPACE_DIR), Some(MountMode::ReadOnly));
    assert_eq!(
        mode(&qa, "/workspace/tests/features"),
        Some(MountMode::ReadWrite)
    );
}

#[test]
fn sandbox_spec_rejects_unsafe_configurations() {
    let mut s = coder_spec();
    s.mounts.retain(|m| m.target != "/workspace/tests");
    assert!(s.validate().unwrap_err().contains("tests"));

    let mut s = coder_spec();
    for m in &mut s.mounts {
        if m.target == CODEGRAPH_DIR {
            m.mode = MountMode::ReadWrite;
        }
    }
    assert!(s.validate().unwrap_err().contains("codegraph"));

    let mut s = coder_spec();
    s.env.remove("DO_NOT_TRACK");
    assert!(s.validate().unwrap_err().contains("DO_NOT_TRACK"));

    let mut s = coder_spec();
    s.egress = s.egress.with(EgressRule {
        host: "api.anthropic.com".into(),
        port: 443,
        purpose: EgressPurpose::Gateway,
    });
    assert!(s.validate().unwrap_err().contains("gateway"));

    let mut s = coder_spec();
    s.egress = s.egress.with(EgressRule {
        host: "api.githubcopilot.com".into(),
        port: 443,
        purpose: EgressPurpose::CopilotException,
    });
    assert!(s.validate().unwrap_err().contains("copilot"));
    s.copilot_exception = true;
    assert_eq!(s.validate(), Ok(()));

    let mut s = coder_spec();
    s.image = "ghcr.io/acme/guard-sandbox:latest".into();
    assert!(s.validate().unwrap_err().contains("digest"));

    let mut s = coder_spec();
    s.env.insert("ANTHROPIC_API_KEY".into(), "x".into());
    assert!(s.validate().unwrap_err().contains("secrets"));
}

#[test]
fn secrets_never_print() {
    let s = Secret::new("sk-very-secret");
    assert_eq!(format!("{s:?}"), "Secret(\"***\")");
    assert_eq!(s.expose(), "sk-very-secret");
}

#[tokio::test]
async fn fake_sandbox_lifecycle() {
    let provider = FakeSandboxProvider::new().with_diff(ExtractedDiff {
        base_commit: SHA.into(),
        unified_diff: "diff --git a/src/a.ts b/src/a.ts\n".into(),
        changed_paths: vec!["src/a.ts".into()],
    });
    let sb = provider.create(&coder_spec()).await.unwrap();
    assert_eq!(sb.role, Role::Coder);
    provider
        .put_file(&sb, TASK_FILE, b"{}".to_vec())
        .await
        .unwrap();
    assert_eq!(provider.read_file(&sb, TASK_FILE).await.unwrap(), b"{}");
    assert!(matches!(
        provider.read_file(&sb, RESULT_FILE).await,
        Err(SandboxError::NotFound(_))
    ));
    let out = provider
        .exec(&sb, &ExecRequest::new(["git", "status"]))
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert_eq!(provider.exec_log(&sb.id)[0], vec!["git", "status"]);
    let diff = provider.extract_diff(&sb, SHA).await.unwrap();
    assert_eq!(diff.changed_paths, vec!["src/a.ts"]);
    provider.destroy(&sb).await.unwrap();
    provider.destroy(&sb).await.unwrap(); // idempotent
    assert!(provider.is_destroyed(&sb.id));
    assert!(matches!(
        provider.exec(&sb, &ExecRequest::new(["true"])).await,
        Err(SandboxError::NotFound(_))
    ));

    let mut bad = coder_spec();
    bad.env.clear();
    assert!(matches!(
        provider.create(&bad).await,
        Err(SandboxError::InvalidSpec(_))
    ));
}

// ---------------------------------------------------------------- runtime

#[tokio::test]
async fn fake_runtime_returns_scripted_outcomes() {
    let provider = FakeSandboxProvider::new();
    let sb = provider.create(&coder_spec()).await.unwrap();
    let runtime = FakeAgentRuntime::new()
        .with_outcome(Ok(FakeAgentRuntime::completed("fixed", 0.42)))
        .with_outcome(Err(RuntimeError::Timeout { secs: 1800 }));
    assert_eq!(runtime.name(), "fake");
    let first = runtime.run(&sb, &coder_task()).await.unwrap();
    assert_eq!(first.result.status, TaskStatus::Completed);
    assert_eq!(first.runtime, "fake");
    assert!(matches!(
        runtime.run(&sb, &coder_task()).await,
        Err(RuntimeError::Timeout { secs: 1800 })
    ));
    assert!(matches!(
        runtime.run(&sb, &coder_task()).await,
        Err(RuntimeError::Unavailable(_))
    ));
    assert_eq!(runtime.tasks().len(), 3);

    let mut invalid = coder_task();
    invalid.sealed_spec = None;
    assert!(matches!(
        FakeAgentRuntime::new().run(&sb, &invalid).await,
        Err(RuntimeError::InvalidTask(_))
    ));
}

// ---------------------------------------------------------------- gateway

fn key_request() -> KeyRequest {
    KeyRequest {
        alias: "run-1:coder:1".into(),
        models: vec!["anthropic/claude-sonnet-4-5".into()],
        max_budget_usd: 5.0,
        rpm_limit: Some(60),
        tpm_limit: Some(400_000),
        max_parallel_requests: Some(2),
        duration: Duration::from_secs(3600),
        metadata: [("run_id".to_string(), "run-1".to_string())].into(),
    }
}

#[tokio::test]
async fn fake_gateway_mints_revokes_and_pages_spend() {
    let gw = FakeModelGateway::new();
    let key = gw.mint_key(&key_request()).await.unwrap();
    assert!(key.token.expose().starts_with("sk-fake-"));
    assert!(!format!("{key:?}").contains(key.token.expose()));
    assert_eq!(key.max_budget_usd, 5.0);
    assert!(gw.is_active(&key.key_id));

    let mut bad = key_request();
    bad.models.clear();
    assert!(matches!(
        gw.mint_key(&bad).await,
        Err(GatewayError::InvalidRequest(_))
    ));
    let mut bad = key_request();
    bad.duration = Duration::ZERO;
    assert!(gw.mint_key(&bad).await.is_err());

    for i in 0..5 {
        gw.push_spend(SpendRecord {
            request_id: format!("req-{i}"),
            key_alias: Some(key.alias.clone()),
            model: "anthropic/claude-sonnet-4-5".into(),
            spend_usd: 0.01,
            prompt_tokens: 1000,
            completion_tokens: 100,
            cached_tokens: 800,
            cache_creation_input_tokens: 0,
            cache_hit: None,
            start_time: "2026-11-01T00:00:00Z".into(),
            end_time: "2026-11-01T00:00:01Z".into(),
            metadata: Default::default(),
        });
    }
    let q = SpendQuery {
        key_alias: Some(key.alias.clone()),
        start: "2026-11-01 00:00:00".into(),
        end: "2026-11-02 00:00:00".into(),
        page: 1,
        page_size: 2,
    };
    let p1 = gw.spend_logs(&q).await.unwrap();
    assert_eq!((p1.records.len(), p1.page, p1.total_pages), (2, 1, 3));
    let p3 = gw
        .spend_logs(&SpendQuery {
            page: 3,
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(p3.records.len(), 1);
    assert!(
        gw.spend_logs(&SpendQuery { page: 0, ..q }).await.is_err(),
        "pages start at 1"
    );

    gw.revoke(&key.key_id).await.unwrap();
    assert!(!gw.is_active(&key.key_id));
    assert!(matches!(
        gw.revoke("missing").await,
        Err(GatewayError::NotFound(_))
    ));
}

// ---------------------------------------------------------------- queue

fn new_task(ticket: &str, role: Role, files: &[&str]) -> NewTask {
    NewTask {
        repo: "acme/shop".into(),
        ticket: ticket.into(),
        role,
        priority: 0,
        predicted_files: files.iter().map(|f| f.to_string()).collect(),
        payload: serde_json::json!({}),
        max_attempts: 2,
    }
}

#[tokio::test]
async fn task_queue_dedupes_claims_and_avoids_conflicts() {
    let q = InMemoryTaskQueue::new();
    let a = q
        .enqueue(&new_task("SHOP-1", Role::Coder, &["src/a.ts"]))
        .await
        .unwrap();
    assert!(a.created);
    let again = q
        .enqueue(&new_task("SHOP-1", Role::Coder, &["src/a.ts"]))
        .await
        .unwrap();
    assert_eq!((again.id, again.created), (a.id, false));
    let b = q
        .enqueue(&new_task("SHOP-2", Role::Coder, &["src/a.ts", "src/b.ts"]))
        .await
        .unwrap();
    let c = q
        .enqueue(&new_task("SHOP-3", Role::Coder, &["src/c.ts"]))
        .await
        .unwrap();

    let lease = Duration::from_secs(60);
    let first = q.claim("w1", lease).await.unwrap().unwrap();
    assert_eq!(first.task.id, a.id);
    assert_eq!(first.task.attempts, 1);
    // SHOP-2 shares src/a.ts with the running SHOP-1: it waits; SHOP-3 is claimed.
    let second = q.claim("w2", lease).await.unwrap().unwrap();
    assert_eq!(second.task.id, c.id);
    assert!(q.claim("w3", lease).await.unwrap().is_none());
    assert_eq!(
        q.conflicts(&["src/b.ts".into(), "src/a.ts".into()])
            .await
            .unwrap(),
        vec![a.id]
    );

    assert!(matches!(
        q.complete(a.id, "w2").await,
        Err(StoreError::Invalid(_))
    ));
    q.complete(a.id, "w1").await.unwrap();
    let third = q.claim("w3", lease).await.unwrap().unwrap();
    assert_eq!(third.task.id, b.id);

    // fail with retry re-queues while attempts < max_attempts, then fails for good.
    q.fail(b.id, "w3", "boom", Some(Duration::ZERO))
        .await
        .unwrap();
    let retry = q.claim("w4", lease).await.unwrap().unwrap();
    assert_eq!((retry.task.id, retry.task.attempts), (b.id, 2));
    q.fail(b.id, "w4", "boom again", Some(Duration::ZERO))
        .await
        .unwrap();
    assert_eq!(q.get(b.id).unwrap().state, TaskState::Failed);

    // An expired lease makes a running task claimable again.
    q.expire_leases();
    let reclaimed = q.claim("w5", lease).await.unwrap().unwrap();
    assert_eq!(reclaimed.task.id, c.id);
}

// ---------------------------------------------------------------- memory

fn fact(topic: &str, body: &str) -> NewFact {
    NewFact {
        repo: "acme/shop".into(),
        scope: FactScope::Journey,
        scope_key: Some("checkout".into()),
        topic_key: topic.into(),
        body: body.into(),
        commit_sha: SHA.into(),
        run_id: Some("run-1".into()),
        evidence_uri: Some("s3://guard-evidence/runs/run-1/trace.zip".into()),
        source: FactSource::GateResult,
        valid_from: "2026-11-01T00:00:00Z".into(),
        review_after: None,
        referenced_paths: vec!["src/checkout/pay.ts".into()],
    }
}

#[test]
fn new_fact_validation() {
    assert_eq!(
        fact("flaky/checkout-pay", "Retries once").validate(),
        Ok(())
    );
    let mut f = fact("Bad Topic", "x");
    assert!(f.validate().unwrap_err().contains("topic_key"));
    f = fact("t", "x");
    f.scope_key = None;
    assert!(f.validate().unwrap_err().contains("scope_key"));
    f = fact("t", "x");
    f.scope = FactScope::Repo;
    assert!(f.validate().unwrap_err().contains("scope_key"));
    f = fact("t", &"x".repeat(MAX_FACT_BODY_BYTES + 1));
    assert!(f.validate().unwrap_err().contains("body"));
    f = fact("t", "x");
    f.commit_sha = "zzz".into();
    assert!(f.validate().unwrap_err().contains("commit_sha"));
}

#[tokio::test]
async fn memory_facts_supersede_review_and_candidates() {
    let store = Arc::new(InMemoryMemoryFacts::new());
    let first = store.record_fact(&fact("flaky/pay", "v1")).await.unwrap();
    // Same body at the same commit: idempotent, no new version.
    assert_eq!(
        store.record_fact(&fact("flaky/pay", "v1")).await.unwrap(),
        first
    );
    let mut v2 = fact("flaky/pay", "v2");
    v2.valid_from = "2026-11-02T00:00:00Z".into();
    let second = store.record_fact(&v2).await.unwrap();

    let old = store.get_fact(first).await.unwrap().unwrap();
    assert_eq!(old.status, FactStatus::Invalidated);
    assert_eq!(old.invalid_at.as_deref(), Some("2026-11-02T00:00:00Z"));
    let new = store.get_fact(second).await.unwrap().unwrap();
    assert_eq!(new.supersedes, Some(first));
    assert_eq!(new.status, FactStatus::Active);

    let history = store.topic_history("acme/shop", "flaky/pay").await.unwrap();
    assert_eq!(
        history.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![first, second]
    );

    store
        .mark_needs_review(second, "src/checkout/pay.ts changed in abc1234")
        .await
        .unwrap();
    let page = store
        .list_facts(&FactsQuery::for_repo("acme/shop"))
        .await
        .unwrap();
    assert_eq!(page.facts.len(), 1);
    assert_eq!(page.facts[0].status, FactStatus::NeedsReview);
    store.confirm(second, None).await.unwrap();
    assert_eq!(
        store.get_fact(second).await.unwrap().unwrap().status,
        FactStatus::Active
    );

    // Agent proposals only reach a candidate queue; approval writes the fact.
    let cand = store
        .enqueue_candidate(&FactCandidate {
            fact: fact("convention/selectors", "Use getByRole"),
            proposed_by: "claude-code:qa".into(),
        })
        .await
        .unwrap();
    assert_eq!(store.list_candidates("acme/shop").await.unwrap().len(), 1);
    let approved = store.approve_candidate(cand, "alice").await.unwrap();
    let f = store.get_fact(approved).await.unwrap().unwrap();
    assert_eq!(f.source, FactSource::ApprovedCandidate);
    assert!(store.list_candidates("acme/shop").await.unwrap().is_empty());
    assert!(store.approve_candidate(cand, "alice").await.is_err());

    store
        .invalidate(
            approved,
            "2026-11-03T00:00:00Z",
            "selector convention dropped",
        )
        .await
        .unwrap();
    let all = store
        .list_facts(&FactsQuery {
            statuses: vec![FactStatus::Invalidated],
            ..FactsQuery::for_repo("acme/shop")
        })
        .await
        .unwrap();
    assert_eq!(all.facts.len(), 2);
}

#[tokio::test]
async fn memory_reader_view_pages_and_has_no_write_side() {
    let store = Arc::new(InMemoryMemoryFacts::new());
    for i in 0..5 {
        store
            .record_fact(&fact(&format!("topic/{i}"), "b"))
            .await
            .unwrap();
    }
    // The only handle agents and `guard mcp` get: a reader trait object.
    let reader: Arc<dyn MemoryFactsReader> = store.reader();
    let q = FactsQuery {
        limit: 2,
        ..FactsQuery::for_repo("acme/shop")
    };
    let p1 = reader.list_facts(&q).await.unwrap();
    assert_eq!(p1.facts.len(), 2);
    let p2 = reader
        .list_facts(&FactsQuery {
            cursor: p1.next_cursor.clone(),
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(p2.facts.len(), 2);
    assert_ne!(p1.facts[0].id, p2.facts[0].id);
    let p3 = reader
        .list_facts(&FactsQuery {
            cursor: p2.next_cursor.clone(),
            ..q.clone()
        })
        .await
        .unwrap();
    assert_eq!(p3.facts.len(), 1);
    assert!(p3.next_cursor.is_none());
    let over = reader
        .list_facts(&FactsQuery {
            limit: MAX_FACTS_PAGE + 1,
            ..q
        })
        .await;
    assert!(over.is_err());
}

#[test]
fn facts_brief_is_deterministic_and_byte_capped() {
    let mk = |id: i64, topic: &str, body: &str, status: FactStatus| MemoryFact {
        id: FactId(id),
        repo: "acme/shop".into(),
        scope: FactScope::Journey,
        scope_key: Some("checkout".into()),
        topic_key: topic.into(),
        body: body.into(),
        commit_sha: SHA.into(),
        run_id: None,
        evidence_uri: None,
        source: FactSource::GateResult,
        valid_from: "2026-11-01T00:00:00Z".into(),
        invalid_at: None,
        recorded_at: "2026-11-01T00:00:00Z".into(),
        review_after: None,
        status,
        review_reason: None,
        supersedes: None,
        referenced_paths: vec![],
    };
    let facts = vec![
        mk(2, "b/topic", "second\nline", FactStatus::NeedsReview),
        mk(1, "a/topic", "first", FactStatus::Active),
        mk(3, "c/topic", "gone", FactStatus::Invalidated),
    ];
    let brief = FactsBrief::render(&facts, FACTS_BRIEF_MAX_BYTES);
    assert_eq!(
        brief.text,
        format!(
            "{FACTS_BRIEF_HEADER}\n- [1] journey:checkout a/topic: first\n- [2] journey:checkout b/topic: second line (needs_review)\n"
        )
    );
    assert_eq!(brief.fact_ids, vec![FactId(1), FactId(2)]);
    assert!(!brief.truncated);
    assert_eq!(brief.validate(), Ok(()));

    let many: Vec<MemoryFact> = (0..200)
        .map(|i| mk(i, &format!("t/{i:03}"), &"x".repeat(60), FactStatus::Active))
        .collect();
    let capped = FactsBrief::render(&many, FACTS_BRIEF_MAX_BYTES);
    assert!(capped.text.len() <= FACTS_BRIEF_MAX_BYTES);
    assert!(capped.truncated);
    assert!(capped.text.ends_with('\n'));

    let tiny = FactsBrief::render(&facts, 10);
    assert!(tiny.text.len() <= 10);

    let oversized = FactsBrief {
        text: "x".repeat(FACTS_BRIEF_MAX_BYTES + 1),
        fact_ids: vec![],
        truncated: false,
    };
    assert!(oversized.validate().is_err());
}

#[tokio::test]
async fn fake_gateway_lists_active_keys_by_alias_prefix() {
    let gw = FakeModelGateway::new();
    let mut a = key_request();
    a.alias = "a17-shop-1:coder:1".into();
    let mut b = key_request();
    b.alias = "a17-shop-1:qa:1".into();
    let mut other = key_request();
    other.alias = "exp:memory:SHOP-1:r1".into();
    let ka = gw.mint_key(&a).await.unwrap();
    let kb = gw.mint_key(&b).await.unwrap();
    gw.mint_key(&other).await.unwrap();
    gw.set_created_at(&ka.key_id, "2026-11-01T10:00:00Z");

    let keys = gw.list_keys("a17-").await.unwrap();
    let aliases: Vec<_> = keys.iter().map(|k| k.alias.as_deref()).collect();
    assert_eq!(
        aliases,
        [Some("a17-shop-1:coder:1"), Some("a17-shop-1:qa:1")]
    );
    assert_eq!(keys[0].key_id, ka.key_id);
    assert_eq!(keys[0].created_at.as_deref(), Some("2026-11-01T10:00:00Z"));
    assert_eq!(keys[0].metadata["run_id"], "run-1");
    assert_eq!(gw.list_keys("").await.unwrap().len(), 3);

    gw.revoke(&kb.key_id).await.unwrap();
    assert_eq!(gw.list_keys("a17-").await.unwrap().len(), 1);
}

#[tokio::test]
async fn list_keys_defaults_to_unsupported() {
    struct Minimal;
    #[async_trait::async_trait]
    impl ModelGateway for Minimal {
        async fn mint_key(&self, _: &KeyRequest) -> Result<VirtualKey, GatewayError> {
            Err(GatewayError::Unconfigured("x".into()))
        }
        async fn revoke(&self, _: &str) -> Result<(), GatewayError> {
            Ok(())
        }
        async fn spend_logs(&self, _: &SpendQuery) -> Result<SpendPage, GatewayError> {
            Err(GatewayError::Unconfigured("x".into()))
        }
    }
    assert!(matches!(
        Minimal.list_keys("a").await,
        Err(GatewayError::Unconfigured(_))
    ));
}
