//! The last local run: `<repo>/e2e-playwright/results/report.json` (the
//! Playwright JSON reporter, runner-env.md), mapped to journeys through the
//! `@journey:<id>` tag, plus `.guard/durations.json` for history.

use crate::log_buffer::strip_ansi;
use guard_core::durations::DurationHistory;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[cfg(test)]
mod tests;

const JOURNEY_TAG: &str = "@journey:";

/// Aggregated result of every test of one journey.
#[derive(Debug, Clone, PartialEq)]
pub struct JourneyResult {
    pub outcome: Outcome,
    /// Sum of every attempt of every test.
    pub duration_ms: u64,
    /// Number of tests (one per spec and project).
    pub tests: usize,
    /// The first error message, without terminal colors.
    pub error: Option<String>,
    pub attachments: Vec<Attachment>,
    /// Spec files, relative to the Playwright dir, in report order.
    pub files: Vec<String>,
}

/// Ordered from best to worst, so the worst test of a journey wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Outcome {
    Skipped,
    Passed,
    Flaky,
    Failed,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Passed => "passed",
            Self::Flaky => "flaky",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    /// As written by Playwright: usually absolute.
    pub path: PathBuf,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.content_type.starts_with("image/")
    }

    /// The path on disk; a relative one is relative to the Playwright dir.
    pub fn resolved(&self, e2e_dir: &Path) -> PathBuf {
        if self.path.is_absolute() {
            self.path.clone()
        } else {
            e2e_dir.join(&self.path)
        }
    }
}

#[derive(Deserialize)]
struct Report {
    #[serde(default)]
    suites: Vec<Suite>,
}

#[derive(Deserialize)]
struct Suite {
    #[serde(default)]
    suites: Vec<Suite>,
    #[serde(default)]
    specs: Vec<Spec>,
}

#[derive(Deserialize)]
struct Spec {
    #[serde(default)]
    file: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    tests: Vec<Test>,
}

#[derive(Deserialize)]
struct Test {
    status: String,
    #[serde(default)]
    results: Vec<TestResult>,
}

#[derive(Deserialize)]
struct TestResult {
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    error: Option<TestError>,
    #[serde(default)]
    attachments: Vec<RawAttachment>,
}

#[derive(Deserialize)]
struct TestError {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
struct RawAttachment {
    #[serde(default)]
    name: String,
    #[serde(rename = "contentType", default)]
    content_type: String,
    #[serde(default)]
    path: Option<String>,
}

pub fn report_path(repo: &Path) -> PathBuf {
    crate::catalog::e2e_dir(repo)
        .join("results")
        .join("report.json")
}

pub fn durations_path(repo: &Path) -> PathBuf {
    repo.join(".guard").join("durations.json")
}

/// `Ok(None)` when there is no report yet (no run so far).
pub fn load(repo: &Path) -> Result<Option<BTreeMap<String, JourneyResult>>, String> {
    let path = report_path(repo);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_report(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// `Ok(None)` when the file is missing (no history, as for `guard gate plan`).
pub fn load_durations(repo: &Path) -> Result<Option<DurationHistory>, String> {
    let path = durations_path(repo);
    match std::fs::read_to_string(&path) {
        Ok(text) => guard_core::durations::parse_history(&text)
            .map(Some)
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Journey id -> aggregated result. Tests without a journey tag are ignored.
pub fn parse_report(text: &str) -> Result<BTreeMap<String, JourneyResult>, String> {
    let report: Report =
        serde_json::from_str(text).map_err(|e| format!("malformed report.json: {e}"))?;
    let mut out = BTreeMap::new();
    let mut stack: Vec<&Suite> = report.suites.iter().collect();
    // Depth-first in file order.
    stack.reverse();
    while let Some(suite) = stack.pop() {
        for spec in &suite.specs {
            add_spec(&mut out, spec);
        }
        stack.extend(suite.suites.iter().rev());
    }
    Ok(out)
}

fn add_spec(out: &mut BTreeMap<String, JourneyResult>, spec: &Spec) {
    let Some(id) = spec.tags.iter().find_map(|tag| {
        let tag = tag.strip_prefix('@').unwrap_or(tag);
        tag.strip_prefix(&JOURNEY_TAG[1..])
    }) else {
        return;
    };
    let entry = out.entry(id.to_string()).or_insert_with(|| JourneyResult {
        outcome: Outcome::Skipped,
        duration_ms: 0,
        tests: 0,
        error: None,
        attachments: Vec::new(),
        files: Vec::new(),
    });
    if !spec.file.is_empty() && !entry.files.contains(&spec.file) {
        entry.files.push(spec.file.clone());
    }
    for test in &spec.tests {
        entry.tests += 1;
        entry.outcome = entry.outcome.max(outcome_of(&test.status));
        for result in &test.results {
            entry.duration_ms += result.duration.max(0.0).round() as u64;
            if entry.error.is_none()
                && let Some(message) = result.error.as_ref().and_then(|e| e.message.as_deref())
            {
                entry.error = Some(strip_ansi(message));
            }
            entry
                .attachments
                .extend(result.attachments.iter().filter_map(|a| {
                    a.path.as_ref().map(|path| Attachment {
                        name: a.name.clone(),
                        content_type: a.content_type.clone(),
                        path: PathBuf::from(path),
                    })
                }));
        }
    }
}

fn outcome_of(status: &str) -> Outcome {
    match status {
        "expected" => Outcome::Passed,
        "flaky" => Outcome::Flaky,
        "skipped" => Outcome::Skipped,
        // `unexpected` and anything unknown.
        _ => Outcome::Failed,
    }
}

/// What identifies one write of `report.json`: its modification time and
/// length. A run that leaves both unchanged wrote no report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportStamp {
    pub modified: Option<SystemTime>,
    pub len: u64,
}

/// The stamp of `<repo>/e2e-playwright/results/report.json`, `None` when it
/// does not exist or cannot be read.
pub fn report_stamp(repo: &Path) -> Option<ReportStamp> {
    let meta = std::fs::metadata(report_path(repo)).ok()?;
    Some(ReportStamp {
        modified: meta.modified().ok(),
        len: meta.len(),
    })
}

/// True when a run left a report that differs from the one before it.
pub fn report_is_fresh(before: Option<ReportStamp>, after: Option<ReportStamp>) -> bool {
    after.is_some() && after != before
}

/// The failed and flaky journeys a run produced: none when its report is
/// stale, because those results belong to an earlier run.
pub fn run_counts(fresh: bool, results: &BTreeMap<String, JourneyResult>) -> (usize, usize) {
    if !fresh {
        return (0, 0);
    }
    let count = |o: Outcome| results.values().filter(|r| r.outcome == o).count();
    (count(Outcome::Failed), count(Outcome::Flaky))
}

/// The worst outcome of a report, `None` when it maps no journey.
pub fn report_outcome(results: &BTreeMap<String, JourneyResult>) -> Option<Outcome> {
    results.values().map(|r| r.outcome).max()
}

/// `just now`, `5 min ago`, `3 h ago`, `2 days ago`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86_400 => format!("{} h ago", secs / 3600),
        _ => {
            let days = secs / 86_400;
            format!("{days} day{} ago", if days == 1 { "" } else { "s" })
        }
    }
}

/// The status bar text for results read from disk, not run this session.
pub fn last_report_label(outcome: Outcome, age: Option<Duration>) -> String {
    match age {
        Some(age) => format!("Last report: {} ({})", outcome.label(), format_age(age)),
        None => format!("Last report: {}", outcome.label()),
    }
}

/// Historical duration of `files` from `.guard/durations.json`: the sum of the
/// known ones, `None` when none is known.
pub fn history_ms(history: &DurationHistory, files: &[String]) -> Option<u64> {
    let known: Vec<u64> = files
        .iter()
        .filter_map(|f| history.files.get(f).map(|e| e.duration_ms))
        .collect();
    (!known.is_empty()).then(|| known.iter().sum())
}

/// `0.9s`, `12.4s`, `2m05s` (tenths truncated).
pub fn format_duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("{}.{}s", ms / 1000, (ms % 1000) / 100)
    } else {
        let secs = ms / 1000;
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}
