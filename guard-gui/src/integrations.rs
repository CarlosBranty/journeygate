//! The live state of the flow's integrations, from
//! `guard status --integrations --json`, and the settings the Flow view's
//! side panel can write.
//!
//! guard is the source of truth: the GUI only parses its report, maps nodes
//! to it and decides when to ask again ([`poll_due`]). What the panel saves
//! goes where guard already reads it: the Jira project key and the server
//! URL into `target.toml` (the same layout-preserving edit `guard connect`
//! uses, validated before writing), the Jira site and email and the gateway
//! URL into eframe persistence, handed to the guard child as environment
//! variables ([`child_env`]). The Jira token is never a field here: it lives
//! in the Windows Credential Manager, one per Jira host
//! ([`crate::credentials::jira_target`]).

use crate::ai_settings::endpoint_host;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// While the Flow view is visible the report is refreshed this often.
pub const POLL_EVERY: Duration = Duration::from_secs(30);

pub const JIRA_BASE_URL_ENV: &str = "JIRA_BASE_URL";
pub const JIRA_EMAIL_ENV: &str = "JIRA_EMAIL";
pub const JIRA_TOKEN_ENV: &str = "JIRA_API_TOKEN";
pub const GATEWAY_URL_ENV: &str = "GUARD_GATEWAY_ADMIN_URL";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Connected,
    Configured,
    NotConfigured,
    Disconnected,
    Error,
    Working,
    /// A state a newer guard reports.
    #[serde(other)]
    Unknown,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Conectado",
            Self::Configured | Self::Unknown => "Configurado",
            Self::NotConfigured => "Sin configurar",
            Self::Disconnected => "Desconectado",
            Self::Error => "Error",
            Self::Working => "Trabajando",
        }
    }

    /// Only a working lane keeps the diagram moving.
    pub fn animates(self) -> bool {
        self == Self::Working
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentTask {
    pub ticket: String,
    pub role: String,
    pub branch: String,
    #[serde(default)]
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Integration {
    pub id: String,
    #[serde(default)]
    pub nodes: Vec<String>,
    #[serde(default)]
    pub label: String,
    pub state: State,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub configure: Option<String>,
    #[serde(default)]
    pub tasks: Vec<AgentTask>,
    /// Jira probed with the site, email and token of this GUI's side panel
    /// ([`mark_gui_jira_probe`]): guard's own commands do not read them.
    #[serde(skip)]
    pub gui_probe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Report {
    pub integrations: Vec<Integration>,
}

pub fn parse(text: &str) -> Result<Report, String> {
    serde_json::from_str(text).map_err(|e| format!("bad guard status --integrations output: {e}"))
}

impl Report {
    pub fn get(&self, id: &str) -> Option<&Integration> {
        self.integrations.iter().find(|i| i.id == id)
    }

    /// The integration a Flow node shows.
    pub fn for_node(&self, node: &str) -> Option<&Integration> {
        self.integrations
            .iter()
            .find(|i| i.nodes.iter().any(|n| n == node))
    }

    /// A Flow node's badge. The agent lane's nodes are working only for the
    /// role that runs there ([`Self::working_lane_nodes`]); the others show
    /// the queue as connected.
    pub fn node_state(&self, node: &str) -> Option<State> {
        let i = self.for_node(node)?;
        if i.id == "agents" && i.state == State::Working {
            return Some(if self.working_lane_nodes().contains(node) {
                State::Working
            } else {
                State::Connected
            });
        }
        Some(i.state)
    }

    /// The agent lane's nodes with a task running: `qa` works on the QA
    /// agent and its red-first seal, `coder` on the coder. A role this GUI
    /// does not know lights the whole lane.
    pub fn working_lane_nodes(&self) -> BTreeSet<&'static str> {
        let Some(lane) = self.get("agents").filter(|i| i.state == State::Working) else {
            return BTreeSet::new();
        };
        let mut nodes = BTreeSet::new();
        let mut unknown = lane.tasks.is_empty();
        for task in &lane.tasks {
            match task.role.as_str() {
                "qa" => nodes.extend(["qa", "seal"]),
                "coder" => {
                    nodes.insert("coder");
                }
                _ => unknown = true,
            }
        }
        if unknown {
            nodes.extend(LANE_NODES);
        }
        nodes
    }

    /// Running tasks of the agent lane.
    pub fn running_agents(&self) -> usize {
        self.get("agents")
            .filter(|i| i.state == State::Working)
            .map_or(0, |i| i.tasks.len().max(1))
    }
}

/// The agent lane's nodes.
pub const LANE_NODES: [&str; 3] = ["qa", "seal", "coder"];

/// The probe ran with the side panel's Jira site, email and token, which
/// only this GUI holds: `guard serve`, `finalize` and the agents read Jira
/// from their own environment or `guard connect`'s credentials file. So a
/// successful probe is shown as configured, not connected.
pub fn mark_gui_jira_probe(report: &mut Report) {
    for i in report.integrations.iter_mut().filter(|i| i.id == "jira") {
        i.gui_probe = true;
        if i.state == State::Connected {
            i.state = State::Configured;
        }
    }
}

/// The Spanish name of an integration; guard's own for one it adds later.
pub fn spanish_label(i: &Integration) -> &str {
    match i.id.as_str() {
        "jira" => "Jira",
        "server" => "guard serve (servidor)",
        "jev" => "Triaje con Jev",
        "gatekeeper" => "Gatekeeper",
        "agents" => "Carril de agentes",
        "gateway" => "Gateway de modelos (LiteLLM)",
        "skills" => "Skills y AGENTS.md",
        "local" => "App local",
        "code_host" => "Host de código (GitHub / GitLab)",
        "ci" => "Guard remoto (CI)",
        _ => &i.label,
    }
}

/// One Spanish sentence on the state; guard's English detail stays below it.
pub fn summary(i: &Integration) -> String {
    let name = spanish_label(i);
    if i.gui_probe {
        return match i.state {
            State::Configured => format!(
                "{name}: probado solo desde esta app. guard serve, finalize y los agentes no leen \
                 estas credenciales; para que las usen, ejecuta guard connect jira."
            ),
            _ => format!(
                "{name}: falló la prueba con las credenciales de esta app (guard serve, finalize \
                 y los agentes no las leen)."
            ),
        };
    }
    match i.state {
        State::Connected => format!("{name}: conectado y respondiendo."),
        State::Configured => format!("{name}: configurado (sin prueba de conexión)."),
        State::NotConfigured => format!("{name}: todavía sin configurar."),
        State::Disconnected => format!("{name}: configurado, pero no responde."),
        State::Error => format!("{name}: rechazó la conexión o su configuración no es válida."),
        State::Working => {
            let n = i.tasks.len().max(1);
            format!("{name}: {n} tarea(s) de agente en curso.")
        }
        State::Unknown => format!("{name}: estado nuevo que esta versión de la app no conoce."),
    }
}

/// `configure` when it is one guard command to paste in a terminal, not
/// prose or a choice of commands.
pub fn copy_command(configure: &str) -> Option<&str> {
    let c = configure.trim();
    (c.starts_with("guard ") && !c.contains(['(', '|', ';'])).then_some(c)
}

/// Ask on project open (no report yet), then every [`POLL_EVERY`] while
/// the Flow view is visible.
pub fn poll_due(last: Option<Instant>, now: Instant, visible: bool) -> bool {
    match last {
        None => true,
        Some(t) => visible && now.saturating_duration_since(t) >= POLL_EVERY,
    }
}

/// How long until the next poll is due.
pub fn next_poll_in(last: Option<Instant>, now: Instant) -> Duration {
    last.map_or(Duration::ZERO, |t| {
        POLL_EVERY.saturating_sub(now.saturating_duration_since(t))
    })
}

/// The settings form of a node's panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// Site, email, token (Credential Manager) and project key.
    Jira,
    /// `[server] url` of target.toml (guard serve, its triage and gatekeeper).
    Server,
    /// The LiteLLM admin URL.
    Gateway,
    /// `[testability]` roles, login path and token variable of target.toml
    /// ([`crate::accounts`]).
    Accounts,
}

pub fn form(node: &str) -> Option<Form> {
    match node {
        "jira" => Some(Form::Jira),
        "ingest" | "gate" => Some(Form::Server),
        "gateway" => Some(Form::Gateway),
        "local" | "guard" => Some(Form::Accounts),
        _ => None,
    }
}

/// The persisted, non-secret settings of the side panel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationSettings {
    #[serde(default)]
    pub jira_base_url: String,
    #[serde(default)]
    pub jira_email: String,
    #[serde(default)]
    pub gateway_url: String,
}

fn url_value(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

impl IntegrationSettings {
    /// The Jira host a token is saved for; `None` without a site.
    pub fn jira_host(&self) -> Option<String> {
        let url = self.jira_base_url.trim();
        (!url.is_empty()).then(|| endpoint_host(url))
    }

    /// The panel's site and email, copied in only once both pass the
    /// checks; a blank site clears them (guard's own configuration applies).
    pub fn apply_jira(&mut self, base_url: &str, email: &str) -> Result<(), String> {
        let base = base_url.trim();
        if base.is_empty() {
            self.jira_base_url.clear();
            self.jira_email.clear();
            return Ok(());
        }
        let base = check_url(base)?;
        check_email(email)?;
        self.jira_base_url = base;
        self.jira_email = email.trim().to_string();
        Ok(())
    }

    /// The panel's gateway URL, copied in only once it passes the check.
    pub fn apply_gateway(&mut self, url: &str) -> Result<(), String> {
        let url = url.trim();
        self.gateway_url = if url.is_empty() {
            String::new()
        } else {
            check_url(url)?
        };
        Ok(())
    }
}

/// Whether the saved Jira token may go to the settings' site: it passes
/// [`check_url`] and is https. The token is saved per host, so without
/// this, `http://` on the same host would send it in cleartext.
pub fn jira_token_allowed(s: &IntegrationSettings) -> bool {
    check_url(&s.jira_base_url).is_ok_and(|u| u.starts_with("https://"))
}

/// The variables a guard child gets on top of the inherited environment.
/// Blank settings are left out, so guard's own configuration (environment,
/// `guard connect`'s credentials file) applies, and so are settings that
/// fail [`check_url`] (persisted by an older build). The token is the one
/// saved for this site, and only for an https site
/// ([`jira_token_allowed`]); otherwise the inherited token is blanked
/// (guard treats blank as unset), so a token meant for another site never
/// reaches this one.
pub fn child_env(s: &IntegrationSettings, jira_token: Option<&str>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut push = |name: &str, value: String| env.push((name.to_string(), value));
    if let Some(base) = jira_override(s) {
        let allowed = jira_token_allowed(s);
        push(JIRA_BASE_URL_ENV, base);
        let email = s.jira_email.trim();
        if !email.is_empty() {
            push(JIRA_EMAIL_ENV, email.to_string());
        }
        let token = jira_token
            .map(str::trim)
            .filter(|t| allowed && !t.is_empty());
        push(JIRA_TOKEN_ENV, token.unwrap_or_default().to_string());
    }
    if let Ok(gateway) = check_url(&s.gateway_url) {
        push(GATEWAY_URL_ENV, gateway);
    }
    env
}

/// The Jira site [`child_env`] hands the child, if any: set, and safe for
/// a token (https, or http to this machine).
pub fn jira_override(s: &IntegrationSettings) -> Option<String> {
    check_url(&s.jira_base_url).ok()
}

/// `localhost`, `127.x.x.x` or `[::1]`, with or without a port.
fn is_loopback(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The URL without its trailing slash, as `guard connect` accepts it:
/// https, or plain http only for this machine; no credentials inside.
pub fn check_url(url: &str) -> Result<String, String> {
    let url = url_value(url);
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| "la URL debe empezar con https://".to_string())?;
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || url.chars().any(char::is_whitespace) {
        return Err("la URL no es válida".into());
    }
    if authority.contains('@') {
        return Err("la URL no puede llevar credenciales".into());
    }
    if url.starts_with("http://") && !is_loopback(authority) {
        return Err("usa https (http solo se acepta para localhost)".into());
    }
    Ok(url)
}

pub fn check_project_key(key: &str) -> Result<(), String> {
    let valid = key.starts_with(|c: char| c.is_ascii_uppercase())
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(())
    } else {
        Err("la clave del proyecto va en mayúsculas, p. ej. SHOP".into())
    }
}

pub fn check_email(email: &str) -> Result<(), String> {
    let email = email.trim();
    if email.contains('@') && !email.chars().any(char::is_whitespace) {
        Ok(())
    } else {
        Err("el email no es válido".into())
    }
}

fn edited(text: &str, table: &str, entries: &[(&str, &str)]) -> Result<String, String> {
    let new = guard_core::config_edit::set_table_keys(text, table, entries)?;
    guard_core::config::parse_target(&new)
        .map_err(|e| format!("target.toml no sería válido: {e}"))?;
    Ok(new)
}

/// `target.toml` with `[tracker] kind = "jira"`, `project = <key>`.
pub fn set_tracker_project(text: &str, key: &str) -> Result<String, String> {
    check_project_key(key)?;
    edited(text, "tracker", &[("kind", "jira"), ("project", key)])
}

/// `target.toml` with `[server] url = <url>`.
pub fn set_server_url(text: &str, url: &str) -> Result<String, String> {
    let url = check_url(url)?;
    edited(text, "server", &[("url", &url)])
}
