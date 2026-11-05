//! `SandboxProvider`: one fresh, isolated container per (ticket, role, attempt).
//!
//! Permissions are imposed by mounts and network, never by prompts
//! (architecture section 4; `docs/contracts/sandbox.md`). [`SandboxSpec::for_role`]
//! builds the role mounts of the permission matrix and [`SandboxSpec::validate`]
//! rejects any spec that would weaken them; every provider calls `validate`
//! in `create` and refuses an invalid spec with [`SandboxError::InvalidSpec`].

use super::agent::{Role, TASK_DIR, is_env_name};
use super::secret::Secret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// The worktree inside the sandbox.
pub const WORKSPACE_DIR: &str = "/workspace";
/// Read-only CodeGraph index built per checkout by the orchestrator.
pub const CODEGRAPH_DIR: &str = "/codegraph";
/// Read-only rendered skills / `AGENTS.md` for the runtime (`guard agents sync`).
pub const SKILLS_DIR: &str = "/agents";
/// Env vars every sandbox carries (telemetry off).
pub const REQUIRED_ENV: &[(&str, &str)] = &[("CODEGRAPH_TELEMETRY", "0"), ("DO_NOT_TRACK", "1")];

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SandboxId(pub String);

/// A live sandbox handle returned by [`SandboxProvider::create`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sandbox {
    pub id: SandboxId,
    pub role: Role,
    pub run_id: String,
    pub ticket: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
    /// An empty read-only tmpfs over the target: the path exists but shows nothing.
    Hidden,
}

/// A bind mount (`source` = host path; ignored for [`MountMode::Hidden`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub source: String,
    pub target: String,
    pub mode: MountMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressPurpose {
    /// The model gateway (LiteLLM). Exactly one per sandbox.
    Gateway,
    /// QA only: the preview environment Playwright MCP drives.
    Preview,
    /// Coder only: a read-only package mirror.
    PackageMirror,
    /// Copilot-hosted models (`via_gateway = false` in agents.toml).
    CopilotException,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressRule {
    pub host: String,
    pub port: u16,
    pub purpose: EgressPurpose,
}

/// Destinations the egress proxy lets through. Everything else is dropped.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EgressAllowlist {
    pub rules: Vec<EgressRule>,
}

impl EgressAllowlist {
    pub fn gateway_only(host: &str, port: u16) -> Self {
        Self {
            rules: vec![EgressRule {
                host: host.to_string(),
                port,
                purpose: EgressPurpose::Gateway,
            }],
        }
    }

    pub fn with(mut self, rule: EgressRule) -> Self {
        self.rules.push(rule);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResourceLimits {
    pub cpus: f64,
    pub memory_mb: u64,
    pub pids: u32,
    /// Wall clock of the whole sandbox; the provider kills it after this.
    pub timeout: Duration,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpus: 2.0,
            memory_mb: 4096,
            pids: 512,
            timeout: Duration::from_secs(1800),
        }
    }
}

/// Host-side inputs of [`SandboxSpec::for_role`].
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxLayout {
    pub run_id: String,
    pub ticket: String,
    pub attempt: u32,
    /// `<repo>@sha256:<64 hex>`.
    pub image: String,
    /// Host path of the worktree (branch `agent/<KEY>`).
    pub checkout: String,
    /// Host directory mounted read-write at `/task`.
    pub task_dir: String,
    /// Host directory of the CodeGraph index (read-only).
    pub codegraph_index: Option<String>,
    /// Coder only: (host path of the sealed copy, repo-relative spec path).
    pub sealed_spec: Option<(String, String)>,
    /// Repo-relative tests directory (`[playwright].tests_dir`, usually `tests`).
    pub tests_dir: String,
    /// Host directory of the rendered skills for this runtime.
    pub skills_dir: Option<String>,
}

/// Everything a provider needs to create one sandbox.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxSpec {
    pub role: Role,
    pub layout: SandboxLayout,
    pub image: String,
    pub mounts: Vec<Mount>,
    pub egress: EgressAllowlist,
    /// Non-secret env vars.
    pub env: BTreeMap<String, String>,
    /// Env vars carrying credentials (the virtual key), injected at process
    /// start and never written to disk.
    pub secrets: BTreeMap<String, Secret>,
    /// Set only for an `agents.toml` agent with `via_gateway = false`.
    pub copilot_exception: bool,
    pub limits: ResourceLimits,
}

fn join(base: &str, rel: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        rel.trim_start_matches('/')
    )
}

const SECRET_SUFFIXES: &[&str] = &["_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_CREDENTIALS"];

impl SandboxSpec {
    /// The role mounts of the permission matrix (sandbox.md), telemetry env and
    /// default limits. Callers add `secrets` and egress exceptions.
    pub fn for_role(role: Role, layout: SandboxLayout, egress: EgressAllowlist) -> Self {
        let tests = layout.tests_dir.trim_matches('/').to_string();
        let mut mounts = Vec::new();
        match role {
            Role::Coder => {
                mounts.push(Mount {
                    source: layout.checkout.clone(),
                    target: WORKSPACE_DIR.into(),
                    mode: MountMode::ReadWrite,
                });
                mounts.push(Mount {
                    source: String::new(),
                    target: join(WORKSPACE_DIR, &tests),
                    mode: MountMode::Hidden,
                });
                if let Some((host, rel)) = &layout.sealed_spec {
                    mounts.push(Mount {
                        source: host.clone(),
                        target: join(WORKSPACE_DIR, rel),
                        mode: MountMode::ReadOnly,
                    });
                }
            }
            Role::Qa => {
                mounts.push(Mount {
                    source: layout.checkout.clone(),
                    target: WORKSPACE_DIR.into(),
                    mode: MountMode::ReadOnly,
                });
                let features = format!("{tests}/features");
                mounts.push(Mount {
                    source: join(&layout.checkout, &features),
                    target: join(WORKSPACE_DIR, &features),
                    mode: MountMode::ReadWrite,
                });
            }
        }
        mounts.push(Mount {
            source: layout.task_dir.clone(),
            target: TASK_DIR.into(),
            mode: MountMode::ReadWrite,
        });
        if let Some(index) = &layout.codegraph_index {
            mounts.push(Mount {
                source: index.clone(),
                target: CODEGRAPH_DIR.into(),
                mode: MountMode::ReadOnly,
            });
        }
        if let Some(skills) = &layout.skills_dir {
            mounts.push(Mount {
                source: skills.clone(),
                target: SKILLS_DIR.into(),
                mode: MountMode::ReadOnly,
            });
        }
        let env = REQUIRED_ENV
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Self {
            role,
            image: layout.image.clone(),
            layout,
            mounts,
            egress,
            env,
            secrets: BTreeMap::new(),
            copilot_exception: false,
            limits: ResourceLimits::default(),
        }
    }

    /// Rejects any spec that weakens the isolation contract (sandbox.md).
    pub fn validate(&self) -> Result<(), String> {
        let digest_ok = self.image.split_once("@sha256:").is_some_and(|(repo, d)| {
            !repo.is_empty()
                && d.len() == 64
                && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if !digest_ok {
            return Err(format!(
                "image {:?} must be pinned by digest (@sha256:...)",
                self.image
            ));
        }
        for (k, v) in REQUIRED_ENV {
            if self.env.get(*k).map(String::as_str) != Some(*v) {
                return Err(format!("env {k} must be {v}"));
            }
        }
        for name in self.env.keys() {
            if !is_env_name(name) {
                return Err(format!("env name {name:?} is not A-Z, 0-9, _"));
            }
            if SECRET_SUFFIXES.iter().any(|s| name.ends_with(s)) {
                return Err(format!(
                    "{name} looks like a credential: secrets go in `secrets`, not env"
                ));
            }
        }
        if let Some(name) = self.secrets.keys().find(|n| !is_env_name(n)) {
            return Err(format!("secrets name {name:?} is not an env var name"));
        }
        self.validate_mounts()?;
        self.validate_egress()
    }

    fn validate_mounts(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for m in &self.mounts {
            if !m.target.starts_with('/') || m.target.split('/').any(|s| s == "..") {
                return Err(format!(
                    "mount target {:?} must be absolute without ..",
                    m.target
                ));
            }
            if !seen.insert(m.target.as_str()) {
                return Err(format!("mount target {} appears twice", m.target));
            }
        }
        let under = |path: &str, dir: &str| path == dir || path.starts_with(&format!("{dir}/"));
        for m in &self.mounts {
            if under(&m.target, CODEGRAPH_DIR) && m.mode != MountMode::ReadOnly {
                return Err("the codegraph index must be mounted read-only".into());
            }
            if under(&m.target, SKILLS_DIR) && m.mode != MountMode::ReadOnly {
                return Err("the skills directory must be mounted read-only".into());
            }
        }
        let tests = join(WORKSPACE_DIR, self.layout.tests_dir.trim_matches('/'));
        let mode_at = |target: &str| {
            self.mounts
                .iter()
                .find(|m| m.target == target)
                .map(|m| m.mode)
        };
        let rw: Vec<&str> = self
            .mounts
            .iter()
            .filter(|m| m.mode == MountMode::ReadWrite)
            .map(|m| m.target.as_str())
            .collect();
        match self.role {
            Role::Coder => {
                if mode_at(WORKSPACE_DIR) != Some(MountMode::ReadWrite) {
                    return Err("coder: /workspace must be mounted read-write".into());
                }
                if !matches!(
                    mode_at(&tests),
                    Some(MountMode::Hidden | MountMode::ReadOnly)
                ) {
                    return Err(format!(
                        "coder: {tests} must be mounted hidden or read-only"
                    ));
                }
                if let Some(t) = rw.iter().find(|t| under(t, &tests)) {
                    return Err(format!("coder: {t} under the tests dir cannot be writable"));
                }
                if let Some((_, rel)) = &self.layout.sealed_spec {
                    if mode_at(&join(WORKSPACE_DIR, rel)) != Some(MountMode::ReadOnly) {
                        return Err("coder: the sealed spec must be mounted read-only".into());
                    }
                } else {
                    return Err("coder: a sealed spec is required".into());
                }
                if let Some(t) = rw.iter().find(|t| **t != WORKSPACE_DIR && **t != TASK_DIR) {
                    return Err(format!("coder: unexpected writable mount {t}"));
                }
            }
            Role::Qa => {
                if mode_at(WORKSPACE_DIR) != Some(MountMode::ReadOnly) {
                    return Err("qa: /workspace must be mounted read-only".into());
                }
                let features = format!("{tests}/features");
                if let Some(t) = rw.iter().find(|t| **t != features && **t != TASK_DIR) {
                    return Err(format!(
                        "qa: only {features} and /task may be writable, not {t}"
                    ));
                }
                if self.layout.sealed_spec.is_some() {
                    return Err("qa: a sealed spec belongs to the coder only".into());
                }
            }
        }
        Ok(())
    }

    fn validate_egress(&self) -> Result<(), String> {
        let gateways = self
            .egress
            .rules
            .iter()
            .filter(|r| r.purpose == EgressPurpose::Gateway)
            .count();
        if gateways != 1 {
            return Err(format!(
                "egress must contain exactly one gateway rule, found {gateways}"
            ));
        }
        for r in &self.egress.rules {
            match (r.purpose, self.role) {
                (EgressPurpose::Preview, Role::Coder) => {
                    return Err("coder egress cannot reach the preview environment".into());
                }
                (EgressPurpose::PackageMirror, Role::Qa) => {
                    return Err("qa egress cannot reach the package mirror".into());
                }
                (EgressPurpose::CopilotException, _) if !self.copilot_exception => {
                    return Err("copilot egress requires copilot_exception = true".into());
                }
                _ => {}
            }
            if r.host.is_empty() || r.host.contains('/') || r.host.contains('*') {
                return Err(format!(
                    "egress host {:?} must be a plain host name",
                    r.host
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecRequest {
    /// argv, run directly (never through a shell).
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Default [`WORKSPACE_DIR`].
    pub workdir: Option<String>,
    /// Default: the remaining sandbox timeout.
    pub timeout: Option<Duration>,
    pub stdin: Option<Vec<u8>>,
}

impl ExecRequest {
    pub fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExecOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

/// The worktree diff against `base_commit` (tracked and untracked files,
/// `git diff --no-renames --binary`), taken by the orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedDiff {
    pub base_commit: String,
    pub unified_diff: String,
    /// Sorted, unique, repo-relative.
    pub changed_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SandboxError {
    #[error("invalid sandbox spec: {0}")]
    InvalidSpec(String),
    /// Docker daemon / VM host unreachable, image missing.
    #[error("sandbox backend unavailable: {0}")]
    Unavailable(String),
    /// Unknown or destroyed sandbox, or a missing file inside it.
    #[error("not found: {0}")]
    NotFound(String),
    #[error("exec failed: {0}")]
    Exec(String),
    #[error("sandbox io error: {0}")]
    Io(String),
}

#[async_trait::async_trait]
pub trait SandboxProvider: Send + Sync {
    /// Validates `spec` and starts a fresh container (rootless, `--read-only`
    /// rootfs, `--cap-drop=ALL`, `no-new-privileges`, internal network behind
    /// the egress proxy).
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox, SandboxError>;

    /// Writes a file inside a writable mount (the orchestrator writes `/task/task.json`).
    async fn put_file(
        &self,
        sandbox: &Sandbox,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SandboxError>;

    /// Reads a file (`/task/result.json`, `/task/events.jsonl`). Missing = `NotFound`.
    async fn read_file(&self, sandbox: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError>;

    /// Runs argv inside the sandbox. A non-zero exit is `Ok` with that code.
    async fn exec(
        &self,
        sandbox: &Sandbox,
        request: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError>;

    /// The diff of the worktree against `base_commit`.
    async fn extract_diff(
        &self,
        sandbox: &Sandbox,
        base_commit: &str,
    ) -> Result<ExtractedDiff, SandboxError>;

    /// Stops and removes the container and its volumes. Idempotent.
    async fn destroy(&self, sandbox: &Sandbox) -> Result<(), SandboxError>;
}
