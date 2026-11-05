//! `AgentRuntime` and the agent-lane types (architecture sections 4 and 16).
//!
//! A runtime (Claude Code, Codex CLI, Copilot CLI, mini-swe-agent, OpenHands)
//! is only the agent loop. The security boundary is the sandbox
//! ([`super::sandbox`]): mounts, egress and the diff path allowlist. Every
//! runtime speaks the same in-sandbox file contract
//! (`docs/contracts/agent-task.md`):
//!
//! - in : [`TASK_FILE`] — an [`AgentTask`] as JSON (`schemas/agent-task.schema.json`)
//! - out: [`EVENTS_FILE`] — one [`RuntimeEvent`] per line (`schemas/agent-event.schema.json`)
//! - out: [`RESULT_FILE`] — a [`TaskResult`] (`schemas/agent-result.schema.json`)
//!
//! The diff is never reported by the runtime: the orchestrator extracts it
//! from the worktree with [`super::sandbox::SandboxProvider::extract_diff`].

use super::memory_facts::FactsBrief;
use super::sandbox::{Sandbox, SandboxError};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Directory of the file contract inside every sandbox.
pub const TASK_DIR: &str = "/task";
pub const TASK_FILE: &str = "/task/task.json";
pub const EVENTS_FILE: &str = "/task/events.jsonl";
pub const RESULT_FILE: &str = "/task/result.json";
/// `version` of `task.json` and `result.json`.
pub const TASK_SCHEMA_VERSION: u32 = 1;
/// Hard ceiling of Coder attempts per ticket (architecture section 4).
pub const MAX_ATTEMPTS: u32 = 3;

// ---------------------------------------------------------------- role

/// The two agent roles of lane A.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Writes `tests/features/<KEY>.spec.ts` red-first; nothing else.
    Qa,
    /// Writes app code against the sealed spec; never tests.
    Coder,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Qa => "qa",
            Role::Coder => "coder",
        }
    }

    pub fn from_name(name: &str) -> Option<Role> {
        [Role::Qa, Role::Coder]
            .into_iter()
            .find(|r| r.as_str() == name)
    }
}

// ---------------------------------------------------------------- sealing

/// The QA spec after red-first, sealed by the orchestrator outside the sandbox.
/// `path` is repo-relative; `sha256` is lowercase hex of the exact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSpec {
    pub path: String,
    pub sha256: String,
}

impl SealedSpec {
    pub fn seal(path: &str, bytes: &[u8]) -> Self {
        Self {
            path: path.to_string(),
            sha256: hex_sha256(bytes),
        }
    }

    /// `true` when `bytes` are exactly the sealed content.
    pub fn verify(&self, bytes: &[u8]) -> bool {
        hex_sha256(bytes) == self.sha256
    }

    /// 64 lowercase hex characters.
    pub fn is_valid_digest(digest: &str) -> bool {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------- attempts

/// Attempt budget of one (ticket, role). `1 <= max_attempts <= MAX_ATTEMPTS`,
/// enforced on construction and on deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawAttemptPolicy")]
pub struct AttemptPolicy {
    max_attempts: u32,
}

#[derive(Deserialize)]
struct RawAttemptPolicy {
    max_attempts: u32,
}

impl TryFrom<RawAttemptPolicy> for AttemptPolicy {
    type Error = String;
    fn try_from(raw: RawAttemptPolicy) -> Result<Self, String> {
        AttemptPolicy::new(raw.max_attempts)
    }
}

impl Default for AttemptPolicy {
    fn default() -> Self {
        Self {
            max_attempts: MAX_ATTEMPTS,
        }
    }
}

impl AttemptPolicy {
    pub fn new(max_attempts: u32) -> Result<Self, String> {
        if (1..=MAX_ATTEMPTS).contains(&max_attempts) {
            Ok(Self { max_attempts })
        } else {
            Err(format!(
                "max_attempts must be in 1..={MAX_ATTEMPTS}, got {max_attempts}"
            ))
        }
    }

    pub fn max_attempts(self) -> u32 {
        self.max_attempts
    }

    /// `attempt` is 1-based.
    pub fn allows(self, attempt: u32) -> bool {
        (1..=self.max_attempts).contains(&attempt)
    }
}

// ---------------------------------------------------------------- paths

/// Repo-relative globs (globset, `literal_separator = true`) a diff may touch.
/// A changed path is a violation when it is absolute, has a `..` segment,
/// matches any `deny` glob, or matches no `write` glob. `deny` wins.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AllowedPaths {
    pub write: Vec<String>,
    pub deny: Vec<String>,
}

/// Files the Coder may read but never change (architecture section 4 matrix).
/// Patterns match at any depth (a monorepo package's manifest, the Playwright
/// project's config), except the root-only CI files.
pub const CODER_PROTECTED_FILES: &[&str] = &[
    "**/package.json",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/pnpm-workspace.yaml",
    "**/yarn.lock",
    "**/bun.lockb",
    "**/bun.lock",
    "**/.npmrc",
    "**/target.toml",
    "**/thresholds.toml",
    "**/agents.toml",
    "**/playwright.config.*",
    "**/.github/**",
    "**/.gitlab-ci.yml",
    ".gitlab/**",
];

impl AllowedPaths {
    /// QA writes exactly `<tests_dir>/features/<ticket>.spec.ts`.
    pub fn qa(tests_dir: &str, ticket: &str) -> Self {
        Self {
            write: vec![format!("{}/features/{ticket}.spec.ts", trim_dir(tests_dir))],
            deny: vec![],
        }
    }

    /// Coder writes `app_paths` and never `<tests_dir>/**`, the sealed spec or
    /// [`CODER_PROTECTED_FILES`].
    pub fn coder(app_paths: &[String], tests_dir: &str, sealed: &SealedSpec) -> Self {
        let mut deny = vec![format!("{}/**", trim_dir(tests_dir)), sealed.path.clone()];
        deny.extend(CODER_PROTECTED_FILES.iter().map(|s| s.to_string()));
        Self {
            write: app_paths.to_vec(),
            deny,
        }
    }

    /// The offending paths, in input order. `Err` = an invalid glob.
    pub fn violations(&self, changed: &[String]) -> Result<Vec<String>, String> {
        let write = build_set(&self.write)?;
        let deny = build_set(&self.deny)?;
        Ok(changed
            .iter()
            .filter(|p| {
                !is_safe_relative(p) || deny.is_match(p.as_str()) || !write.is_match(p.as_str())
            })
            .cloned()
            .collect())
    }
}

fn trim_dir(dir: &str) -> &str {
    dir.trim_end_matches('/')
}

fn build_set(globs: &[String]) -> Result<GlobSet, String> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        b.add(
            GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("invalid glob {g:?}: {e}"))?,
        );
    }
    b.build().map_err(|e| e.to_string())
}

fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains(':')
        && !path.split('/').any(|s| s == ".." || s.is_empty())
}

pub(crate) fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

// ---------------------------------------------------------------- task

/// How the runtime reaches models. The key itself is injected as the env var
/// named `key_env`; `task.json` never holds a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayBinding {
    /// The only model endpoint the sandbox egress allows.
    pub base_url: String,
    /// Env var name holding the virtual key (e.g. `GUARD_GATEWAY_KEY`).
    pub key_env: String,
    /// `false` only for the explicit Copilot-hosted exception (agents-toml.md).
    pub via_gateway: bool,
}

/// `/task/task.json`. Field names are the contract (`schemas/agent-task.schema.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentTask {
    /// [`TASK_SCHEMA_VERSION`].
    pub version: u32,
    pub run_id: String,
    pub ticket: String,
    pub role: Role,
    /// 1-based attempt of this (ticket, role).
    pub attempt: u32,
    pub attempt_policy: AttemptPolicy,
    /// `owner/name` of the app repo.
    pub repo: String,
    /// Commit the worktree was checked out at.
    pub base_commit: String,
    pub summary: String,
    pub acceptance_criteria: Vec<String>,
    pub allowed_paths: AllowedPaths,
    /// Required for [`Role::Coder`], absent for [`Role::Qa`].
    pub sealed_spec: Option<SealedSpec>,
    /// Gateway model name.
    pub model: String,
    pub budget_usd: f64,
    pub timeout_secs: u64,
    pub gateway: GatewayBinding,
    /// Fixed tool set of the task, in a stable order (deterministic prompt prefix, tokens.md).
    pub tools: Vec<String>,
    /// Byte-capped orchestrator memory (memory.md).
    pub facts_brief: Option<FactsBrief>,
    /// Retry only: what failed in the previous attempt (trace summary, never journey sources).
    pub failure_context: Option<String>,
}

impl AgentTask {
    /// Structural rules every runtime and the harness rely on.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != TASK_SCHEMA_VERSION {
            return Err(format!("version must be {TASK_SCHEMA_VERSION}"));
        }
        for (name, v) in [
            ("run_id", &self.run_id),
            ("ticket", &self.ticket),
            ("repo", &self.repo),
            ("model", &self.model),
            ("gateway.base_url", &self.gateway.base_url),
        ] {
            if v.trim().is_empty() {
                return Err(format!("{name} is empty"));
            }
        }
        let sha_ok = (7..=64).contains(&self.base_commit.len())
            && self
                .base_commit
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !sha_ok {
            return Err("base_commit must be a lowercase hex commit sha".into());
        }
        if !self.attempt_policy.allows(self.attempt) {
            return Err(format!(
                "attempt {} outside 1..={}",
                self.attempt,
                self.attempt_policy.max_attempts()
            ));
        }
        if !(self.budget_usd.is_finite() && self.budget_usd > 0.0) {
            return Err("budget_usd must be a positive finite number".into());
        }
        if self.timeout_secs == 0 {
            return Err("timeout_secs must be positive".into());
        }
        if !is_env_name(&self.gateway.key_env) {
            return Err("gateway.key_env must be an env var name (A-Z, 0-9, _)".into());
        }
        if self.allowed_paths.write.is_empty() {
            return Err("allowed_paths.write is empty".into());
        }
        match (self.role, &self.sealed_spec) {
            (Role::Coder, None) => return Err("sealed_spec is required for the coder role".into()),
            (Role::Qa, Some(_)) => return Err("sealed_spec must be absent for the qa role".into()),
            (Role::Coder, Some(s)) => {
                if !SealedSpec::is_valid_digest(&s.sha256) {
                    return Err("sealed_spec.sha256 must be 64 lowercase hex chars".into());
                }
                if !self.allowed_paths.deny.contains(&s.path) {
                    return Err("allowed_paths.deny must contain the sealed spec path".into());
                }
            }
            (Role::Qa, None) => {}
        }
        if let Some(brief) = &self.facts_brief {
            brief.validate()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- result

/// Token usage, summed per model call. Cache fields follow provider usage:
/// `cached_input_tokens` = cache reads (OpenAI `cached_tokens`, Anthropic
/// `cache_read_input_tokens`), `cache_creation_input_tokens` = Anthropic writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    pub fn sum<'a>(items: impl IntoIterator<Item = &'a Usage>) -> Usage {
        items.into_iter().fold(Usage::default(), |acc, u| Usage {
            input_tokens: acc.input_tokens + u.input_tokens,
            output_tokens: acc.output_tokens + u.output_tokens,
            cached_input_tokens: acc.cached_input_tokens + u.cached_input_tokens,
            cache_creation_input_tokens: acc.cache_creation_input_tokens
                + u.cache_creation_input_tokens,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// The agent believes it is done (the gate decides whether it passed).
    Completed,
    /// The agent gave up or hit an unrecoverable error.
    Failed,
    BudgetExceeded,
    TimedOut,
    /// The agent declined the task (unsafe, unclear criteria).
    Refused,
}

/// A fact an agent suggests. It only reaches the human-approval candidate
/// queue (memory.md); agents never write memory facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProposal {
    pub topic_key: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// `/task/result.json` (`schemas/agent-result.schema.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskResult {
    pub version: u32,
    pub status: TaskStatus,
    pub summary: String,
    pub usage: Usage,
    pub cost_usd: f64,
    pub steps: u32,
    #[serde(default)]
    pub memory_proposals: Vec<MemoryProposal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One line of `/task/events.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeEvent {
    /// 1-based, strictly increasing within a file.
    pub seq: u64,
    /// RFC 3339 UTC.
    pub ts: String,
    #[serde(flatten)]
    pub event: RuntimeEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeEventKind {
    ModelCall {
        model: String,
        usage: Usage,
        cost_usd: f64,
    },
    ToolCall {
        tool: String,
        ok: bool,
        duration_ms: u64,
    },
    /// Assistant text, truncated by the runtime to 2000 bytes.
    Message {
        text: String,
    },
    Error {
        message: String,
    },
}

impl RuntimeEvent {
    pub fn usage(&self) -> Option<&Usage> {
        match &self.event {
            RuntimeEventKind::ModelCall { usage, .. } => Some(usage),
            _ => None,
        }
    }
}

/// What a runtime returns: the parsed result plus process facts.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutcome {
    /// [`AgentRuntime::name`] of the runtime that ran.
    pub runtime: String,
    pub result: TaskResult,
    pub events: Vec<RuntimeEvent>,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

// ---------------------------------------------------------------- port

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RuntimeError {
    /// Binary missing from the image, unsupported version, misconfiguration.
    #[error("runtime unavailable: {0}")]
    Unavailable(String),
    /// [`AgentTask::validate`] failed; the runtime never started.
    #[error("invalid task: {0}")]
    InvalidTask(String),
    #[error("runtime timed out after {secs}s")]
    Timeout { secs: u64 },
    /// The process died without writing a valid `result.json`.
    #[error("runtime crashed (exit {exit_code:?}): {stderr_tail}")]
    Crashed {
        exit_code: Option<i32>,
        stderr_tail: String,
    },
    /// `result.json` or `events.jsonl` missing or not matching the schema.
    #[error("file contract violated: {0}")]
    Protocol(String),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
}

/// One agent loop. Implementations write [`TASK_FILE`] into the sandbox, start
/// the runtime non-interactively pointed at `task.gateway`, wait at most
/// `task.timeout_secs`, then read [`EVENTS_FILE`] and [`RESULT_FILE`].
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Stable id used in `agents.toml` `runtime = "..."` (e.g. `claude-code`).
    fn name(&self) -> &'static str;

    async fn run(&self, sandbox: &Sandbox, task: &AgentTask) -> Result<AgentOutcome, RuntimeError>;
}
