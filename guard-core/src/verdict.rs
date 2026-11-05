//! Classification of Playwright JSON report results into a gate verdict.
//!
//! Only the report fields listed in `runner-env.md` are read; everything else
//! is ignored so newer Playwright versions keep working.

use crate::routing::SMOKE_TAG;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[cfg(test)]
pub(crate) mod tests;

/// Values of `not_implemented` in `verdict.json` (Phase 2+ features).
pub const NOT_IMPLEMENTED: [&str; 2] = ["baseline", "tracker"];

#[derive(Debug, Clone, Deserialize)]
pub struct PwReport {
    #[serde(default)]
    pub suites: Vec<PwSuite>,
    #[serde(default)]
    pub errors: Vec<serde_json::Value>,
    #[serde(default)]
    pub stats: Option<PwStats>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwSuite {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub suites: Vec<PwSuite>,
    #[serde(default)]
    pub specs: Vec<PwSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwSpec {
    pub title: String,
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub tests: Vec<PwTest>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwTest {
    #[serde(rename = "projectName", default)]
    pub project_name: String,
    pub status: PwStatus,
    #[serde(default)]
    pub results: Vec<PwResult>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PwStatus {
    Expected,
    Unexpected,
    Flaky,
    Skipped,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwResult {
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub retry: u32,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<PwError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwError {
    #[serde(default)]
    pub message: Option<String>,
    /// Where the error was raised (an absolute path, as Playwright writes it).
    #[serde(default)]
    pub location: Option<PwLocation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwLocation {
    pub file: String,
    #[serde(default)]
    pub line: u32,
}

/// Where a failing or flaky test's last failing attempt raised its error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureLocation {
    /// The test title as [`classify`] writes it (describe chain included).
    pub title: String,
    pub spec_file: String,
    pub spec_line: u32,
    pub project: String,
    /// As the report has it (usually absolute).
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PwStats {
    #[serde(default)]
    pub duration: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Classification {
    Passed,
    Flaky,
    Failed,
    Skipped,
}

/// One classified test (one `tests[]` entry of a spec, i.e. one project).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TestOutcome {
    pub title: String,
    pub file: String,
    pub line: u32,
    pub project: String,
    pub tags: Vec<String>,
    pub journey: Option<String>,
    pub status: Classification,
    pub blocking: bool,
    pub attempts: usize,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct Counts {
    pub passed: usize,
    pub flaky: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Green,
    Red,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Green => "green",
            Verdict::Red => "red",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Verdict::Green => 0,
            Verdict::Red => 1,
        }
    }
}

/// How guard reads a `npx playwright test` exit (runner-env exit table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Passed,
    TestsFailed,
    Infrastructure(String),
}

impl RunOutcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            RunOutcome::Passed => 0,
            RunOutcome::TestsFailed => 1,
            RunOutcome::Infrastructure(_) => 3,
        }
    }
}

/// `.guard/verdict.json`.
#[derive(Debug, Clone, Serialize)]
pub struct VerdictDocument {
    pub version: u32,
    pub run_id: String,
    pub generated_at: String,
    pub verdict: Verdict,
    pub counts: Counts,
    pub tests: Vec<TestOutcome>,
    pub baseline: Option<serde_json::Value>,
    pub not_implemented: Vec<String>,
}

impl VerdictDocument {
    pub fn new(run_id: String, generated_at: String, tests: Vec<TestOutcome>) -> Self {
        Self {
            version: 1,
            run_id,
            generated_at,
            verdict: decide(&tests),
            counts: count(&tests),
            tests,
            baseline: None,
            not_implemented: NOT_IMPLEMENTED.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid Playwright JSON report: {0}")]
pub struct ReportError(pub String);

impl PwReport {
    /// `stats.duration`, rounded to whole milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.stats.as_ref().map_or(0, |s| round_ms(s.duration))
    }

    /// Number of `tests[]` entries across every spec.
    pub fn test_count(&self) -> usize {
        fn walk(suite: &PwSuite) -> usize {
            suite.specs.iter().map(|s| s.tests.len()).sum::<usize>()
                + suite.suites.iter().map(walk).sum::<usize>()
        }
        self.suites.iter().map(walk).sum()
    }

    /// True when the report carries top-level (config or global) errors.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }
}

fn round_ms(value: f64) -> u64 {
    if value.is_finite() && value > 0.0 {
        value.round() as u64
    } else {
        0
    }
}

pub fn parse_report(text: &str) -> Result<PwReport, ReportError> {
    serde_json::from_str(text).map_err(|e| ReportError(e.to_string()))
}

/// Classifies every test, sorted by file, line, then project.
pub fn classify(report: &PwReport) -> Vec<TestOutcome> {
    let mut tests = Vec::new();
    for root in &report.suites {
        // Root suites are files: their title is the file name, not a describe.
        collect(root, &mut Vec::new(), &mut tests);
    }
    tests.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then_with(|| a.project.cmp(&b.project))
    });
    tests
}

fn collect<'a>(suite: &'a PwSuite, chain: &mut Vec<&'a str>, out: &mut Vec<TestOutcome>) {
    for spec in &suite.specs {
        let mut title: Vec<&str> = chain.clone();
        title.push(&spec.title);
        let title = title.join(" > ");
        let tags = normalize_tags(&spec.tags);
        let journey = tags
            .iter()
            .find_map(|t| t.strip_prefix("@journey:"))
            .map(str::to_string);
        let smoke = tags.iter().any(|t| t == SMOKE_TAG);
        for test in &spec.tests {
            let status = match test.status {
                PwStatus::Expected => Classification::Passed,
                PwStatus::Flaky => Classification::Flaky,
                PwStatus::Unexpected => Classification::Failed,
                PwStatus::Skipped => Classification::Skipped,
            };
            let blocking = match status {
                Classification::Failed => true,
                Classification::Flaky => smoke,
                Classification::Passed | Classification::Skipped => false,
            };
            out.push(TestOutcome {
                title: title.clone(),
                file: spec.file.clone(),
                line: spec.line,
                project: test.project_name.clone(),
                tags: tags.clone(),
                journey: journey.clone(),
                status,
                blocking,
                attempts: test.results.len(),
                duration_ms: test.results.iter().map(|r| round_ms(r.duration)).sum(),
                error: last_failure_line(&test.results),
            });
        }
    }
    for child in &suite.suites {
        chain.push(&child.title);
        collect(child, chain, out);
        chain.pop();
    }
}

/// Adds the leading `@` when Playwright omitted it; sorted and deduplicated.
fn normalize_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .filter(|t| !t.is_empty())
        .map(|t| {
            if t.starts_with('@') {
                t.clone()
            } else {
                format!("@{t}")
            }
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The error location of every failing or flaky test that has one, in
/// report order.
pub fn failure_locations(report: &PwReport) -> Vec<FailureLocation> {
    fn walk<'a>(suite: &'a PwSuite, chain: &mut Vec<&'a str>, out: &mut Vec<FailureLocation>) {
        for spec in &suite.specs {
            let mut title = chain.clone();
            title.push(&spec.title);
            let title = title.join(" > ");
            for test in &spec.tests {
                if !matches!(test.status, PwStatus::Unexpected | PwStatus::Flaky) {
                    continue;
                }
                let location = test
                    .results
                    .iter()
                    .rev()
                    .filter(|r| !matches!(r.status.as_deref(), Some("passed") | Some("skipped")))
                    .find_map(|r| r.error.as_ref()?.location.as_ref());
                if let Some(location) = location {
                    out.push(FailureLocation {
                        title: title.clone(),
                        spec_file: spec.file.clone(),
                        spec_line: spec.line,
                        project: test.project_name.clone(),
                        file: location.file.clone(),
                        line: location.line,
                    });
                }
            }
        }
        for child in &suite.suites {
            chain.push(&child.title);
            walk(child, chain, out);
            chain.pop();
        }
    }
    let mut out = Vec::new();
    for suite in &report.suites {
        walk(suite, &mut Vec::new(), &mut out);
    }
    out
}

/// First line of the last failing attempt's error message, without ANSI codes.
fn last_failure_line(results: &[PwResult]) -> Option<String> {
    results
        .iter()
        .rev()
        .filter(|r| !matches!(r.status.as_deref(), Some("passed") | Some("skipped")))
        .find_map(|r| r.error.as_ref()?.message.as_deref())
        .map(|m| {
            strip_ansi(m)
                .lines()
                .next()
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // CSI sequence: parameters and intermediates, then one final byte in @..~.
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn count(tests: &[TestOutcome]) -> Counts {
    let mut counts = Counts::default();
    for test in tests {
        match test.status {
            Classification::Passed => counts.passed += 1,
            Classification::Flaky => counts.flaky += 1,
            Classification::Failed => counts.failed += 1,
            Classification::Skipped => counts.skipped += 1,
        }
    }
    counts
}

/// `red` if any test is blocking.
pub fn decide(tests: &[TestOutcome]) -> Verdict {
    if tests.iter().any(|t| t.blocking) {
        Verdict::Red
    } else {
        Verdict::Green
    }
}

/// `guard test` exit: `1` when at least one test failed unexpectedly.
pub fn local_exit_code(tests: &[TestOutcome]) -> i32 {
    i32::from(tests.iter().any(|t| t.status == Classification::Failed))
}

/// Applies the runner-env exit table. `exit` is `None` when Playwright was
/// terminated by a signal; `report` is `None` when it is missing or unparseable.
pub fn interpret_exit(exit: Option<i32>, report: Option<&PwReport>) -> RunOutcome {
    let infra = |reason: &str| RunOutcome::Infrastructure(reason.to_string());
    match (exit, report) {
        (None, _) => infra("Playwright was terminated by a signal"),
        (Some(130), _) => infra("Playwright was interrupted (exit 130)"),
        (Some(0), Some(_)) => RunOutcome::Passed,
        (Some(0 | 1), None) => infra("Playwright produced no parseable JSON report"),
        (Some(1), Some(r)) if r.has_errors() => infra("Playwright reported top-level errors"),
        (Some(1), Some(r)) if r.test_count() == 0 => infra("Playwright ran zero tests"),
        (Some(1), Some(_)) => RunOutcome::TestsFailed,
        (Some(code), _) => {
            RunOutcome::Infrastructure(format!("Playwright exited with code {code}"))
        }
    }
}
