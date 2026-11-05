//! Shared helpers of the guard-runtimes integration tests.
#![allow(dead_code)]

use guard_core::ports::agent::AgentTask;
use guard_core::ports::sandbox::{
    EgressAllowlist, Sandbox, SandboxLayout, SandboxProvider, SandboxSpec,
};
use guard_core::ports::{AllowedPaths, Role};
use std::path::PathBuf;

/// The contract example (`schemas/examples/agent-task.example.json`), a coder task.
pub fn coder_task() -> AgentTask {
    let raw = include_str!("../../../schemas/examples/agent-task.example.json");
    let task: AgentTask = serde_json::from_str(raw).expect("example task parses");
    task.validate().expect("example task is valid");
    task
}

/// A QA task derived from the example.
pub fn qa_task() -> AgentTask {
    let mut t = coder_task();
    t.role = Role::Qa;
    t.sealed_spec = None;
    t.allowed_paths = AllowedPaths::qa("tests", &t.ticket);
    t.model = "claude-opus-5-5".into();
    t.attempt_policy = guard_core::ports::AttemptPolicy::new(1).unwrap();
    t.validate().expect("qa task is valid");
    t
}

pub fn fixture(rel: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("fixture {}: {e}", p.display()))
}

/// Compares `actual` with `tests/golden/<rel>`. `GUARD_UPDATE_GOLDEN=1`
/// rewrites the file instead (review the diff before committing).
pub fn assert_golden(rel: &str, actual: &str) {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(rel);
    if std::env::var("GUARD_UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| {
            panic!(
                "golden {} missing ({e}); run with GUARD_UPDATE_GOLDEN=1",
                p.display()
            )
        })
        .replace("\r\n", "\n");
    assert_eq!(expected, actual, "golden {} differs", p.display());
}

/// A sandbox in a `FakeSandboxProvider`, created from a valid role spec.
pub async fn sandbox_for(provider: &dyn SandboxProvider, task: &AgentTask) -> Sandbox {
    let layout = SandboxLayout {
        run_id: task.run_id.clone(),
        ticket: task.ticket.clone(),
        attempt: task.attempt,
        image: format!("ghcr.io/acme/guard-sandbox@sha256:{}", "a".repeat(64)),
        checkout: "/host/checkout".into(),
        task_dir: "/host/task".into(),
        codegraph_index: Some("/host/codegraph".into()),
        sealed_spec: task
            .sealed_spec
            .as_ref()
            .map(|s| ("/host/sealed.spec.ts".to_string(), s.path.clone())),
        tests_dir: "tests".into(),
        skills_dir: Some("/host/agents".into()),
    };
    let spec = SandboxSpec::for_role(
        task.role,
        layout,
        EgressAllowlist::gateway_only("llm-gateway", 4000),
    );
    provider.create(&spec).await.expect("valid spec")
}
