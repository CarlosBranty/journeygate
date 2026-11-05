//! `agents.toml` (docs/contracts/agents-toml.md, `schemas/agents.schema.json`).
//!
//! [`AgentsConfig::parse`] rejects unknown keys (TOML/serde) and then runs
//! [`AgentsConfig::validate`], which reports every rule violation at once.
//! Credentials appear only as env var **names**; nothing here reads a secret.

use guard_core::ports::agent::MAX_ATTEMPTS;
use guard_core::ports::{AttemptPolicy, KeyRequest, Role};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

/// Seconds a virtual key outlives its agent's `timeout_secs`.
pub const KEY_GRACE_SECS: u64 = 600;

/// Env var that admits `runtime = "fake"` (tests only).
pub const ALLOW_FAKE_RUNTIME_ENV: &str = "GUARD_ALLOW_FAKE_RUNTIME";
pub const DEFAULT_KEY_ENV: &str = "GUARD_GATEWAY_KEY";
pub const DEFAULT_KEY_DURATION_SECS: u64 = 7200;
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;
pub const DEFAULT_LEASE_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read agents.toml: {0}")]
    Io(String),
    /// Syntax error, wrong type or unknown key.
    #[error("agents.toml: {0}")]
    Toml(String),
    /// Every rule violation found, in a stable order.
    #[error("agents.toml is invalid:\n- {}", .0.join("\n- "))]
    Invalid(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoadOptions {
    /// `runtime = "fake"` is accepted only with `GUARD_ALLOW_FAKE_RUNTIME=1`.
    pub allow_fake_runtime: bool,
}

impl LoadOptions {
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var(ALLOW_FAKE_RUNTIME_ENV).ok().as_deref())
    }

    /// Only the exact value `1` opts in.
    pub fn from_env_value(value: Option<&str>) -> Self {
        Self {
            allow_fake_runtime: value == Some("1"),
        }
    }
}

// ---------------------------------------------------------------- document

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    pub version: u32,
    pub gateway: GatewayConfig,
    pub roles: RolesConfig,
    pub agents: BTreeMap<String, AgentConfig>,
    pub scheduler: SchedulerConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub url: String,
    pub master_key_env: String,
    #[serde(default = "default_key_env")]
    pub key_env: String,
    #[serde(default = "default_key_duration")]
    pub key_duration_secs: u64,
    #[serde(default)]
    pub rpm_limit: Option<u32>,
    #[serde(default)]
    pub tpm_limit: Option<u64>,
    #[serde(default)]
    pub max_parallel_requests: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolesConfig {
    pub qa: String,
    pub coder: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    ClaudeCode,
    Codex,
    CopilotCli,
    MiniSweAgent,
    #[serde(rename = "openhands")]
    OpenHands,
    /// Tests only (`GUARD_ALLOW_FAKE_RUNTIME=1`).
    Fake,
}

impl RuntimeKind {
    pub const ALL: [RuntimeKind; 6] = [
        RuntimeKind::ClaudeCode,
        RuntimeKind::Codex,
        RuntimeKind::CopilotCli,
        RuntimeKind::MiniSweAgent,
        RuntimeKind::OpenHands,
        RuntimeKind::Fake,
    ];

    /// The `AgentRuntime::name()` of the adapter for this runtime.
    pub fn as_str(self) -> &'static str {
        match self {
            RuntimeKind::ClaudeCode => "claude-code",
            RuntimeKind::Codex => "codex",
            RuntimeKind::CopilotCli => "copilot-cli",
            RuntimeKind::MiniSweAgent => "mini-swe-agent",
            RuntimeKind::OpenHands => "openhands",
            RuntimeKind::Fake => "fake",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == name)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub role: Role,
    pub runtime: RuntimeKind,
    pub model: String,
    pub budget_usd: f64,
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_true")]
    pub via_gateway: bool,
    #[serde(default)]
    pub copilot_token_env: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub rpm_limit: Option<u32>,
    #[serde(default)]
    pub tpm_limit: Option<u64>,
    #[serde(default)]
    pub max_parallel_requests: Option<u32>,
}

impl AgentConfig {
    /// Valid after [`AgentsConfig::validate`]; clamps defensively otherwise.
    pub fn attempt_policy(&self) -> AttemptPolicy {
        AttemptPolicy::new(self.max_attempts.clamp(1, MAX_ATTEMPTS)).unwrap_or_default()
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }

    /// Provider slot this agent's attempts count against.
    pub fn provider(&self) -> String {
        provider_of(&self.model)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    pub host_slots: u32,
    pub daily_usd_cap: f64,
    #[serde(default)]
    pub provider_slots: BTreeMap<String, u32>,
    #[serde(default = "default_lease")]
    pub lease_secs: u64,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub best_of_n: Option<BestOfN>,
}

impl SchedulerConfig {
    pub fn lease(&self) -> Duration {
        Duration::from_secs(self.lease_secs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BestOfN {
    pub agents: Vec<String>,
    /// Only tickets carrying this label run best-of-N; `None` = never.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub select: Selection,
}

impl BestOfN {
    pub fn applies_to(&self, labels: &[String]) -> bool {
        self.label
            .as_deref()
            .is_some_and(|l| labels.iter().any(|x| x == l))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Selection {
    /// Among the candidates that pass the guard, the smallest diff (changed
    /// lines) wins; equal diffs are broken by who passed first.
    #[default]
    FirstGreenSmallestDiff,
}

fn default_key_env() -> String {
    DEFAULT_KEY_ENV.to_string()
}
fn default_key_duration() -> u64 {
    DEFAULT_KEY_DURATION_SECS
}
fn default_max_attempts() -> u32 {
    MAX_ATTEMPTS
}
fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECS
}
fn default_true() -> bool {
    true
}
fn default_lease() -> u64 {
    DEFAULT_LEASE_SECS
}

// ---------------------------------------------------------------- helpers

/// Provider of a gateway model name: the prefix before the first `/`
/// (`anthropic/...` → `anthropic`). A bare Claude model counts as
/// `anthropic`; any other bare name is its own provider key.
pub fn provider_of(model: &str) -> String {
    match model.split_once('/') {
        Some((prefix, _)) => prefix.to_string(),
        None if model.starts_with("claude") => "anthropic".to_string(),
        None => model.to_string(),
    }
}

/// Claude Code accepts only Claude models (agents-toml.md).
fn is_claude_model(model: &str) -> bool {
    model
        .rsplit('/')
        .next()
        .is_some_and(|m| m.starts_with("claude"))
}

pub(crate) fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub(crate) fn is_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_pinned_image(image: &str) -> bool {
    image.split_once("@sha256:").is_some_and(|(repo, d)| {
        !repo.is_empty()
            && !repo.contains(char::is_whitespace)
            && d.len() == 64
            && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn is_provider_key(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

fn positive(v: f64) -> bool {
    v.is_finite() && v > 0.0
}

// ---------------------------------------------------------------- loading

impl AgentsConfig {
    pub fn parse(text: &str, options: &LoadOptions) -> Result<Self, ConfigError> {
        let config: AgentsConfig =
            toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        config.validate(options)?;
        Ok(config)
    }

    pub fn load(path: &Path, options: &LoadOptions) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
        Self::parse(&text, options)
    }

    /// Every rule of agents-toml.md beyond the schema shape.
    pub fn validate(&self, options: &LoadOptions) -> Result<(), ConfigError> {
        let mut p = Vec::new();
        if self.version != 1 {
            p.push(format!("version must be 1, got {}", self.version));
        }
        self.validate_gateway(&mut p);
        if self.agents.is_empty() {
            p.push("agents: at least one [agents.<name>] is required".into());
        }
        for (name, agent) in &self.agents {
            self.validate_agent(name, agent, options, &mut p);
        }
        for (role, target) in [(Role::Qa, &self.roles.qa), (Role::Coder, &self.roles.coder)] {
            match self.agents.get(target) {
                None => p.push(format!(
                    "roles.{}: agent {target:?} is not defined",
                    role.as_str()
                )),
                Some(a) if a.role != role => p.push(format!(
                    "roles.{}: agent {target:?} has role {}",
                    role.as_str(),
                    a.role.as_str()
                )),
                Some(_) => {}
            }
            if !self.agents.values().any(|a| a.role == role) {
                p.push(format!("agents: no agent has role {}", role.as_str()));
            }
        }
        self.validate_scheduler(&mut p);
        if p.is_empty() {
            Ok(())
        } else {
            Err(ConfigError::Invalid(p))
        }
    }

    fn validate_gateway(&self, p: &mut Vec<String>) {
        let g = &self.gateway;
        if self.gateway_endpoint().is_none() {
            p.push(format!(
                "gateway.url {:?} must be an http(s):// URL with a host",
                g.url
            ));
        }
        for (key, v) in [
            ("master_key_env", &g.master_key_env),
            ("key_env", &g.key_env),
        ] {
            if !is_env_name(v) {
                p.push(format!(
                    "gateway.{key} {v:?} must be an env var name (A-Z, 0-9, _)"
                ));
            }
        }
        if g.key_duration_secs < 60 {
            p.push("gateway.key_duration_secs must be >= 60".into());
        }
        limits(
            "gateway",
            g.rpm_limit,
            g.tpm_limit,
            g.max_parallel_requests,
            p,
        );
    }

    fn validate_agent(
        &self,
        name: &str,
        a: &AgentConfig,
        options: &LoadOptions,
        p: &mut Vec<String>,
    ) {
        let at = format!("agents.{name}");
        if !is_agent_name(name) {
            p.push(format!("{at}: name must match ^[a-z][a-z0-9-]*$"));
        }
        if a.model.trim().is_empty() {
            p.push(format!("{at}.model is empty"));
        }
        if !positive(a.budget_usd) {
            p.push(format!("{at}.budget_usd must be a positive number"));
        }
        if let Err(e) = AttemptPolicy::new(a.max_attempts) {
            p.push(format!("{at}.max_attempts: {e}"));
        }
        if a.timeout_secs < 60 {
            p.push(format!("{at}.timeout_secs must be >= 60"));
        }
        if a.runtime == RuntimeKind::ClaudeCode && !is_claude_model(&a.model) {
            p.push(format!(
                "{at}: runtime claude-code accepts only Claude models, not {:?}",
                a.model
            ));
        }
        if a.runtime == RuntimeKind::Fake && !options.allow_fake_runtime {
            p.push(format!(
                "{at}: runtime \"fake\" is for tests only (set {ALLOW_FAKE_RUNTIME_ENV}=1)"
            ));
        }
        if !a.via_gateway {
            if a.runtime != RuntimeKind::CopilotCli {
                p.push(format!(
                    "{at}: via_gateway = false is allowed only with runtime copilot-cli"
                ));
            }
            if a.copilot_token_env.is_none() {
                p.push(format!(
                    "{at}: via_gateway = false requires copilot_token_env"
                ));
            }
        }
        if let Some(env) = &a.copilot_token_env {
            if !is_env_name(env) {
                p.push(format!(
                    "{at}.copilot_token_env {env:?} is not an env var name"
                ));
            }
        }
        match a.image.as_ref().or(self.scheduler.image.as_ref()) {
            None => p.push(format!(
                "{at}: no sandbox image (set {at}.image or scheduler.image)"
            )),
            Some(img) if !is_pinned_image(img) => p.push(format!(
                "{at}: image {img:?} must be pinned by digest (repo@sha256:<64 hex>)"
            )),
            Some(_) => {}
        }
        limits(&at, a.rpm_limit, a.tpm_limit, a.max_parallel_requests, p);
    }

    fn validate_scheduler(&self, p: &mut Vec<String>) {
        let s = &self.scheduler;
        if s.host_slots == 0 {
            p.push("scheduler.host_slots must be >= 1".into());
        }
        if !positive(s.daily_usd_cap) {
            p.push("scheduler.daily_usd_cap must be a positive number".into());
        }
        for (provider, n) in &s.provider_slots {
            if !is_provider_key(provider) {
                p.push(format!(
                    "scheduler.provider_slots: key {provider:?} must match ^[a-z0-9_-]+$"
                ));
            }
            if *n == 0 {
                p.push(format!("scheduler.provider_slots.{provider} must be >= 1"));
            }
        }
        if s.lease_secs < 30 {
            p.push("scheduler.lease_secs must be >= 30".into());
        }
        if let Some(img) = &s.image {
            if !is_pinned_image(img) {
                p.push(format!(
                    "scheduler.image {img:?} must be pinned by digest (repo@sha256:<64 hex>)"
                ));
            }
        }
        let Some(b) = &s.best_of_n else { return };
        if b.agents.len() < 2 {
            p.push("scheduler.best_of_n.agents needs at least 2 agents".into());
        }
        let unique: BTreeSet<&String> = b.agents.iter().collect();
        if unique.len() != b.agents.len() {
            p.push("scheduler.best_of_n.agents must be unique".into());
        }
        let mut roles = BTreeSet::new();
        for name in &b.agents {
            match self.agents.get(name) {
                Some(a) => {
                    roles.insert(a.role);
                }
                None => p.push(format!(
                    "scheduler.best_of_n.agents: agent {name:?} is not defined"
                )),
            }
        }
        if roles.len() > 1 {
            p.push("scheduler.best_of_n.agents must share a role".into());
        }
        if b.label.as_deref().is_some_and(|l| l.trim().is_empty()) {
            p.push("scheduler.best_of_n.label must not be empty".into());
        }
        // Each candidate runs in its own sandbox at the same time; with fewer
        // host slots the extra candidates could never start.
        if s.host_slots > 0 && b.agents.len() > s.host_slots as usize {
            p.push(format!(
                "scheduler.best_of_n runs {} sandboxes at once but host_slots is {}",
                b.agents.len(),
                s.host_slots
            ));
        }
    }

    // ------------------------------------------------------------ accessors

    /// The agent playing `role` (`[roles]`). Panics only on an unvalidated config.
    pub fn role_agent(&self, role: Role) -> (&str, &AgentConfig) {
        let name = match role {
            Role::Qa => &self.roles.qa,
            Role::Coder => &self.roles.coder,
        };
        (name.as_str(), &self.agents[name])
    }

    /// `[agents.<name>].image`, else `[scheduler].image` (validated to exist).
    pub fn image_for<'a>(&'a self, agent: &'a AgentConfig) -> &'a str {
        agent
            .image
            .as_deref()
            .or(self.scheduler.image.as_deref())
            .unwrap_or_default()
    }

    /// `(host, port)` of `[gateway].url`: the single gateway egress rule.
    pub fn gateway_endpoint(&self) -> Option<(String, u16)> {
        let url = url::Url::parse(&self.gateway.url).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        let host = url.host_str().filter(|h| !h.is_empty())?.to_string();
        Some((host, url.port_or_known_default()?))
    }

    /// The `KeyRequest` of one attempt of agent `name`: its model, budget,
    /// the gateway rate limits (the agent's own override them) and duration.
    /// `[gateway].key_duration_secs`, but never shorter than the agent's
    /// `timeout_secs` plus [`KEY_GRACE_SECS`].
    pub fn key_request(
        &self,
        name: &str,
        alias: &str,
        metadata: BTreeMap<String, String>,
    ) -> KeyRequest {
        let a = &self.agents[name];
        let g = &self.gateway;
        KeyRequest {
            alias: alias.to_string(),
            models: vec![a.model.clone()],
            max_budget_usd: a.budget_usd,
            rpm_limit: a.rpm_limit.or(g.rpm_limit),
            tpm_limit: a.tpm_limit.or(g.tpm_limit),
            max_parallel_requests: a.max_parallel_requests.or(g.max_parallel_requests),
            // The key must outlive the attempt (timeout plus collection).
            duration: Duration::from_secs(g.key_duration_secs.max(a.timeout_secs + KEY_GRACE_SECS)),
            metadata,
        }
    }

    /// Distinct runtimes of every configured agent (what `guard agents sync` renders).
    pub fn runtimes_in_use(&self) -> BTreeSet<RuntimeKind> {
        self.agents.values().map(|a| a.runtime).collect()
    }
}

fn limits(
    at: &str,
    rpm: Option<u32>,
    tpm: Option<u64>,
    parallel: Option<u32>,
    p: &mut Vec<String>,
) {
    if rpm == Some(0) {
        p.push(format!("{at}.rpm_limit must be >= 1"));
    }
    if tpm == Some(0) {
        p.push(format!("{at}.tpm_limit must be >= 1"));
    }
    if parallel == Some(0) {
        p.push(format!("{at}.max_parallel_requests must be >= 1"));
    }
}
