//! The run loop shared by every process-based adapter.
//!
//! 1. `AgentTask::validate` (`InvalidTask`, nothing starts).
//! 2. The adapter builds its [`Invocation`] (gateway pinned) and the loop
//!    checks that no other endpoint appears in it.
//! 3. Adapter preflight (e.g. Claude Code's managed settings).
//! 4. `task.json` and the config files are written into `/task`.
//! 5. One `exec` with `task.timeout_secs`; `timed_out` is `Timeout`.
//! 6. The adapter collects its output into a [`Parsed`]. No terminal record:
//!    `Crashed` on a non-zero exit, `Protocol` otherwise.
//! 7. For CLI runtimes the loop writes `events.jsonl` and `result.json`.

use crate::clock::Clock;
use crate::endpoint::{GatewayEndpoint, ensure_only_gateway_except};
use crate::events::{Parsed, STDERR_TAIL_BYTES, tail, to_jsonl};
use crate::invocation::Invocation;
use guard_core::ports::agent::{
    AgentOutcome, AgentTask, EVENTS_FILE, RESULT_FILE, RuntimeError, TASK_FILE,
};
use guard_core::ports::sandbox::{ExecOutput, Sandbox, SandboxProvider};
use std::time::Duration;

#[async_trait::async_trait]
pub(crate) trait ProcessAdapter: Send + Sync {
    fn name(&self) -> &'static str;

    /// The pinned endpoint, or `None` for the Copilot-hosted exception.
    fn endpoint(&self, task: &AgentTask) -> Result<Option<GatewayEndpoint>, RuntimeError> {
        GatewayEndpoint::pin(&task.gateway).map(Some)
    }

    fn invocation(&self, task: &AgentTask) -> Result<Invocation, RuntimeError>;

    async fn preflight(
        &self,
        _provider: &dyn SandboxProvider,
        _sandbox: &Sandbox,
        _task: &AgentTask,
    ) -> Result<(), RuntimeError> {
        Ok(())
    }

    async fn collect(
        &self,
        provider: &dyn SandboxProvider,
        sandbox: &Sandbox,
        task: &AgentTask,
        output: &ExecOutput,
        clock: &dyn Clock,
    ) -> Result<Parsed, RuntimeError>;

    /// `true` when the runtime itself writes `events.jsonl` and `result.json`.
    fn writes_contract_files(&self) -> bool {
        false
    }
}

pub(crate) async fn run(
    adapter: &dyn ProcessAdapter,
    provider: &dyn SandboxProvider,
    clock: &dyn Clock,
    sandbox: &Sandbox,
    task: &AgentTask,
) -> Result<AgentOutcome, RuntimeError> {
    task.validate().map_err(RuntimeError::InvalidTask)?;
    let endpoint = adapter.endpoint(task)?;
    let inv = adapter.invocation(task)?;
    let prompt = crate::prompt::render(task);
    ensure_only_gateway_except(&inv, endpoint.as_ref(), Some(&prompt))?;
    adapter.preflight(provider, sandbox, task).await?;

    let task_json = serde_json::to_vec_pretty(task)
        .map_err(|e| RuntimeError::Protocol(format!("task.json: {e}")))?;
    provider.put_file(sandbox, TASK_FILE, task_json).await?;
    for f in &inv.files {
        provider
            .put_file(sandbox, &f.path, f.contents.clone().into_bytes())
            .await?;
    }

    let started = clock.now_millis();
    let output = provider
        .exec(
            sandbox,
            &inv.to_exec(Duration::from_secs(task.timeout_secs)),
        )
        .await?;
    let duration_ms = clock.now_millis().saturating_sub(started);
    if output.timed_out {
        return Err(RuntimeError::Timeout {
            secs: task.timeout_secs,
        });
    }

    let crashed = || RuntimeError::Crashed {
        exit_code: Some(output.exit_code),
        stderr_tail: tail(&output.stderr, STDERR_TAIL_BYTES),
    };
    let parsed = match adapter
        .collect(provider, sandbox, task, &output, clock)
        .await
    {
        Ok(p) => p,
        Err(RuntimeError::Protocol(_)) if output.exit_code != 0 => return Err(crashed()),
        Err(e) => return Err(e),
    };
    let Some(result) = parsed.result else {
        return Err(if output.exit_code != 0 {
            crashed()
        } else {
            RuntimeError::Protocol(format!(
                "{} exited 0 without its terminal record",
                adapter.name()
            ))
        });
    };

    if !adapter.writes_contract_files() {
        provider
            .put_file(sandbox, EVENTS_FILE, to_jsonl(&parsed.events))
            .await?;
        let mut bytes = serde_json::to_vec_pretty(&result)
            .map_err(|e| RuntimeError::Protocol(format!("result.json: {e}")))?;
        bytes.push(b'\n');
        provider.put_file(sandbox, RESULT_FILE, bytes).await?;
    }

    Ok(AgentOutcome {
        runtime: adapter.name().to_string(),
        result,
        events: parsed.events,
        exit_code: Some(output.exit_code),
        duration_ms,
    })
}

/// Declares a runtime struct (`new`, `with_clock`) whose `AgentRuntime::run`
/// is the shared loop over its `ProcessAdapter` impl.
macro_rules! process_runtime {
    ($(#[$meta:meta])* $ty:ident) => {
        $(#[$meta])*
        pub struct $ty {
            provider: std::sync::Arc<dyn guard_core::ports::sandbox::SandboxProvider>,
            clock: std::sync::Arc<dyn crate::clock::Clock>,
        }

        impl $ty {
            pub fn new(
                provider: std::sync::Arc<dyn guard_core::ports::sandbox::SandboxProvider>,
            ) -> Self {
                Self {
                    provider,
                    clock: std::sync::Arc::new(crate::clock::SystemClock),
                }
            }

            pub fn with_clock(mut self, clock: std::sync::Arc<dyn crate::clock::Clock>) -> Self {
                self.clock = clock;
                self
            }
        }

        #[async_trait::async_trait]
        impl guard_core::ports::agent::AgentRuntime for $ty {
            fn name(&self) -> &'static str {
                NAME
            }

            async fn run(
                &self,
                sandbox: &guard_core::ports::sandbox::Sandbox,
                task: &guard_core::ports::agent::AgentTask,
            ) -> Result<guard_core::ports::agent::AgentOutcome, guard_core::ports::agent::RuntimeError>
            {
                crate::driver::run(self, self.provider.as_ref(), self.clock.as_ref(), sandbox, task)
                    .await
            }
        }
    };
}
pub(crate) use process_runtime;
