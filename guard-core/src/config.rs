//! `target.toml`: parsing, schema validation and lint warnings.
//!
//! Contract: `docs/contracts/target-toml.md`. Unknown keys and enum values are
//! rejected by serde at parse time; every other schema rule is checked by
//! [`validate`] and reported as a typed [`ValidationError`]. Rules that need the
//! list of tracked files (dead globs, overlapping areas) are warnings from [`lint`].

use crate::globs;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[cfg(test)]
mod tests;

const APP_NAME: &str = r"^[a-z0-9][a-z0-9_-]*$";
const LANGUAGE: &str = r"^[a-z][a-z0-9+#-]*$";
const BASE_URL: &str = r"^https?://[^/\s]+(/[^\s]*)?$";
const HTTP_URL: &str = r"^https?://\S+$";
const URL_PATH: &str = r"^/\S*$";
const ENV_NAME: &str = r"^[A-Z_][A-Z0-9_]*$";
const ROLE: &str = r"^[a-z][a-z0-9_-]*$";
const TEST_ID_ATTRIBUTE: &str = r"^[a-z][a-z0-9-]*$";
const AREA_KEY: &str = r"^[a-z][a-z0-9_]*$";
const BLOCKED_DOMAIN: &str = r"^(\*\.)?[a-z0-9-]+(\.[a-z0-9-]+)+$";
const HOST: &str = r"^[a-z0-9-]+(\.[a-z0-9-]+)+$";
const PROJECT_KEY: &str = r"^[A-Z][A-Z0-9_]*$";
const SERVICE_NAME: &str = r"^[a-z][a-z0-9_-]*$";
/// Repo-relative directory: no leading or trailing slash, no segment starting
/// with a dot (so no `..`), no backslash or drive letter; `.` is the root.
const SERVICE_CWD: &str =
    r"^(\.|[A-Za-z0-9_@+][A-Za-z0-9_.@+-]*(/[A-Za-z0-9_@+][A-Za-z0-9_.@+-]*)*)$";
const READY_URL: &str = r"^https?://[^/\s]+(/\S*)?$";
/// Most `[areas.<key>]` tables a `target.toml` may define.
pub const MAX_AREAS: usize = 255;

/// The parsed `target.toml`, with every default applied.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetConfig {
    pub version: i64,
    pub app: App,
    /// `[[services]]`, in file order. Empty = single-process behavior.
    #[serde(default)]
    pub services: Vec<Service>,
    #[serde(default)]
    pub testability: Testability,
    pub areas: BTreeMap<String, Area>,
    #[serde(default)]
    pub hard_rules: HardRules,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub evidence: Evidence,
    pub tracker: Option<Tracker>,
    pub code_host: Option<CodeHost>,
    pub server: Option<Server>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub name: String,
    #[serde(default)]
    pub languages: Vec<String>,
    pub start: Option<String>,
    pub stop: Option<String>,
    pub base_url: String,
    #[serde(default = "default_ready_path")]
    pub ready_path: String,
    pub preview_url: Option<String>,
}

/// One `[[services]]` entry: an extra process `guard up` starts after `[app].start`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    #[serde(default = "default_service_cwd")]
    pub cwd: String,
    pub start: String,
    pub stop: Option<String>,
    pub ready_url: Option<String>,
}

fn default_service_cwd() -> String {
    ".".into()
}

fn default_ready_path() -> String {
    "/healthz".into()
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Testability {
    pub login_path: String,
    pub seed_path: String,
    pub token_env: String,
    pub roles: Vec<String>,
    pub test_id_attribute: String,
}

impl Default for Testability {
    fn default() -> Self {
        Self {
            login_path: "/__test__/login".into(),
            seed_path: "/__test__/seed".into(),
            token_env: "GUARD_TEST_TOKEN".into(),
            roles: Vec::new(),
            test_id_attribute: "data-testid".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Area {
    pub description: String,
    pub paths: Vec<String>,
    #[serde(default)]
    pub sensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HardRules {
    pub human_only_paths: Vec<String>,
    pub max_fan_out_for_agent: i64,
}

impl Default for HardRules {
    fn default() -> Self {
        Self {
            human_only_paths: Vec::new(),
            max_fan_out_for_agent: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Network {
    pub blocked_domains: Vec<String>,
    pub recorded_hosts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Evidence {
    pub video: Video,
    pub retention_days: i64,
    pub storage_url_env: String,
}

impl Default for Evidence {
    fn default() -> Self {
        Self {
            video: Video::OnFirstRetry,
            retention_days: 14,
            storage_url_env: "GUARD_STORAGE_URL".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Video {
    #[default]
    OnFirstRetry,
    Off,
}

impl Video {
    /// Value exported as `GUARD_VIDEO`.
    pub fn as_env(self) -> &'static str {
        match self {
            Video::OnFirstRetry => "on-first-retry",
            Video::Off => "off",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tracker {
    pub kind: TrackerKind,
    pub project: Option<String>,
    pub ticket_pattern: Option<String>,
    #[serde(default)]
    pub require_ticket: RequireTicket,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    Jira,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RequireTicket {
    #[default]
    Warn,
    Block,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeHost {
    pub kind: CodeHostKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodeHostKind {
    Github,
    Gitlab,
}

impl CodeHostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CodeHostKind::Github => "github",
            CodeHostKind::Gitlab => "gitlab",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub url: String,
}

/// Why `target.toml` could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// Not TOML, wrong types, unknown keys or unknown enum values.
    #[error("{0}")]
    Parse(String),
    /// Parsed, but violates schema rules.
    #[error("{}", join_errors(.0))]
    Invalid(Vec<ValidationError>),
}

pub(crate) fn join_errors(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// One violated rule, located by a dotted field path such as `areas.checkout.paths`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {kind}")]
pub struct ValidationError {
    pub field: String,
    pub kind: ValidationErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationErrorKind {
    UnsupportedVersion(i64),
    PatternMismatch {
        value: String,
        pattern: &'static str,
    },
    Empty,
    Duplicate(String),
    AreaCount(usize),
    BelowMinimum {
        value: i64,
        minimum: i64,
    },
    InvalidRegex(String),
    InvalidGlob {
        glob: String,
        message: String,
    },
}

impl fmt::Display for ValidationErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "unsupported version {v} (only 1 exists)"),
            Self::PatternMismatch { value, pattern } => {
                write!(f, "\"{value}\" does not match {pattern}")
            }
            Self::Empty => write!(f, "must not be empty"),
            Self::Duplicate(value) => write!(f, "duplicate value \"{value}\""),
            Self::AreaCount(n) => {
                write!(f, "must define between 1 and {MAX_AREAS} areas, found {n}")
            }
            Self::BelowMinimum { value, minimum } => {
                write!(f, "{value} is below the minimum {minimum}")
            }
            Self::InvalidRegex(message) => write!(f, "invalid regex: {message}"),
            Self::InvalidGlob { glob, message } => write!(f, "invalid glob \"{glob}\": {message}"),
        }
    }
}

/// Non-fatal findings reported by `guard doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWarning {
    DeadGlob {
        location: String,
        glob: String,
    },
    OverlappingAreas {
        first: String,
        second: String,
        files: usize,
        example: String,
        winner: Option<String>,
    },
    TodoDescription {
        area: String,
    },
}

impl fmt::Display for ConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeadGlob { location, glob } => write!(f, "dead glob in {location}: {glob}"),
            Self::OverlappingAreas {
                first,
                second,
                files,
                example,
                winner,
            } => {
                let noun = if *files == 1 { "file" } else { "files" };
                write!(
                    f,
                    "{files} {noun} matched by areas {first} and {second} (e.g. {example})"
                )?;
                if let Some(winner) = winner {
                    write!(f, "; sensitive area {winner} wins")?;
                }
                Ok(())
            }
            Self::TodoDescription { area } => {
                write!(f, "areas.{area}.description still starts with TODO")
            }
        }
    }
}

impl TargetConfig {
    /// No `[app].start` and no `[[services]]`: guard starts nothing
    /// (target-toml.md "Self mode").
    pub fn is_self_mode(&self) -> bool {
        self.app.start.is_none() && self.services.is_empty()
    }

    /// `base_url + ready_path`, the readiness endpoint.
    pub fn ready_url(&self) -> String {
        format!(
            "{}{}",
            self.app.base_url.trim_end_matches('/'),
            self.app.ready_path
        )
    }

    /// Compiles `[tracker].ticket_pattern`; `Ok(None)` when it is not configured.
    pub fn ticket_regex(&self) -> Result<Option<Regex>, ValidationError> {
        let Some(pattern) = self
            .tracker
            .as_ref()
            .and_then(|t| t.ticket_pattern.as_deref())
        else {
            return Ok(None);
        };
        Regex::new(pattern).map(Some).map_err(|e| ValidationError {
            field: "tracker.ticket_pattern".into(),
            kind: ValidationErrorKind::InvalidRegex(e.to_string()),
        })
    }
}

/// Parses and validates `target.toml`.
pub fn parse_target(text: &str) -> Result<TargetConfig, ConfigError> {
    let config: TargetConfig = toml::from_str(text)
        .map_err(|e| ConfigError::Parse(e.to_string().trim_end().to_string()))?;
    let errors = validate(&config);
    if errors.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Invalid(errors))
    }
}

/// Checks every schema rule serde cannot express. Returns all violations.
pub fn validate(config: &TargetConfig) -> Vec<ValidationError> {
    let mut v = Validator::default();
    if config.version != 1 {
        v.push(
            "version",
            ValidationErrorKind::UnsupportedVersion(config.version),
        );
    }

    let app = &config.app;
    v.pattern("app.name", &app.name, APP_NAME);
    v.each_pattern("app.languages", &app.languages, LANGUAGE);
    v.unique("app.languages", &app.languages);
    v.optional_non_empty("app.start", app.start.as_deref());
    v.optional_non_empty("app.stop", app.stop.as_deref());
    v.pattern("app.base_url", &app.base_url, BASE_URL);
    v.pattern("app.ready_path", &app.ready_path, URL_PATH);
    if let Some(url) = &app.preview_url {
        v.pattern("app.preview_url", url, HTTP_URL);
    }

    for (i, service) in config.services.iter().enumerate() {
        let field = |key: &str| format!("services[{i}].{key}");
        v.pattern(&field("name"), &service.name, SERVICE_NAME);
        v.pattern(&field("cwd"), &service.cwd, SERVICE_CWD);
        if service.start.is_empty() {
            v.push(&field("start"), ValidationErrorKind::Empty);
        }
        v.optional_non_empty(&field("stop"), service.stop.as_deref());
        if let Some(url) = &service.ready_url {
            v.pattern(&field("ready_url"), url, READY_URL);
        }
    }
    let names: Vec<String> = config.services.iter().map(|s| s.name.clone()).collect();
    v.unique("services.name", &names);

    let t = &config.testability;
    v.pattern("testability.login_path", &t.login_path, URL_PATH);
    v.pattern("testability.seed_path", &t.seed_path, URL_PATH);
    v.pattern("testability.token_env", &t.token_env, ENV_NAME);
    v.each_pattern("testability.roles", &t.roles, ROLE);
    v.unique("testability.roles", &t.roles);
    v.pattern(
        "testability.test_id_attribute",
        &t.test_id_attribute,
        TEST_ID_ATTRIBUTE,
    );

    let count = config.areas.len();
    if count == 0 || count > MAX_AREAS {
        v.push("areas", ValidationErrorKind::AreaCount(count));
    }
    for (key, area) in &config.areas {
        let field = format!("areas.{key}");
        v.pattern(&field, key, AREA_KEY);
        if area.description.is_empty() {
            v.push(&format!("{field}.description"), ValidationErrorKind::Empty);
        }
        let paths_field = format!("{field}.paths");
        if area.paths.is_empty() {
            v.push(&paths_field, ValidationErrorKind::Empty);
        }
        v.globs(&paths_field, &area.paths);
    }

    v.globs(
        "hard_rules.human_only_paths",
        &config.hard_rules.human_only_paths,
    );
    v.minimum(
        "hard_rules.max_fan_out_for_agent",
        config.hard_rules.max_fan_out_for_agent,
        1,
    );

    let n = &config.network;
    v.each_pattern(
        "network.blocked_domains",
        &n.blocked_domains,
        BLOCKED_DOMAIN,
    );
    v.unique("network.blocked_domains", &n.blocked_domains);
    v.each_pattern("network.recorded_hosts", &n.recorded_hosts, HOST);
    v.unique("network.recorded_hosts", &n.recorded_hosts);

    let e = &config.evidence;
    v.minimum("evidence.retention_days", e.retention_days, 1);
    v.pattern("evidence.storage_url_env", &e.storage_url_env, ENV_NAME);

    if let Some(tracker) = &config.tracker {
        if let Some(project) = &tracker.project {
            v.pattern("tracker.project", project, PROJECT_KEY);
        }
        v.optional_non_empty("tracker.ticket_pattern", tracker.ticket_pattern.as_deref());
    }
    if let Some(server) = &config.server {
        v.pattern("server.url", &server.url, HTTP_URL);
    }
    v.errors
}

/// Warnings that need the tracked files: dead globs, files claimed by two
/// areas, and descriptions still left as `TODO`.
pub fn lint(config: &TargetConfig, tracked: &[String]) -> Vec<ConfigWarning> {
    let mut warnings = Vec::new();
    for (key, area) in &config.areas {
        for glob in &area.paths {
            if !globs::matches_any(glob, tracked) {
                warnings.push(ConfigWarning::DeadGlob {
                    location: format!("areas.{key}.paths"),
                    glob: glob.clone(),
                });
            }
        }
    }

    let matchers: Vec<(&String, &Area, Vec<globset::GlobMatcher>)> = config
        .areas
        .iter()
        .map(|(key, area)| {
            let compiled = area
                .paths
                .iter()
                .filter_map(|g| globs::compile(g).ok())
                .collect();
            (key, area, compiled)
        })
        .collect();
    let claims =
        |matchers: &[globset::GlobMatcher], file: &str| matchers.iter().any(|m| m.is_match(file));
    for (i, (first, first_area, first_globs)) in matchers.iter().enumerate() {
        for (second, second_area, second_globs) in &matchers[i + 1..] {
            let shared: Vec<&String> = tracked
                .iter()
                // A subtree entry (`frontend/`) is not a file two areas can claim.
                .filter(|f| !f.ends_with('/'))
                .filter(|f| claims(first_globs, f) && claims(second_globs, f))
                .collect();
            if let Some(example) = shared.first() {
                let winner = match (first_area.sensitive, second_area.sensitive) {
                    (true, false) => Some((*first).clone()),
                    (false, true) => Some((*second).clone()),
                    _ => None,
                };
                warnings.push(ConfigWarning::OverlappingAreas {
                    first: (*first).clone(),
                    second: (*second).clone(),
                    files: shared.len(),
                    example: (*example).clone(),
                    winner,
                });
            }
        }
    }

    for (key, area) in &config.areas {
        if area.description.starts_with("TODO") {
            warnings.push(ConfigWarning::TodoDescription { area: key.clone() });
        }
    }
    warnings
}

/// Accumulates validation errors in field order.
#[derive(Default)]
pub(crate) struct Validator {
    pub(crate) errors: Vec<ValidationError>,
}

impl Validator {
    pub(crate) fn push(&mut self, field: &str, kind: ValidationErrorKind) {
        self.errors.push(ValidationError {
            field: field.to_string(),
            kind,
        });
    }

    pub(crate) fn pattern(&mut self, field: &str, value: &str, pattern: &'static str) {
        let re = Regex::new(pattern).expect("built-in pattern compiles");
        if !re.is_match(value) {
            self.push(
                field,
                ValidationErrorKind::PatternMismatch {
                    value: value.to_string(),
                    pattern,
                },
            );
        }
    }

    pub(crate) fn each_pattern(&mut self, field: &str, values: &[String], pattern: &'static str) {
        for value in values {
            self.pattern(field, value, pattern);
        }
    }

    pub(crate) fn unique(&mut self, field: &str, values: &[String]) {
        let mut seen = BTreeSet::new();
        for value in values {
            if !seen.insert(value) {
                self.push(field, ValidationErrorKind::Duplicate(value.clone()));
            }
        }
    }

    pub(crate) fn optional_non_empty(&mut self, field: &str, value: Option<&str>) {
        if value == Some("") {
            self.push(field, ValidationErrorKind::Empty);
        }
    }

    pub(crate) fn minimum(&mut self, field: &str, value: i64, minimum: i64) {
        if value < minimum {
            self.push(field, ValidationErrorKind::BelowMinimum { value, minimum });
        }
    }

    /// Unique, non-empty, compilable globs.
    pub(crate) fn globs(&mut self, field: &str, values: &[String]) {
        self.unique(field, values);
        for glob in values {
            if glob.is_empty() {
                self.push(field, ValidationErrorKind::Empty);
            } else if let Err(e) = globs::compile(glob) {
                self.push(
                    field,
                    ValidationErrorKind::InvalidGlob {
                        glob: glob.clone(),
                        message: e.kind().to_string(),
                    },
                );
            }
        }
    }
}
