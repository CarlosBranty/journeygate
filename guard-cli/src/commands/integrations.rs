//! `guard status --integrations [--json]`: the state of every integration of
//! the E2E flow (Jira, `guard serve` with the triage and the gatekeeper, the
//! agent queue, the model gateway, the code host, CI and the local app), as
//! the GUI's Flow view shows it on its nodes.
//!
//! Read-only: it never writes a file, migrates a database or starts anything.
//! Every network probe is a cheap health or auth check (`/healthz`,
//! `/health/liveliness`, `GET /myself`, `GET /user`, one `SELECT`), bounded by
//! [`PROBE_TIMEOUT`], and they all run in parallel. Secrets are only handed to
//! the probe that needs them: the report names variables, never their values,
//! and no error quotes a URL that may carry credentials (`DATABASE_URL`).
//!
//! [`gather`] reads the facts (files and environment), [`collect`] runs the
//! probes through the [`Prober`] port, and [`assemble`] is the pure mapping
//! from both to the report.

use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::settings::load_thresholds;
use guard_core::config::{CodeHostKind, TargetConfig};
use guard_core::thresholds::Mode;
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

#[cfg(test)]
mod tests;

/// The longest any one probe may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// The state of one integration, as the Flow view badges it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Configured and its probe answered.
    Connected,
    /// Configured; nothing cheap to probe (or only partly set up).
    Configured,
    NotConfigured,
    /// Configured, but its probe got no answer.
    Disconnected,
    /// Configured, and the answer was a refusal or an invalid file.
    Error,
    /// The agent lane has at least one task running.
    Working,
}

impl State {
    fn text(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Configured => "configured",
            Self::NotConfigured => "not configured",
            Self::Disconnected => "disconnected",
            Self::Error => "error",
            Self::Working => "working",
        }
    }
}

/// A running task of the agent queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentTask {
    pub ticket: String,
    pub role: String,
    /// The PR branch the lane works on (`agent/<KEY>`).
    pub branch: String,
    pub attempts: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Integration {
    pub id: &'static str,
    /// The Flow nodes it belongs to.
    pub nodes: Vec<&'static str>,
    pub label: &'static str,
    pub state: State,
    pub detail: String,
    /// What to run or edit to set it up (or fix it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configure: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<AgentTask>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub integrations: Vec<Integration>,
}

/// What one probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// 2xx; the body (health) or the identity (auth checks).
    Ok(String),
    /// 401 / 403.
    Rejected(u16),
    /// Another non-2xx status.
    Http(u16),
    /// No connection; a short reason that never quotes the URL.
    Unreachable(String),
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeHostFacts {
    pub kind: CodeHostKind,
    pub api_url: String,
    /// Only ever passed to the token check.
    pub token: Option<String>,
}

/// Everything read from files and the environment. Holds secrets (tokens,
/// `DATABASE_URL`) for the probes, so it is deliberately not `Debug`.
pub struct Facts {
    /// `[tracker]` of target.toml and its `project`.
    pub tracker: Option<Option<String>>,
    pub jira_base_url: Option<String>,
    pub jira_email: Option<String>,
    pub jira_token: Option<String>,
    /// `[server].url`.
    pub server_url: Option<String>,
    /// The thresholds file (shown path) and its mode; `Ok(None)`: defaults.
    pub thresholds: Result<Option<(String, Mode)>, String>,
    pub jev_key: bool,
    pub jev_fake: bool,
    /// `agents.toml`: its gateway URL, or why it is invalid; `None`: absent.
    pub agents: Option<Result<String, String>>,
    /// `GUARD_GATEWAY_ADMIN_URL`.
    pub gateway_override: Option<String>,
    pub database_url: Option<String>,
    pub agents_md: bool,
    pub code_host: Option<CodeHostFacts>,
    /// CI files (relative, `/`-separated) that run `guard gate`.
    pub ci: Vec<String>,
    /// `[app]` readiness URL.
    pub app_url: String,
    /// The repo slug the agent queue is keyed by.
    pub repo: String,
}

/// The probes [`collect`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeId {
    Jira,
    Server,
    Gateway,
    Queue,
    CodeHost,
    App,
}

/// What [`collect`] found; `None` for a probe that did not run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probed {
    pub jira: Option<Probe>,
    pub server: Option<Probe>,
    pub gateway: Option<Probe>,
    pub code_host: Option<Probe>,
    pub app: Option<Probe>,
    pub queue: Option<Result<Vec<AgentTask>, String>>,
}

/// The network side, injected so tests use stubs.
#[async_trait::async_trait]
pub trait Prober: Send + Sync {
    /// `GET /rest/api/3/myself`: the display name.
    async fn jira(&self, base_url: &str, email: &str, token: &str) -> Probe;
    /// One unauthenticated `GET`.
    async fn get(&self, url: &str) -> Probe;
    /// `GET /user`: the login.
    async fn code_host(&self, kind: CodeHostKind, api_url: &str, token: &str) -> Probe;
    /// The running tasks of `repo` on the Postgres queue.
    async fn queue(&self, database_url: &str, repo: &str) -> Result<Vec<AgentTask>, String>;
}

pub fn run(ctx: &Context, json: bool) -> CliResult<u8> {
    let config = ctx.load_config()?;
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let facts = gather(ctx, &config, &env);
    let probed =
        crate::runtime::block_on(collect(&facts, &Network::new(PROBE_TIMEOUT), PROBE_TIMEOUT));
    let report = Report {
        integrations: assemble(&facts, &probed),
    };
    let mut out = io::stdout().lock();
    let written = if json {
        serde_json::to_string_pretty(&report)
            .map_err(io::Error::other)
            .and_then(|text| writeln!(out, "{text}"))
    } else {
        write!(out, "{}", render(&report))
    };
    written.map_err(|e| CliError::env(format!("cannot write to stdout: {e}")))?;
    Ok(EXIT_OK)
}

/// Reads the facts: target.toml, thresholds.toml, agents.toml, AGENTS.md,
/// the CI files and the environment (after the credentials file load).
pub fn gather(ctx: &Context, config: &TargetConfig, env: &dyn Fn(&str) -> Option<String>) -> Facts {
    let thresholds_file =
        crate::settings::thresholds_path(&ctx.root, None, env(crate::settings::THRESHOLDS_ENV));
    let thresholds = match load_thresholds(ctx, None) {
        Ok(t) => Ok(thresholds_file.map(|p| (ctx.display(&p), t.mode))),
        Err(e) => Err(e.messages.join("; ")),
    };
    let agents_path = crate::agent_lane::agents_toml_path(ctx);
    let agents = agents_path.is_file().then(|| {
        guard_agents::config::AgentsConfig::load(
            &agents_path,
            &guard_agents::config::LoadOptions::from_env(),
        )
        .map(|a| a.gateway.url)
        .map_err(|e| e.to_string())
    });
    let code_host = config.code_host.as_ref().map(|host| {
        let (url_env, token_env, default) = match host.kind {
            CodeHostKind::Github => (
                "GITHUB_API_URL",
                "GITHUB_TOKEN",
                super::connect::GITHUB_DEFAULT_API_URL,
            ),
            CodeHostKind::Gitlab => (
                "GITLAB_API_URL",
                "GITLAB_TOKEN",
                super::connect::GITLAB_DEFAULT_API_URL,
            ),
        };
        CodeHostFacts {
            kind: host.kind,
            api_url: env(url_env).unwrap_or_else(|| default.to_string()),
            token: env(token_env),
        }
    });
    let service_dirs: Vec<String> = config.services.iter().map(|s| s.cwd.clone()).collect();
    Facts {
        tracker: config.tracker.as_ref().map(|t| t.project.clone()),
        jira_base_url: env("JIRA_BASE_URL"),
        jira_email: env("JIRA_EMAIL"),
        jira_token: env(guard_tracker::config::DEFAULT_TOKEN_VAR),
        server_url: config.server.as_ref().map(|s| s.url.clone()),
        thresholds,
        jev_key: env(guard_jev::API_KEY_ENV).is_some(),
        jev_fake: env(super::serve::JEV_FAKE_ENV).is_some_and(|v| v == "1" || v == "true"),
        agents,
        gateway_override: env(crate::agent_lane::GATEWAY_ADMIN_URL_ENV),
        database_url: env(guard_store::DATABASE_URL_ENV),
        agents_md: ctx.root.join("AGENTS.md").is_file(),
        code_host,
        ci: ci_files(&ctx.root, &service_dirs),
        app_url: config.ready_url(),
        repo: crate::agent_lane::repo_slug(ctx, config),
    }
}

/// Whether a CI file runs the guard gate (the steps or the GitLab component).
pub fn runs_guard_gate(text: &str) -> bool {
    text.contains("guard gate") || text.contains("guard-ci/gate")
}

/// The CI files of `root` and of each service dir that run `guard gate`:
/// `.github/workflows/*.y(a)ml` and `.gitlab-ci.yml`, sorted per dir.
pub fn ci_files(root: &Path, dirs: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for dir in std::iter::once(".".to_string()).chain(dirs.iter().cloned()) {
        let base = root.join(&dir);
        let Ok(key) = fs::canonicalize(&base) else {
            continue;
        };
        if !seen.insert(key) {
            continue;
        }
        let prefix = if dir == "." || dir.is_empty() {
            String::new()
        } else {
            format!("{}/", dir.trim_end_matches(['/', '\\']).replace('\\', "/"))
        };
        let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
        if let Ok(entries) = fs::read_dir(base.join(".github").join("workflows")) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(".yml") || name.ends_with(".yaml") {
                    candidates.push((format!(".github/workflows/{name}"), entry.path()));
                }
            }
        }
        candidates.sort();
        candidates.push((".gitlab-ci.yml".into(), base.join(".gitlab-ci.yml")));
        for (shown, path) in candidates {
            if fs::read_to_string(&path).is_ok_and(|t| runs_guard_gate(&t)) {
                found.push(format!("{prefix}{shown}"));
            }
        }
    }
    found
}

/// The gateway the lane talks to: `GUARD_GATEWAY_ADMIN_URL`, else
/// agents.toml's `[gateway].url`.
pub fn gateway_url(f: &Facts) -> Option<String> {
    f.gateway_override
        .clone()
        .or_else(|| f.agents.as_ref().and_then(|a| a.as_ref().ok().cloned()))
}

fn postgres(url: &str) -> bool {
    url.starts_with("postgres://") || url.starts_with("postgresql://")
}

/// Why `JIRA_BASE_URL` must not get the token: plain http to another
/// machine (the Basic-auth header would cross the network in cleartext), or
/// credentials inside the URL. `None`: unset or safe.
fn jira_url_error(f: &Facts) -> Option<String> {
    let url = f.jira_base_url.as_deref()?;
    super::connect::check_url("JIRA_BASE_URL", url.trim())
        .err()
        .map(|e| e.messages.join("; "))
}

fn jira_credentials(f: &Facts) -> Option<(&str, &str, &str)> {
    if jira_url_error(f).is_some() {
        return None;
    }
    Some((
        f.jira_base_url.as_deref()?,
        f.jira_email.as_deref()?,
        f.jira_token.as_deref()?,
    ))
}

/// The probes the facts call for, in report order.
pub fn plan(f: &Facts) -> Vec<ProbeId> {
    let mut out = Vec::new();
    if f.tracker.is_some() && jira_credentials(f).is_some() {
        out.push(ProbeId::Jira);
    }
    if f.server_url.is_some() {
        out.push(ProbeId::Server);
    }
    let lane = matches!(f.agents, Some(Ok(_)));
    if lane && f.database_url.as_deref().is_some_and(postgres) {
        out.push(ProbeId::Queue);
    }
    if gateway_url(f).is_some() {
        out.push(ProbeId::Gateway);
    }
    out.push(ProbeId::App);
    if f.code_host.as_ref().is_some_and(|h| h.token.is_some()) {
        out.push(ProbeId::CodeHost);
    }
    out
}

fn trim(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// Runs every planned probe in parallel, each bounded by `timeout`.
pub async fn collect(f: &Facts, prober: &dyn Prober, timeout: Duration) -> Probed {
    let planned = plan(f);
    let on = |id| planned.contains(&id);
    let timed = |fut| bounded(timeout, fut);
    let jira = async {
        match jira_credentials(f).filter(|_| on(ProbeId::Jira)) {
            Some((url, email, token)) => Some(timed(prober.jira(url, email, token)).await),
            None => None,
        }
    };
    let server = async {
        match f.server_url.as_deref().filter(|_| on(ProbeId::Server)) {
            Some(url) => {
                let url = format!("{}/healthz", trim(url));
                Some(bounded(timeout, prober.get(&url)).await)
            }
            None => None,
        }
    };
    let gateway_base = gateway_url(f);
    let gateway = async {
        match gateway_base.as_deref().filter(|_| on(ProbeId::Gateway)) {
            Some(url) => {
                let url = format!("{}/health/liveliness", trim(url));
                Some(bounded(timeout, prober.get(&url)).await)
            }
            None => None,
        }
    };
    let app = async { Some(timed(prober.get(&f.app_url)).await) };
    let code_host = async {
        match f.code_host.as_ref().filter(|_| on(ProbeId::CodeHost)) {
            Some(CodeHostFacts {
                kind,
                api_url,
                token: Some(token),
            }) => Some(timed(prober.code_host(*kind, api_url, token)).await),
            _ => None,
        }
    };
    let queue = async {
        match f.database_url.as_deref().filter(|_| on(ProbeId::Queue)) {
            Some(url) => Some(
                tokio::time::timeout(timeout, prober.queue(url, &f.repo))
                    .await
                    .unwrap_or_else(|_| Err(format!("no answer in {} ms", timeout.as_millis()))),
            ),
            None => None,
        }
    };
    let (jira, server, gateway, app, code_host, queue) =
        tokio::join!(jira, server, gateway, app, code_host, queue);
    Probed {
        jira,
        server,
        gateway,
        code_host,
        app,
        queue,
    }
}

/// `fut`, or [`Probe::TimedOut`] after `timeout`.
async fn bounded(timeout: Duration, fut: impl std::future::Future<Output = Probe>) -> Probe {
    tokio::time::timeout(timeout, fut)
        .await
        .unwrap_or(Probe::TimedOut)
}

/// The host (and port) of a URL, for details: never its path or userinfo.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, h)| h)
        .to_string()
}

/// The usual mapping of a failed probe of `what` at `at`.
fn failed(probe: &Probe, what: &str, at: &str) -> (State, String) {
    match probe {
        Probe::Ok(_) => (State::Connected, format!("{what} answered at {at}")),
        Probe::Rejected(code) => (
            State::Error,
            format!("{what} rejected the credentials (HTTP {code})"),
        ),
        Probe::Http(code) => (State::Error, format!("{what} at {at} answered HTTP {code}")),
        Probe::Unreachable(reason) => (
            State::Disconnected,
            format!("cannot reach {what} at {at}: {reason}"),
        ),
        Probe::TimedOut => (
            State::Disconnected,
            format!(
                "no answer from {what} at {at} within {} s",
                PROBE_TIMEOUT.as_secs()
            ),
        ),
    }
}

struct Row {
    state: State,
    detail: String,
    configure: Option<String>,
}

fn row(state: State, detail: impl Into<String>, configure: Option<&str>) -> Row {
    Row {
        state,
        detail: detail.into(),
        configure: configure.map(str::to_string),
    }
}

const JIRA_HINT: &str = "guard connect jira --base-url https://<site>.atlassian.net --email <you@company.com> --project <KEY>";
const SERVER_HINT: &str =
    "add [server] url = \"https://<guard-serve-host>\" to target.toml, then run guard serve there";
const JEV_HINT: &str =
    "set TYPESAFE_API_KEY where guard serve runs (GUARD_JEV_FAKE=1 for a scripted demo)";
const GATE_HINT: &str = "the gatekeeper runs inside guard serve: add [server] url to target.toml; tune thresholds.toml (mode = \"active\")";
const AGENTS_HINT: &str = "create agents.toml (docs/contracts/agents-toml.md) and set DATABASE_URL=postgres://... for the task queue";
const GATEWAY_HINT: &str = "set [gateway] url in agents.toml, or GUARD_GATEWAY_ADMIN_URL";
const SKILLS_HINT: &str = "guard agents sync";
const CODE_HOST_HINT: &str = "guard connect github | guard connect gitlab";
const CI_HINT: &str = "add a workflow that runs guard gate plan/shard/finalize (see .github/workflows/guard.yml and ci/github-action, or the GitLab component ci/gitlab/templates/gate.yml)";

fn jira(f: &Facts, p: &Probed) -> Row {
    let mut missing = Vec::new();
    if f.jira_base_url.is_none() {
        missing.push("JIRA_BASE_URL");
    }
    if f.jira_email.is_none() {
        missing.push("JIRA_EMAIL");
    }
    if f.jira_token.is_none() {
        missing.push(guard_tracker::config::DEFAULT_TOKEN_VAR);
    }
    let Some(project) = &f.tracker else {
        let detail = if missing.len() == 3 {
            "no [tracker] in target.toml and no Jira credentials".to_string()
        } else {
            "no [tracker] in target.toml".to_string()
        };
        return row(State::NotConfigured, detail, Some(JIRA_HINT));
    };
    if !missing.is_empty() {
        return row(
            State::NotConfigured,
            format!("missing {}", missing.join(", ")),
            Some(JIRA_HINT),
        );
    }
    if let Some(error) = jira_url_error(f) {
        return row(
            State::Error,
            format!("not probed, the token is not sent: {error}"),
            Some(JIRA_HINT),
        );
    }
    let at = host(f.jira_base_url.as_deref().unwrap_or_default());
    match &p.jira {
        None => row(
            State::Configured,
            format!("Jira at {at} (not probed)"),
            None,
        ),
        Some(Probe::Ok(name)) => match project {
            Some(key) => row(
                State::Connected,
                format!("authenticated as {name} at {at}; project {key}"),
                None,
            ),
            None => row(
                State::Configured,
                format!("authenticated as {name} at {at}; [tracker].project is not set"),
                Some(JIRA_HINT),
            ),
        },
        Some(probe) => {
            let (state, detail) = failed(probe, "Jira", &at);
            row(state, detail, Some(JIRA_HINT))
        }
    }
}

/// `guard serve`'s `/healthz`: version and database check.
fn health_detail(body: &str, at: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let version = v["version"].as_str().unwrap_or("?");
    match v["checks"]["database"].as_str() {
        Some(db) => format!("guard serve {version} at {at}; database {db}"),
        None => format!("guard serve {version} at {at}"),
    }
}

fn server(f: &Facts, p: &Probed) -> Row {
    let Some(url) = &f.server_url else {
        return row(
            State::NotConfigured,
            "no [server] url in target.toml",
            Some(SERVER_HINT),
        );
    };
    let at = host(url);
    match &p.server {
        None => row(State::Configured, format!("guard serve at {at}"), None),
        Some(Probe::Ok(body)) => row(State::Connected, health_detail(body, &at), None),
        Some(probe) => {
            let (state, detail) = failed(probe, "guard serve", &at);
            row(state, detail, Some("guard serve (on the server host)"))
        }
    }
}

fn server_up(p: &Probed) -> bool {
    matches!(p.server, Some(Probe::Ok(_)))
}

fn jev(f: &Facts, p: &Probed) -> Row {
    if server_up(p) {
        return row(
            State::Configured,
            format!(
                "triage runs inside guard serve ({}); its key lives on the server",
                guard_jev::MODEL
            ),
            None,
        );
    }
    if f.jev_fake {
        return row(
            State::Configured,
            "GUARD_JEV_FAKE=1: scripted triage, no model is called",
            None,
        );
    }
    if f.jev_key {
        return row(
            State::Configured,
            "TYPESAFE_API_KEY is set; guard serve calls Jev with it",
            None,
        );
    }
    row(
        State::NotConfigured,
        "TYPESAFE_API_KEY is not set here and guard serve is not reachable",
        Some(JEV_HINT),
    )
}

fn gatekeeper(f: &Facts, p: &Probed) -> Row {
    let (mode, source) = match &f.thresholds {
        Err(e) => {
            return row(
                State::Error,
                format!("invalid thresholds: {e}"),
                Some("fix thresholds.toml (docs/contracts/thresholds-toml.md)"),
            );
        }
        Ok(Some((path, mode))) => (*mode, path.as_str()),
        Ok(None) => (Mode::Shadow, "defaults"),
    };
    let mode = match mode {
        Mode::Shadow => "Shadow Mode: every ticket goes to the human lane",
        Mode::Active => "active thresholds",
    };
    let detail = format!("{mode} ({source})");
    match &p.server {
        _ if f.server_url.is_none() => row(
            State::NotConfigured,
            format!("runs inside guard serve, which is not configured; {detail}"),
            Some(GATE_HINT),
        ),
        Some(Probe::Ok(_)) => row(State::Connected, detail, None),
        None => row(State::Configured, detail, None),
        Some(_) => row(
            State::Disconnected,
            format!("guard serve is not reachable; {detail}"),
            Some("guard serve (on the server host)"),
        ),
    }
}

fn agents(f: &Facts, p: &Probed) -> (Row, Vec<AgentTask>) {
    match &f.agents {
        None => {
            return (
                row(
                    State::NotConfigured,
                    "no agents.toml: the agent lane is off",
                    Some(AGENTS_HINT),
                ),
                Vec::new(),
            );
        }
        Some(Err(e)) => {
            return (
                row(State::Error, e.clone(), Some("guard agents check")),
                Vec::new(),
            );
        }
        Some(Ok(_)) => {}
    }
    if !f.database_url.as_deref().is_some_and(postgres) {
        return (
            row(
                State::NotConfigured,
                "agents.toml is valid, but DATABASE_URL (Postgres) is not set: the task queue cannot be read",
                Some(
                    "set DATABASE_URL=postgres://... (the queue guard serve and the workers share)",
                ),
            ),
            Vec::new(),
        );
    }
    match &p.queue {
        None => (
            row(State::Configured, "task queue on Postgres", None),
            Vec::new(),
        ),
        Some(Ok(tasks)) if tasks.is_empty() => (
            row(
                State::Connected,
                "task queue reachable; no agent is running",
                None,
            ),
            Vec::new(),
        ),
        Some(Ok(tasks)) => {
            let shown: Vec<String> = tasks
                .iter()
                .take(3)
                .map(|t| format!("{} {} on {}", t.ticket, t.role, t.branch))
                .collect();
            let more = tasks.len().saturating_sub(3);
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            (
                row(
                    State::Working,
                    format!(
                        "{} agent task(s) running: {}{more}",
                        tasks.len(),
                        shown.join(", ")
                    ),
                    None,
                ),
                tasks.clone(),
            )
        }
        Some(Err(e)) => (
            row(
                State::Disconnected,
                format!("cannot read the task queue: {e}"),
                Some("check DATABASE_URL and that Postgres is up"),
            ),
            Vec::new(),
        ),
    }
}

fn gateway(f: &Facts, p: &Probed) -> Row {
    let Some(url) = gateway_url(f) else {
        return row(
            State::NotConfigured,
            "no model gateway: neither agents.toml [gateway] url nor GUARD_GATEWAY_ADMIN_URL",
            Some(GATEWAY_HINT),
        );
    };
    let at = host(&url);
    match &p.gateway {
        None => row(State::Configured, format!("LiteLLM at {at}"), None),
        Some(Probe::Ok(_)) => row(State::Connected, format!("LiteLLM at {at} is alive"), None),
        Some(probe) => {
            let (state, detail) = failed(probe, "LiteLLM", &at);
            row(state, detail, Some(GATEWAY_HINT))
        }
    }
}

fn skills(f: &Facts) -> Row {
    match (matches!(f.agents, Some(Ok(_))), f.agents_md) {
        (true, true) => row(
            State::Configured,
            "agents.toml and AGENTS.md present (guard agents sync --check verifies them)",
            None,
        ),
        (false, true) => row(State::Configured, "AGENTS.md present", None),
        (true, false) => row(
            State::NotConfigured,
            "agents.toml present, AGENTS.md not rendered yet",
            Some(SKILLS_HINT),
        ),
        (false, false) => row(
            State::NotConfigured,
            "no AGENTS.md for the assistants",
            Some(SKILLS_HINT),
        ),
    }
}

fn local(f: &Facts, p: &Probed) -> Row {
    match &p.app {
        Some(Probe::Ok(_)) => row(
            State::Connected,
            format!("app ready at {}", f.app_url),
            None,
        ),
        Some(Probe::Http(code)) => row(
            State::Error,
            format!("app at {} answered HTTP {code}", f.app_url),
            Some("guard up"),
        ),
        _ => row(
            State::Disconnected,
            format!("app not running at {}", f.app_url),
            Some("guard up"),
        ),
    }
}

fn code_host(f: &Facts, p: &Probed) -> Row {
    let Some(h) = &f.code_host else {
        return row(
            State::NotConfigured,
            "no [code_host] in target.toml: PR comments and status checks are off",
            Some(CODE_HOST_HINT),
        );
    };
    let (name, token_env) = match h.kind {
        CodeHostKind::Github => ("GitHub", "GITHUB_TOKEN"),
        CodeHostKind::Gitlab => ("GitLab", "GITLAB_TOKEN"),
    };
    let at = host(&h.api_url);
    if h.token.is_none() {
        return row(
            State::NotConfigured,
            format!("{token_env} is not set"),
            Some(CODE_HOST_HINT),
        );
    }
    match &p.code_host {
        None => row(State::Configured, format!("{name} at {at}"), None),
        Some(Probe::Ok(login)) => row(State::Connected, format!("{name} at {at} as {login}"), None),
        Some(probe) => {
            let (state, detail) = failed(probe, name, &at);
            row(state, detail, Some(CODE_HOST_HINT))
        }
    }
}

fn ci(f: &Facts) -> Row {
    if f.ci.is_empty() {
        row(
            State::NotConfigured,
            "no CI workflow runs guard gate yet",
            Some(CI_HINT),
        )
    } else {
        row(
            State::Configured,
            format!("guard gate runs in {}", f.ci.join(", ")),
            None,
        )
    }
}

/// The report from the facts and the probes: pure.
pub fn assemble(f: &Facts, p: &Probed) -> Vec<Integration> {
    let item = |id, nodes: &[&'static str], label, r: Row| Integration {
        id,
        nodes: nodes.to_vec(),
        label,
        state: r.state,
        detail: r.detail,
        configure: r.configure,
        tasks: Vec::new(),
    };
    let (lane, tasks) = agents(f, p);
    let mut lane = item("agents", &["qa", "seal", "coder"], "Agent lane", lane);
    lane.tasks = tasks;
    vec![
        item("jira", &["jira"], "Jira", jira(f, p)),
        item("server", &["ingest"], "guard serve", server(f, p)),
        item("jev", &["jev"], "Jev triage", jev(f, p)),
        item("gatekeeper", &["gate"], "Gatekeeper", gatekeeper(f, p)),
        lane,
        item("gateway", &["gateway"], "Model gateway", gateway(f, p)),
        item("skills", &["skills"], "Skills and AGENTS.md", skills(f)),
        item("local", &["local"], "Local app", local(f, p)),
        item(
            "code_host",
            &["direct", "devpr"],
            "Code host",
            code_host(f, p),
        ),
        item("ci", &["guard"], "Remote guard (CI)", ci(f)),
    ]
}

/// The text table: one line per integration, hints underneath.
pub fn render(report: &Report) -> String {
    let id_w = report
        .integrations
        .iter()
        .map(|i| i.id.len())
        .max()
        .unwrap_or(0)
        .max("INTEGRATION".len());
    let state_w = "not configured".len();
    let mut out = format!("{:<id_w$}  {:<state_w$}  DETAIL\n", "INTEGRATION", "STATE");
    for i in &report.integrations {
        out.push_str(&format!(
            "{:<id_w$}  {:<state_w$}  {}\n",
            i.id,
            i.state.text(),
            i.detail
        ));
        if let Some(hint) = &i.configure {
            out.push_str(&format!("{:<id_w$}  {:<state_w$}  hint: {hint}\n", "", ""));
        }
    }
    out
}

/// The real probes.
pub struct Network {
    http: Option<reqwest::Client>,
    timeout: Duration,
}

impl Network {
    pub fn new(timeout: Duration) -> Self {
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("guard-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .ok();
        Self { http, timeout }
    }
}

/// A transport error in a few words, never with its URL.
fn reason(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else if e.is_redirect() {
        "unexpected redirect".into()
    } else {
        "request failed".into()
    }
}

fn status_probe(status: u16, body: String) -> Probe {
    match status {
        200..=299 => Probe::Ok(body),
        401 | 403 => Probe::Rejected(status),
        _ => Probe::Http(status),
    }
}

#[async_trait::async_trait]
impl Prober for Network {
    async fn jira(&self, base_url: &str, email: &str, token: &str) -> Probe {
        use guard_core::ports::TrackerError as E;
        let config = guard_tracker::JiraConfig::for_credentials(base_url, email, token);
        let tracker = match guard_tracker::JiraTracker::new(config) {
            Ok(t) => t,
            Err(_) => return Probe::Unreachable("cannot build the HTTP client".into()),
        };
        match tracker.myself().await {
            Ok(account) => Probe::Ok(account.display_name),
            Err(E::Unauthorized(code)) => Probe::Rejected(code),
            Err(E::NotFound(_)) => Probe::Http(404),
            Err(E::Rejected { status: 0, .. }) | Err(E::Transport(_)) => {
                Probe::Unreachable("connection failed".into())
            }
            Err(E::Rejected { status, .. }) => Probe::Http(status),
            Err(E::RateLimited) => Probe::Http(429),
            Err(E::Unconfigured(_)) => Probe::Unreachable("incomplete credentials".into()),
        }
    }

    async fn get(&self, url: &str) -> Probe {
        let Some(http) = &self.http else {
            return Probe::Unreachable("cannot build the HTTP client".into());
        };
        match http.get(url).send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                status_probe(status, body.chars().take(4000).collect())
            }
            Err(e) if e.is_timeout() => Probe::TimedOut,
            Err(e) => Probe::Unreachable(reason(&e)),
        }
    }

    async fn code_host(&self, kind: CodeHostKind, api_url: &str, token: &str) -> Probe {
        use guard_core::ports::CodeHostError as E;
        let result = match kind {
            CodeHostKind::Github => guard_codehost::whoami::github_user(api_url, token).await,
            CodeHostKind::Gitlab => guard_codehost::whoami::gitlab_user(api_url, token).await,
        };
        match result {
            Ok(login) => Probe::Ok(login),
            Err(E::Unauthorized(code)) => Probe::Rejected(code),
            Err(E::NotFound(_)) => Probe::Http(404),
            Err(E::Rejected { status, .. }) if status >= 300 => Probe::Http(status),
            Err(E::RateLimited) => Probe::Http(429),
            Err(_) => Probe::Unreachable("connection failed".into()),
        }
    }

    async fn queue(&self, database_url: &str, repo: &str) -> Result<Vec<AgentTask>, String> {
        use guard_store::sqlx::{self, Row, postgres::PgConnectOptions, postgres::PgPoolOptions};
        if !postgres(database_url) {
            return Err("DATABASE_URL is not a postgres:// URL".into());
        }
        let options: PgConnectOptions = database_url
            .parse()
            .map_err(|_| "DATABASE_URL is not a valid postgres URL".to_string())?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(self.timeout)
            .connect_with(options)
            .await
            .map_err(|e| db_reason(&e))?;
        let rows = sqlx::query(RUNNING_TASKS_SQL)
            .bind(repo)
            .fetch_all(&pool)
            .await
            .map_err(|e| db_reason(&e));
        pool.close().await;
        Ok(rows?
            .iter()
            .map(|r| {
                let ticket: String = r.get("ticket");
                AgentTask {
                    branch: format!("agent/{ticket}"),
                    role: r.get("role"),
                    attempts: r.get("attempts"),
                    ticket,
                }
            })
            .collect())
    }
}

/// The tasks of a repo that are running now: as the queue itself defines
/// it, a lease that has not expired (an expired one is reclaimable, its
/// worker gone).
pub const RUNNING_TASKS_SQL: &str = "SELECT ticket, role, attempts FROM task_queue \
     WHERE repo = $1 AND state = 'running' AND locked_until > clock_timestamp() \n     ORDER BY id LIMIT 20";

/// A database error in a few words: the server's own message (which never
/// holds the password), never the connection URL.
fn db_reason(e: &guard_store::sqlx::Error) -> String {
    use guard_store::sqlx::Error as E;
    match e {
        E::Database(db) => db.message().to_string(),
        E::PoolTimedOut => "timed out".into(),
        E::Io(_) => "connection failed".into(),
        E::Tls(_) => "TLS handshake failed".into(),
        _ => "database error".into(),
    }
}
