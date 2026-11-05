use crate::common::{coder_task, sandbox_for};
use guard_core::ports::agent::{AgentRuntime, EVENTS_FILE, RESULT_FILE};
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_core::ports::sandbox::{ExtractedDiff, SandboxProvider};
use guard_core::ports::{RuntimeError, RuntimeEventKind, TaskStatus, Usage};
use guard_runtimes::clock::FixedClock;
use guard_runtimes::fake::{FakePlan, FakeRuntime, FakeStep, PLAN_PATH};
use std::sync::Arc;

const T0: u64 = 1_793_527_200_000;

fn plan() -> FakePlan {
    serde_json::from_value(serde_json::json!({
        "steps": [
            {"edit": {"path": "src/badge.ts", "content": "export const badge = (p) => p.stock === 0;\n"}},
            {"event": {"kind": "model_call", "model": "fake/model",
                       "usage": {"input_tokens": 100, "output_tokens": 10, "cached_input_tokens": 80, "cache_creation_input_tokens": 0},
                       "cost_usd": 0.01}},
            {"event": {"kind": "tool_call", "tool": "guard_test", "ok": true, "duration_ms": 5}},
            {"event": {"kind": "model_call", "model": "fake/model",
                       "usage": {"input_tokens": 50, "output_tokens": 5, "cached_input_tokens": 0, "cache_creation_input_tokens": 0},
                       "cost_usd": 0.02}},
            {"event": {"kind": "message", "text": "done"}}
        ],
        "result": {"status": "completed", "summary": "Fixed the badge",
                   "memory_proposals": [{"topic_key": "catalog/stock-badge", "body": "Badge reads product.stock"}]}
    }))
    .unwrap()
}

#[tokio::test]
async fn scripted_plan_edits_files_and_emits_the_contract() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let task = coder_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let rt = FakeRuntime::new(provider.clone())
        .with_plan(plan())
        .with_clock(Arc::new(FixedClock::new(T0)));
    assert_eq!(rt.name(), "fake");

    let out = rt.run(&sb, &task).await.unwrap();
    assert_eq!(out.runtime, "fake");
    assert_eq!(out.exit_code, Some(0));
    let edited = provider
        .read_file(&sb, "/workspace/src/badge.ts")
        .await
        .unwrap();
    assert_eq!(edited, b"export const badge = (p) => p.stock === 0;\n");

    assert_eq!(out.events.len(), 4);
    assert_eq!(
        out.events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(&out.events[3].event, RuntimeEventKind::Message { text } if text == "done"));
    let r = &out.result;
    assert_eq!(r.status, TaskStatus::Completed);
    assert_eq!(r.summary, "Fixed the badge");
    assert_eq!(r.steps, 5);
    assert!((r.cost_usd - 0.03).abs() < 1e-12);
    assert_eq!(
        r.usage,
        Usage {
            input_tokens: 150,
            output_tokens: 15,
            cached_input_tokens: 80,
            cache_creation_input_tokens: 0
        }
    );
    assert_eq!(r.memory_proposals.len(), 1);

    let events = String::from_utf8(provider.read_file(&sb, EVENTS_FILE).await.unwrap()).unwrap();
    assert_eq!(events.lines().count(), 4);
    let written: guard_core::ports::TaskResult =
        serde_json::from_slice(&provider.read_file(&sb, RESULT_FILE).await.unwrap()).unwrap();
    assert_eq!(&written, r);
}

#[tokio::test]
async fn plan_is_read_from_the_sandbox_when_not_given() {
    let provider = Arc::new(FakeSandboxProvider::new().with_diff(ExtractedDiff {
        base_commit: coder_task().base_commit,
        unified_diff: "diff --git a/src/badge.ts b/src/badge.ts\n".into(),
        changed_paths: vec!["src/badge.ts".into()],
    }));
    let task = coder_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    provider
        .put_file(&sb, PLAN_PATH, serde_json::to_vec(&plan()).unwrap())
        .await
        .unwrap();
    let out = FakeRuntime::new(provider.clone())
        .run(&sb, &task)
        .await
        .unwrap();
    assert_eq!(out.result.status, TaskStatus::Completed);

    // End to end with the orchestrator's next step: the diff passes the allowlist.
    let diff = provider.extract_diff(&sb, &task.base_commit).await.unwrap();
    assert!(
        task.allowed_paths
            .violations(&diff.changed_paths)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unsafe_edit_paths_and_missing_plans_fail() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let task = coder_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let err = FakeRuntime::new(provider.clone())
        .run(&sb, &task)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Unavailable(_)), "{err:?}");

    let bad = FakePlan {
        steps: vec![FakeStep::Edit {
            path: "../etc/passwd".into(),
            content: String::new(),
        }],
        ..plan()
    };
    let err = FakeRuntime::new(provider.clone())
        .with_plan(bad)
        .run(&sb, &task)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn scripted_failures_map_to_runtime_errors() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let task = coder_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let timeout: FakePlan =
        serde_json::from_value(serde_json::json!({"steps": [], "fail": "timeout"})).unwrap();
    let err = FakeRuntime::new(provider.clone())
        .with_plan(timeout)
        .run(&sb, &task)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        RuntimeError::Timeout {
            secs: task.timeout_secs
        }
    );

    let crash: FakePlan = serde_json::from_value(
        serde_json::json!({"steps": [], "fail": {"crash": {"exit_code": 137, "stderr": "killed"}}}),
    )
    .unwrap();
    let err = FakeRuntime::new(provider.clone())
        .with_plan(crash)
        .run(&sb, &task)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        RuntimeError::Crashed {
            exit_code: Some(137),
            stderr_tail: "killed".into()
        }
    );
}

#[tokio::test]
async fn invalid_tasks_never_start() {
    let provider = Arc::new(FakeSandboxProvider::new());
    let task = coder_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;
    let mut bad = task.clone();
    bad.budget_usd = 0.0;
    let err = FakeRuntime::new(provider.clone())
        .with_plan(plan())
        .run(&sb, &bad)
        .await
        .unwrap_err();
    assert!(matches!(err, RuntimeError::InvalidTask(_)));
    assert!(
        provider
            .read_file(&sb, "/workspace/src/badge.ts")
            .await
            .is_err()
    );
}

fn exec_plan(expect: i32) -> FakePlan {
    serde_json::from_value(serde_json::json!({
        "steps": [
            {"exec": {"argv": ["node", "-e", "probe()"], "expect_exit": expect}},
            {"edit": {"path": "src/a.ts", "content": "x\n"}}
        ]
    }))
    .unwrap()
}

#[tokio::test]
async fn exec_steps_run_in_the_sandbox_and_check_the_exit_code() {
    use guard_core::ports::sandbox::ExecOutput;
    let output = ExecOutput {
        exit_code: 7,
        stdout: b"blocked\n".to_vec(),
        stderr: vec![],
        timed_out: false,
    };
    let provider = Arc::new(FakeSandboxProvider::new().with_exec("node", output));
    let task = coder_task();
    let sb = sandbox_for(provider.as_ref(), &task).await;

    let rt = FakeRuntime::new(provider.clone()).with_plan(exec_plan(7));
    let out = rt.run(&sb, &task).await.unwrap();
    assert_eq!(
        provider.exec_log(&sb.id),
        vec![vec!["node", "-e", "probe()"]]
    );
    assert!(out.events.iter().any(|e| matches!(&e.event,
        RuntimeEventKind::ToolCall { tool, ok: true, .. } if tool == "exec:node")));

    let rt = FakeRuntime::new(provider.clone()).with_plan(exec_plan(0));
    match rt.run(&sb, &task).await {
        Err(RuntimeError::Protocol(m)) => assert!(m.contains("exited 7, expected 0"), "{m}"),
        other => panic!("{other:?}"),
    }
}
