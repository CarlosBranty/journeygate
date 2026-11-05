//! `fake`: a scripted runtime for end-to-end tests of the agent lane.
//!
//! It runs no process and calls no model. It follows a [`FakePlan`] (given
//! with [`FakeRuntime::with_plan`], or read from [`PLAN_PATH`] inside the
//! sandbox): edit files in the worktree, emit events, then write the `/task`
//! file contract like a real runtime. The plan may also script a timeout or a
//! crash. It does **not** enforce `allowed_paths` (a fake agent may misbehave
//! on purpose so the harness's diff check can be tested); it only refuses
//! paths that escape the worktree.
//!
//! `guard agents` accepts `runtime = "fake"` only with
//! `GUARD_ALLOW_FAKE_RUNTIME=1` (agents-toml.md).

use crate::clock::{Clock, SystemClock};
use crate::events::{EventLog, to_jsonl};
use guard_core::ports::agent::{
    AgentOutcome, AgentRuntime, AgentTask, EVENTS_FILE, MemoryProposal, RESULT_FILE, RuntimeError,
    RuntimeEventKind, TASK_FILE, TASK_SCHEMA_VERSION, TaskResult, TaskStatus,
};
use guard_core::ports::sandbox::{
    ExecRequest, Sandbox, SandboxError, SandboxProvider, WORKSPACE_DIR,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const NAME: &str = "fake";
/// Where [`FakeRuntime`] looks for a plan when none was given.
pub const PLAN_PATH: &str = "/task/fake-plan.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeStep {
    /// Writes `content` to `/workspace/<path>` (repo-relative).
    Edit { path: String, content: String },
    /// Appends one event to `events.jsonl`.
    Event(RuntimeEventKind),
    /// Runs `argv` inside the sandbox (no shell; the sandbox secrets are in
    /// its env) and records a `tool_call` event `exec:<argv[0]>`. With
    /// `expect_exit`, any other exit code fails the run with
    /// `RuntimeError::Protocol` (end-to-end egress probes use this).
    Exec {
        argv: Vec<String>,
        #[serde(default)]
        expect_exit: Option<i32>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FakeResult {
    pub status: TaskStatus,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub memory_proposals: Vec<MemoryProposal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeFailure {
    /// The run is reported as `RuntimeError::Timeout`.
    Timeout,
    /// The process "dies" without a result: `RuntimeError::Crashed`.
    Crash { exit_code: i32, stderr: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FakePlan {
    pub steps: Vec<FakeStep>,
    /// Default: `completed` with an empty summary.
    #[serde(default)]
    pub result: Option<FakeResult>,
    /// Applied after the steps (edits stay in the worktree).
    #[serde(default)]
    pub fail: Option<FakeFailure>,
}

pub struct FakeRuntime {
    provider: Arc<dyn SandboxProvider>,
    clock: Arc<dyn Clock>,
    plan: Option<FakePlan>,
}

impl FakeRuntime {
    pub fn new(provider: Arc<dyn SandboxProvider>) -> Self {
        Self {
            provider,
            clock: Arc::new(SystemClock),
            plan: None,
        }
    }

    pub fn with_plan(mut self, plan: FakePlan) -> Self {
        self.plan = Some(plan);
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    async fn load_plan(&self, sandbox: &Sandbox) -> Result<FakePlan, RuntimeError> {
        if let Some(p) = &self.plan {
            return Ok(p.clone());
        }
        match self.provider.read_file(sandbox, PLAN_PATH).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| RuntimeError::Protocol(format!("{PLAN_PATH}: {e}"))),
            Err(SandboxError::NotFound(_)) => Err(RuntimeError::Unavailable(format!(
                "fake runtime: no plan given and no {PLAN_PATH}"
            ))),
            Err(e) => Err(e.into()),
        }
    }
}

fn workspace_path(rel: &str) -> Result<String, RuntimeError> {
    let safe = !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel.contains(':')
        && !rel
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..");
    if safe {
        Ok(format!("{WORKSPACE_DIR}/{rel}"))
    } else {
        Err(RuntimeError::Protocol(format!(
            "fake plan edits {rel:?}, which is not a plain repo-relative path"
        )))
    }
}

#[async_trait::async_trait]
impl AgentRuntime for FakeRuntime {
    fn name(&self) -> &'static str {
        NAME
    }

    async fn run(&self, sandbox: &Sandbox, task: &AgentTask) -> Result<AgentOutcome, RuntimeError> {
        task.validate().map_err(RuntimeError::InvalidTask)?;
        let plan = self.load_plan(sandbox).await?;
        let started = self.clock.now_millis();
        let task_json = serde_json::to_vec_pretty(task)
            .map_err(|e| RuntimeError::Protocol(format!("task.json: {e}")))?;
        self.provider
            .put_file(sandbox, TASK_FILE, task_json)
            .await?;

        let mut log = EventLog::default();
        for step in &plan.steps {
            match step {
                FakeStep::Edit { path, content } => {
                    let target = workspace_path(path)?;
                    self.provider
                        .put_file(sandbox, &target, content.clone().into_bytes())
                        .await?;
                }
                FakeStep::Event(kind) => log.push_at(self.clock.now_millis(), kind.clone()),
                FakeStep::Exec { argv, expect_exit } => {
                    let program = argv.first().cloned().ok_or_else(|| {
                        RuntimeError::Protocol("fake plan exec step has an empty argv".into())
                    })?;
                    let started = self.clock.now_millis();
                    let out = self
                        .provider
                        .exec(sandbox, &ExecRequest::new(argv.clone()))
                        .await?;
                    let ok = expect_exit.is_none_or(|code| code == out.exit_code);
                    log.push_at(
                        self.clock.now_millis(),
                        RuntimeEventKind::ToolCall {
                            tool: format!("exec:{program}"),
                            ok,
                            duration_ms: self.clock.now_millis().saturating_sub(started),
                        },
                    );
                    if !ok {
                        return Err(RuntimeError::Protocol(format!(
                            "fake plan exec {program:?} exited {}, expected {}: {}{}",
                            out.exit_code,
                            expect_exit.unwrap_or_default(),
                            String::from_utf8_lossy(&out.stdout).trim(),
                            String::from_utf8_lossy(&out.stderr).trim()
                        )));
                    }
                }
            }
        }
        match plan.fail {
            Some(FakeFailure::Timeout) => {
                return Err(RuntimeError::Timeout {
                    secs: task.timeout_secs,
                });
            }
            Some(FakeFailure::Crash { exit_code, stderr }) => {
                return Err(RuntimeError::Crashed {
                    exit_code: Some(exit_code),
                    stderr_tail: stderr,
                });
            }
            None => {}
        }

        let scripted = plan.result.unwrap_or(FakeResult {
            status: TaskStatus::Completed,
            summary: String::new(),
            memory_proposals: vec![],
            error: None,
        });
        let result = TaskResult {
            version: TASK_SCHEMA_VERSION,
            status: scripted.status,
            summary: scripted.summary,
            usage: log.usage_sum(),
            cost_usd: log.cost_sum(),
            steps: plan.steps.len() as u32,
            memory_proposals: scripted.memory_proposals,
            error: scripted.error,
        };
        let events = log.into_events();
        self.provider
            .put_file(sandbox, EVENTS_FILE, to_jsonl(&events))
            .await?;
        let mut bytes = serde_json::to_vec_pretty(&result)
            .map_err(|e| RuntimeError::Protocol(format!("result.json: {e}")))?;
        bytes.push(b'\n');
        self.provider.put_file(sandbox, RESULT_FILE, bytes).await?;
        Ok(AgentOutcome {
            runtime: NAME.into(),
            result,
            events,
            exit_code: Some(0),
            duration_ms: self.clock.now_millis().saturating_sub(started),
        })
    }
}
