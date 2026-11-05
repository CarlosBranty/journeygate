//! Deterministic repository detection used by `guard init` (no network, no I/O).
//!
//! The caller lists tracked files, branches and the origin URL; these functions
//! turn them into an [`InitPlan`] and render a `target.toml` that always validates.

use crate::config::{CodeHostKind, MAX_AREAS, Service};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

mod app;
pub mod compose;

pub use app::*;

#[cfg(test)]
mod tests;

/// Body of a freshly created `catalog/journeys.json`.
pub const EMPTY_CATALOG: &str = "{\n  \"version\": 1,\n  \"journeys\": []\n}\n";

const IGNORED_DIRS: [&str; 4] = ["node_modules", "target", "vendor", "dist"];
const NON_AREA_DIRS: [&str; 3] = ["e2e-playwright", ".github", "docs"];
const COMPOSE_FILES: [&str; 4] = [
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartStop {
    pub start: Option<String>,
    pub stop: Option<String>,
}

/// An area proposed for a top-level directory. An empty `dir` means the whole repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedArea {
    pub key: String,
    pub dir: String,
}

impl ProposedArea {
    fn glob(&self) -> String {
        if self.dir.is_empty() {
            "**".to_string()
        } else {
            format!("{}/**", escape_glob(&self.dir))
        }
    }

    fn label(&self) -> &str {
        if self.dir.is_empty() {
            &self.key
        } else {
            &self.dir
        }
    }
}

/// Everything `guard init` writes into `target.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPlan {
    pub name: String,
    pub languages: Vec<String>,
    pub start: Option<String>,
    pub stop: Option<String>,
    pub code_host: Option<CodeHostKind>,
    pub ticket_project: Option<String>,
    pub base_url: String,
    pub ready_path: String,
    /// `[[services]]`, in start order. Empty = the single-`[app]` shape.
    pub services: Vec<Service>,
    pub areas: Vec<ProposedArea>,
}

/// The process part of `target.toml` that a [`DetectionReport`] proposes
/// (detection.md "base_url, ready_path and the target shape").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPlan {
    pub start: Option<String>,
    pub stop: Option<String>,
    pub base_url: String,
    pub ready_path: String,
    pub services: Vec<Service>,
}

/// Turns a report into `[app]` start/stop, `base_url`, `ready_path` and
/// `[[services]]`. The `single` shape puts its only service's start (or the
/// whole compose stack) in `[app]`; `services` puts the compose infrastructure
/// in `[app]` and every service in `[[services]]`.
pub fn app_plan(report: &DetectionReport) -> AppPlan {
    let infra = report.compose.as_ref();
    let infra_start = infra
        .and_then(|c| c.start.as_ref())
        .map(|g| g.value.clone());
    let infra_stop = infra.and_then(|c| c.stop.as_ref()).map(|g| g.value.clone());
    let (start, stop, services) = match (report.target_shape, report.services.as_slice()) {
        (TargetShape::Single, [only]) => (Some(only.start.value.clone()), None, Vec::new()),
        (TargetShape::Single, _) => (infra_start, infra_stop, Vec::new()),
        (TargetShape::Services, detected) => (
            infra_start,
            infra_stop,
            detected
                .iter()
                .map(|s| Service {
                    name: s.name.clone(),
                    cwd: s.root.clone(),
                    start: s.start.value.clone(),
                    stop: None,
                    ready_url: Some(s.ready_url.clone()),
                })
                .collect(),
        ),
    };
    AppPlan {
        start,
        stop,
        base_url: report.base_url.value.clone(),
        ready_path: report.ready_path.value.clone(),
        services,
    }
}

fn is_ignored(path: &str) -> bool {
    let mut segments: Vec<&str> = path.split('/').collect();
    segments.pop();
    segments.iter().any(|s| IGNORED_DIRS.contains(s))
}

/// Language of one file from its manifest name or extension.
fn language_of(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let by_manifest = match name {
        "package.json" | "tsconfig.json" => Some("typescript"),
        "go.mod" => Some("go"),
        "Cargo.toml" => Some("rust"),
        "pyproject.toml" | "requirements.txt" => Some("python"),
        "pom.xml" | "build.gradle" => Some("java"),
        "Gemfile" => Some("ruby"),
        "composer.json" => Some("php"),
        _ => None,
    };
    if by_manifest.is_some() {
        return by_manifest;
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    match ext {
        "ts" | "tsx" => Some("typescript"),
        "js" | "mjs" | "cjs" | "jsx" => Some("javascript"),
        "go" => Some("go"),
        "rs" => Some("rust"),
        "py" => Some("python"),
        "java" => Some("java"),
        "kt" => Some("kotlin"),
        "rb" => Some("ruby"),
        "cs" | "csproj" => Some("csharp"),
        "php" => Some("php"),
        _ => None,
    }
}

fn is_source(path: &str) -> bool {
    !is_ignored(path) && language_of(path).is_some()
}

/// Sorted, unique language names.
pub fn detect_languages(files: &[String]) -> Vec<String> {
    files
        .iter()
        .filter(|f| !is_ignored(f))
        .filter_map(|f| language_of(f))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// `github.com` -> GitHub, a host containing `gitlab` -> GitLab.
pub fn detect_code_host(remote_url: &str) -> Option<CodeHostKind> {
    let rest = match remote_url.split_once("://") {
        Some((_, rest)) => rest,
        None => remote_url,
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port
        .split(':')
        .next()
        .unwrap_or(host_port)
        .to_ascii_lowercase();
    if host == "github.com" {
        Some(CodeHostKind::Github)
    } else if host.contains("gitlab") {
        Some(CodeHostKind::Gitlab)
    } else {
        None
    }
}

/// Compose files at the root win; otherwise `npm start` when `scripts.start` exists.
pub fn detect_start_stop(root_entries: &[String], package_json: Option<&str>) -> StartStop {
    if root_entries
        .iter()
        .any(|e| COMPOSE_FILES.contains(&e.as_str()))
    {
        return StartStop {
            start: Some("docker compose up -d --wait".into()),
            stop: Some("docker compose down -v".into()),
        };
    }
    let has_start = package_json
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .is_some_and(|pkg| pkg["scripts"]["start"].is_string());
    StartStop {
        start: has_start.then(|| "npm start".into()),
        stop: None,
    }
}

/// The project of the first `[A-Z][A-Z0-9]+-\d+` prefix in a branch name
/// (the full name, or its last `/` segment for `origin/...` or `feature/...`).
pub fn detect_ticket_project(branches: &[String]) -> Option<String> {
    let re = Regex::new(r"^([A-Z][A-Z0-9]+)-\d+").expect("built-in pattern compiles");
    branches.iter().find_map(|branch| {
        let last = branch.rsplit('/').next().unwrap_or(branch);
        [branch.as_str(), last]
            .iter()
            .find_map(|name| re.captures(name).map(|c| c[1].to_string()))
    })
}

/// One area per top-level directory with source files, sorted by key.
pub fn propose_areas(files: &[String]) -> Vec<ProposedArea> {
    propose_areas_with_warnings(files).0
}

/// [`propose_areas`] plus a warning for every adjustment: a directory whose key
/// collides with an earlier one (`Web`/`web`, `my-app`/`my_app`) gets the first
/// free `<key>_<n>` (n >= 2), and more than [`MAX_AREAS`] directories fall back
/// to a single `[areas.app]` so the rendered `target.toml` still validates.
pub fn propose_areas_with_warnings(files: &[String]) -> (Vec<ProposedArea>, Vec<String>) {
    let dirs: BTreeSet<&str> = files
        .iter()
        .filter(|f| is_source(f))
        .filter_map(|f| f.split_once('/').map(|(dir, _)| dir))
        .filter(|dir| !dir.starts_with('.') && !NON_AREA_DIRS.contains(dir))
        .collect();
    let mut taken: BTreeSet<String> = dirs.iter().map(|d| area_key(d)).collect();
    let mut owners: BTreeMap<String, &str> = BTreeMap::new();
    let mut by_key: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings = Vec::new();
    for dir in &dirs {
        let natural = area_key(dir);
        let Some(owner) = owners.get(&natural) else {
            owners.insert(natural.clone(), dir);
            by_key.insert(natural, dir.to_string());
            continue;
        };
        let key = (2..)
            .map(|n| format!("{natural}_{n}"))
            .find(|k| !taken.contains(k))
            .expect("an unbounded range always has a free suffix");
        warnings.push(format!(
            "directories {owner:?} and {dir:?} both map to area key `{natural}`; proposed `{key}` for {dir:?}"
        ));
        taken.insert(key.clone());
        by_key.insert(key, dir.to_string());
    }
    let app = || {
        vec![ProposedArea {
            key: "app".into(),
            dir: String::new(),
        }]
    };
    if by_key.is_empty() {
        return (app(), warnings);
    }
    if by_key.len() > MAX_AREAS {
        warnings.push(format!(
            "{} top-level directories exceed the limit of {MAX_AREAS} areas; proposed a single [areas.app]",
            by_key.len()
        ));
        return (app(), warnings);
    }
    let areas = by_key
        .into_iter()
        .map(|(key, dir)| ProposedArea { key, dir })
        .collect();
    (areas, warnings)
}

fn area_key(dir: &str) -> String {
    let key: String = dir
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if key.starts_with(|c: char| c.is_ascii_lowercase()) {
        key
    } else {
        format!("area_{key}")
    }
}

/// Lower-cases and maps anything outside `[a-z0-9_-]` to `-`, then strips a
/// leading non-alphanumeric run so the result matches `^[a-z0-9][a-z0-9_-]*$`.
pub fn normalize_app_name(dir_name: &str) -> String {
    let mapped: String = dir_name
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = mapped.trim_start_matches(['-', '_']);
    if trimmed.is_empty() {
        "app".into()
    } else {
        trimmed.into()
    }
}

/// Renders `target.toml`. Values come from detection, fixed values from the contract.
pub fn render_target_toml(plan: &InitPlan) -> String {
    render(plan, false)
}

/// Renders the self-mode `target.toml` (target-toml.md "Self mode"): no
/// `[app].start`/`stop` and no `[[services]]`; the detected commands follow
/// the last `[app]` key as a commented block, so uncommenting it gives the
/// [`render_target_toml`] shape. Without detected commands there is no block.
pub fn render_self_target_toml(plan: &InitPlan) -> String {
    render(plan, true)
}

fn push_start_stop(out: &mut String, plan: &InitPlan) {
    if let Some(start) = &plan.start {
        let _ = writeln!(out, "start = {}", toml_string(start));
    }
    if let Some(stop) = &plan.stop {
        let _ = writeln!(out, "stop = {}", toml_string(stop));
    }
}

fn push_services(out: &mut String, services: &[crate::config::Service]) {
    for service in services {
        out.push_str("\n[[services]]\n");
        let _ = writeln!(out, "name = {}", toml_string(&service.name));
        let _ = writeln!(out, "cwd = {}", toml_string(&service.cwd));
        let _ = writeln!(out, "start = {}", toml_string(&service.start));
        if let Some(stop) = &service.stop {
            let _ = writeln!(out, "stop = {}", toml_string(stop));
        }
        if let Some(url) = &service.ready_url {
            let _ = writeln!(out, "ready_url = {}", toml_string(url));
        }
    }
}

/// The detected commands as comment lines, or `""` when there are none.
fn commented_block(plan: &InitPlan) -> String {
    let mut body = String::new();
    push_start_stop(&mut body, plan);
    push_services(&mut body, &plan.services);
    let body = body.strip_prefix('\n').unwrap_or(&body);
    if body.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n{}\n{}\n",
        crate::self_mode::SELF_MODE_HEADER,
        crate::self_mode::SELF_MODE_MARKER
    );
    for line in body.lines() {
        if line.is_empty() {
            out.push_str("#\n");
        } else {
            let _ = writeln!(out, "# {line}");
        }
    }
    out
}

fn render(plan: &InitPlan, self_mode: bool) -> String {
    let mut out = String::new();
    out.push_str("# target.toml, generated by guard init.\n");
    out.push_str("# Contract: docs/contracts/target-toml.md of the guard repository.\n");
    out.push_str("version = 1\n\n[app]\n");
    let _ = writeln!(out, "name = {}", toml_string(&plan.name));
    let languages: Vec<String> = plan.languages.iter().map(|l| toml_string(l)).collect();
    let _ = writeln!(out, "languages = [{}]", languages.join(", "));
    if !self_mode {
        push_start_stop(&mut out, plan);
    }
    let _ = writeln!(out, "base_url = {}", toml_string(&plan.base_url));
    let _ = writeln!(out, "ready_path = {}", toml_string(&plan.ready_path));
    if self_mode {
        out.push_str(&commented_block(plan));
    } else {
        push_services(&mut out, &plan.services);
    }

    for area in &plan.areas {
        let _ = write!(
            out,
            "\n[areas.{}]\ndescription = {}\npaths = [{}]\n",
            area.key,
            toml_string(&format!("TODO: describe {}", area.label())),
            toml_string(&area.glob())
        );
    }
    if let Some(project) = &plan.ticket_project {
        let _ = write!(
            out,
            "\n[tracker]\nkind = \"jira\"\nproject = {}\nticket_pattern = '{project}-\\d+'\n",
            toml_string(project)
        );
    }
    if let Some(host) = plan.code_host {
        let _ = write!(out, "\n[code_host]\nkind = \"{}\"\n", host.as_str());
    }
    out
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Escapes glob metacharacters in a literal directory name.
fn escape_glob(literal: &str) -> String {
    literal
        .chars()
        .map(|c| match c {
            '*' | '?' | '[' | ']' | '{' | '}' => format!("[{c}]"),
            c => c.to_string(),
        })
        .collect()
}
