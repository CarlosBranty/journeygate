//! Agent runtimes and the model gateway client (architecture section 16).
//!
//! - One `guard_core::ports::AgentRuntime` per runtime: `claude-code`
//!   (`claude -p`), `codex` (`codex exec`), `copilot-cli` (`copilot -p`),
//!   `mini-swe-agent`, `openhands`, plus `fake` for end-to-end tests. Each
//!   runs inside a sandbox through `SandboxProvider::exec`, speaks the `/task`
//!   file contract (`docs/contracts/agent-task.md`) and is pinned to the
//!   gateway ([`endpoint`]). Tests use `FakeSandboxProvider` with fixtures
//!   under `tests/fixtures/<runtime>/`; no real provider is ever called.
//! - [`gateway::LiteLlmGateway`]: `guard_core::ports::ModelGateway` over
//!   `/key/generate`, `/key/delete` and `/spend/logs/v2`
//!   (`docs/contracts/tokens.md`), tested against wiremock. The master key is
//!   read from the env var named by `[gateway].master_key_env` in `agents.toml`.
//!
//! Every adapter is split in two pure, unit-tested halves, glued by one shared
//! run loop (`driver`):
//! - `invocation(&AgentTask) -> Invocation`: the exact argv, env, stdin and
//!   config files (golden-tested);
//! - `parse_*`: the runtime's native output into [`RuntimeEvent`]s and a
//!   [`TaskResult`] (fixture-tested).
//!
//! CLI runtimes do not write `/task/events.jsonl` and `/task/result.json`
//! themselves; the run loop writes both from the parsed output, so the
//! orchestrator always finds the file contract.
//!
//! [`RuntimeEvent`]: guard_core::ports::RuntimeEvent
//! [`TaskResult`]: guard_core::ports::TaskResult

pub mod claude_code;
pub mod clock;
pub mod codex;
pub mod copilot;
mod driver;
pub mod endpoint;
mod events;
pub mod fake;
pub mod gateway;
mod invocation;
pub mod mini_swe_agent;
pub mod openhands;
pub mod prompt;

pub use events::Parsed;
pub use invocation::{Invocation, SandboxFile};

use guard_core::ports::agent::{AgentRuntime, AgentTask};
use guard_core::ports::sandbox::SandboxProvider;
use std::sync::Arc;

/// Every `runtime = "..."` value `agents.toml` accepts, in contract order.
pub const RUNTIME_NAMES: [&str; 6] = [
    claude_code::NAME,
    codex::NAME,
    copilot::NAME,
    mini_swe_agent::NAME,
    openhands::NAME,
    fake::NAME,
];

/// How far a runtime's gateway pinning has been confirmed (architecture
/// section 16: a runtime is admitted only if it can be pinned to the gateway).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Pinning confirmed against the runtime's current documentation.
    Admitted,
    /// Part of the pinning is unconfirmed (see the adapter module docs).
    Provisional,
    /// The scripted `fake` runtime; `guard agents` rejects it outside
    /// `GUARD_ALLOW_FAKE_RUNTIME=1`.
    TestOnly,
}

pub fn admission(runtime: &str) -> Option<Admission> {
    match runtime {
        claude_code::NAME | codex::NAME | mini_swe_agent::NAME => Some(Admission::Admitted),
        copilot::NAME | openhands::NAME => Some(Admission::Provisional),
        fake::NAME => Some(Admission::TestOnly),
        _ => None,
    }
}

/// The adapter for `runtime`, running in sandboxes of `provider`.
pub fn runtime_for(
    runtime: &str,
    provider: Arc<dyn SandboxProvider>,
) -> Option<Arc<dyn AgentRuntime>> {
    let rt: Arc<dyn AgentRuntime> = match runtime {
        claude_code::NAME => Arc::new(claude_code::ClaudeCodeRuntime::new(provider)),
        codex::NAME => Arc::new(codex::CodexRuntime::new(provider)),
        copilot::NAME => Arc::new(copilot::CopilotRuntime::new(provider)),
        mini_swe_agent::NAME => Arc::new(mini_swe_agent::MiniSweAgentRuntime::new(provider)),
        openhands::NAME => Arc::new(openhands::OpenHandsRuntime::new(provider)),
        fake::NAME => Arc::new(fake::FakeRuntime::new(provider)),
        _ => return None,
    };
    Some(rt)
}

/// Env var names under which the sandbox must carry the virtual key **in
/// addition to** `task.gateway.key_env`, for runtimes that cannot be told
/// which variable to read. The orchestrator adds them to
/// `SandboxSpec::secrets` with the same `Secret`.
///
/// - `claude-code`: none (`apiKeyHelper` = `printenv <key_env>`).
/// - `codex`: none (`model_providers.<id>.env_key = <key_env>`).
/// - `copilot-cli`: none through the gateway
///   (`COPILOT_PROVIDER_API_KEY_COMMAND`); with the hosted exception the CLI
///   only reads `COPILOT_GITHUB_TOKEN`.
/// - `mini-swe-agent`: `LITELLM_PROXY_API_KEY` (the `litellm_proxy/` provider
///   of the LiteLLM SDK reads only that variable).
/// - `openhands`: none (the driver reads `key_env`).
pub fn secret_env_aliases(runtime: &str, task: &AgentTask) -> Vec<String> {
    let key_env = task.gateway.key_env.as_str();
    let alias = match runtime {
        mini_swe_agent::NAME => mini_swe_agent::LITELLM_KEY_ENV,
        copilot::NAME if !task.gateway.via_gateway => copilot::GITHUB_TOKEN_ENV,
        _ => return vec![],
    };
    if alias == key_env {
        vec![]
    } else {
        vec![alias.to_string()]
    }
}
