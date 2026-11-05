//! The JSON outputs of `guard status --json` and `guard affected --json` (cli.md).

use serde::Deserialize;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusReport {
    pub base_url: String,
    pub ready: bool,
    pub entries: Vec<StatusEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusEntry {
    /// `app` or `service`.
    pub kind: String,
    pub name: String,
    pub cwd: String,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub ready_url: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub alive: Option<bool>,
    #[serde(default)]
    pub ready: Option<bool>,
    pub log: String,
}

/// The parts of `guard affected --json` the GUI shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affected {
    pub base: String,
    pub changed_files: usize,
    pub tags: Vec<String>,
    /// Journeys that will run, sorted by id.
    pub ids: Vec<String>,
}

pub fn parse_status(text: &str) -> Result<StatusReport, String> {
    serde_json::from_str(text).map_err(|e| format!("unexpected guard status output: {e}"))
}

pub fn parse_affected(text: &str) -> Result<Affected, String> {
    #[derive(Deserialize)]
    struct Raw {
        base: String,
        changed_files: Vec<String>,
        tags: Vec<String>,
        journeys: Vec<RawJourney>,
    }
    #[derive(Deserialize)]
    struct RawJourney {
        id: String,
    }
    let raw: Raw =
        serde_json::from_str(text).map_err(|e| format!("unexpected guard affected output: {e}"))?;
    Ok(Affected {
        base: raw.base,
        changed_files: raw.changed_files.len(),
        tags: raw.tags,
        ids: raw.journeys.into_iter().map(|j| j.id).collect(),
    })
}

/// `yes`, `no`, or `-` for `null` (the text form of `guard status`).
pub fn yes_no(flag: Option<bool>) -> &'static str {
    match flag {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    }
}
