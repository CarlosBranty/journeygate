//! The gate reporting use case: fingerprinted Bugs, occurrence comments,
//! screenshots and the change-ticket summary (`docs/contracts/reporting.md`).
//!
//! `report_gate` never fails: every tracker problem becomes a warning in
//! `ReportOutcome.warnings`, because reporting never changes the verdict.

use crate::adf::{
    self, bullet_list, code, code_block, doc, heading, link, paragraph, strong, text,
};
use guard_core::fingerprint::{Fingerprint, normalize_error};
use guard_core::ports::{
    Attachment, BaselineOutcome, BugAction, EvidenceBundle, EvidenceFile, EvidenceKind, GateReport,
    GateTest, GateVerdict, IssueTracker, NewBug, ReportOutcome, ReportedBug, StatusCategory,
    TestStatus, TrackerError,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// Most Bugs created or commented per gate run.
pub const MAX_BUGS_PER_RUN: usize = 10;
/// Most screenshots attached per Bug.
pub const MAX_SCREENSHOTS_PER_BUG: usize = 3;
/// Largest screenshot attached (10 MiB).
pub const MAX_SCREENSHOT_BYTES: usize = 10 * 1024 * 1024;
/// Jira summary limit, in characters.
pub const MAX_SUMMARY_CHARS: usize = 255;

const FLOW_CHANGE_SENTENCE: &str =
    "If the flow change is intentional, update this journey in the same PR.";

/// Reads screenshot bytes: local `test-results/` (CLI) or `EvidenceStore`
/// objects by `EvidenceFile.key` (server). `None` when unavailable.
#[async_trait::async_trait]
pub trait ScreenshotSource: Send + Sync {
    async fn read(&self, file: &EvidenceFile) -> Option<Vec<u8>>;
}

/// A source with no screenshots (attachments are skipped silently).
pub struct NoScreenshots;

#[async_trait::async_trait]
impl ScreenshotSource for NoScreenshots {
    async fn read(&self, _file: &EvidenceFile) -> Option<Vec<u8>> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportOptions {
    /// `[tracker].project`: where Bugs are created.
    pub project: String,
    /// `JIRA_PRE_EXISTING_PRIORITY` (default `Highest`).
    pub pre_existing_priority: String,
}

impl ReportOptions {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            pre_existing_priority: "Highest".into(),
        }
    }
}

// ------------------------------------------------------------ selection

/// `failed` with baseline `regression` or `pre_existing`.
fn reportable_kind(test: &GateTest) -> Option<BaselineOutcome> {
    match (test.status, test.baseline) {
        (
            TestStatus::Failed,
            Some(b @ (BaselineOutcome::Regression | BaselineOutcome::PreExisting)),
        ) => Some(b),
        _ => None,
    }
}

/// Fingerprints that get a Bug slot this run, in `tests[]` order (first
/// [`MAX_BUGS_PER_RUN`] distinct fingerprints).
fn slotted(report: &GateReport) -> Vec<Fingerprint> {
    let mut seen = BTreeSet::new();
    report
        .tests
        .iter()
        .filter(|t| reportable_kind(t).is_some())
        .filter_map(|t| t.fingerprint.clone())
        .filter(|fp| seen.insert(fp.clone()))
        .take(MAX_BUGS_PER_RUN)
        .collect()
}

// ------------------------------------------------------------ use case

pub async fn report_gate(
    tracker: &dyn IssueTracker,
    screenshots: &dyn ScreenshotSource,
    report: &GateReport,
    options: &ReportOptions,
) -> ReportOutcome {
    let mut outcome = ReportOutcome::default();
    let slots = slotted(report);
    let mut done: BTreeSet<Fingerprint> = BTreeSet::new();
    let mut unreachable = false;
    let mut skipped = 0usize;

    for test in &report.tests {
        let Some(kind) = reportable_kind(test) else {
            continue;
        };
        let Some(fp) = test.fingerprint.clone() else {
            outcome.warnings.push(format!(
                "{}: failed without a fingerprint, not reported to Jira",
                test.test_id
            ));
            continue;
        };
        if !slots.contains(&fp) || !done.insert(fp.clone()) {
            continue;
        }
        if unreachable {
            skipped += 1;
            continue;
        }
        match report_one(tracker, test, kind, &fp, report, options).await {
            Ok(bug) => {
                unreachable =
                    attach_screenshots(tracker, screenshots, test, &bug.key, &mut outcome).await;
                outcome.bugs.push(bug);
            }
            Err((warning, transport)) => {
                outcome.warnings.push(warning);
                unreachable = transport;
            }
        }
    }

    // Circuit breaker: after a transport error every later call would wait
    // for its own timeout, so the rest of the run is skipped.
    if unreachable {
        let ticket = if report.ticket_key.is_some() {
            " and the ticket summary"
        } else {
            ""
        };
        outcome.warnings.push(format!(
            "Jira unreachable: {skipped} more Bug(s){ticket} not reported this run"
        ));
        return outcome;
    }
    if let Some(ticket) = &report.ticket_key {
        match tracker
            .comment_adf(ticket, &gate_summary_adf(report, &outcome))
            .await
        {
            Ok(_) => outcome.ticket_comment = Some(ticket.clone()),
            Err(e) => outcome
                .warnings
                .push(format!("gate summary comment on {ticket}: {e}")),
        }
    }
    outcome
}

async fn report_one(
    tracker: &dyn IssueTracker,
    test: &GateTest,
    kind: BaselineOutcome,
    fp: &Fingerprint,
    report: &GateReport,
    options: &ReportOptions,
) -> Result<ReportedBug, (String, bool)> {
    let failure = |what: String| {
        move |e: TrackerError| {
            let transport = matches!(e, TrackerError::Transport(_));
            (format!("{}: {what}: {e}", test.test_id), transport)
        }
    };
    let label = fp.label();
    let found = tracker
        .find_by_label(&label)
        .await
        .map_err(failure(format!("Jira search for {label}")))?;
    let bundle = test.evidence.as_ref();

    if let Some(open) = found
        .iter()
        .find(|i| i.status_category != StatusCategory::Done)
    {
        tracker
            .comment_adf(&open.key, &occurrence_comment_adf(test, bundle, report))
            .await
            .map_err(failure(format!("comment on {}", open.key)))?;
        return Ok(ReportedBug {
            fingerprint: fp.clone(),
            key: open.key.clone(),
            url: open.url.clone(),
            action: BugAction::Commented,
        });
    }

    let mut description = bug_description_adf(test, bundle, report);
    if let Some(closed) = found.first() {
        if let Some(content) = description["content"].as_array_mut() {
            content.push(paragraph(vec![text(format!(
                "Previously reported as {} (closed).",
                closed.key
            ))]));
        }
    }
    let regression = kind == BaselineOutcome::Regression;
    let bug = NewBug {
        project: options.project.clone(),
        summary: bug_summary(test),
        description_adf: description,
        labels: vec![
            "guard".into(),
            if regression {
                "guard-regression".into()
            } else {
                "guard-pre-existing".into()
            },
            label,
        ],
        priority: (!regression).then(|| options.pre_existing_priority.clone()),
        linked_issue: if regression {
            report.ticket_key.clone()
        } else {
            None
        },
    };
    let created = tracker
        .create_bug(&bug)
        .await
        .map_err(failure("create Bug".into()))?;
    Ok(ReportedBug {
        fingerprint: fp.clone(),
        key: created.key,
        url: created.url,
        action: BugAction::Created,
    })
}

fn is_png(file: &EvidenceFile) -> bool {
    file.kind == EvidenceKind::Screenshot
        && (file.content_type.eq_ignore_ascii_case("image/png")
            || file.name.to_ascii_lowercase().ends_with(".png"))
}

/// Attaches up to three screenshots; `true` when Jira was unreachable.
async fn attach_screenshots(
    tracker: &dyn IssueTracker,
    source: &dyn ScreenshotSource,
    test: &GateTest,
    key: &str,
    outcome: &mut ReportOutcome,
) -> bool {
    let Some(bundle) = &test.evidence else {
        return false;
    };
    for file in bundle
        .files
        .iter()
        .filter(|f| is_png(f))
        .take(MAX_SCREENSHOTS_PER_BUG)
    {
        let Some(bytes) = source.read(file).await else {
            outcome
                .warnings
                .push(format!("{key}: screenshot {} unavailable", file.name));
            continue;
        };
        if bytes.len() > MAX_SCREENSHOT_BYTES {
            outcome.warnings.push(format!(
                "{key}: screenshot {} is larger than 10 MiB, not attached",
                file.name
            ));
            continue;
        }
        let attachment = Attachment {
            file_name: file.name.clone(),
            content_type: "image/png".into(),
            bytes,
        };
        if let Err(e) = tracker.attach_file(key, &attachment).await {
            outcome
                .warnings
                .push(format!("{key}: attach {}: {e}", file.name));
            if matches!(e, TrackerError::Transport(_)) {
                return true;
            }
        }
    }
    false
}

// ------------------------------------------------------------ layouts

fn display_name(test: &GateTest) -> &str {
    test.journey.as_deref().unwrap_or(&test.title)
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// The raw first error line: the bundle's, else the test's.
fn error_line(test: &GateTest, bundle: Option<&EvidenceBundle>) -> String {
    let raw = bundle
        .map(|b| b.error.as_str())
        .or(test.error.as_deref())
        .unwrap_or("");
    strip_ansi(first_line(raw))
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn location(test: &GateTest, bundle: Option<&EvidenceBundle>) -> String {
    bundle
        .and_then(|b| b.location.clone())
        .or_else(|| test.evidence.as_ref().and_then(|b| b.location.clone()))
        .unwrap_or_else(|| format!("{}:{}", test.file, test.line))
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn base_branch(report: &GateReport) -> &str {
    report
        .pr
        .as_ref()
        .map(|p| p.base_branch.as_str())
        .filter(|b| !b.is_empty())
        .unwrap_or("default branch")
}

fn baseline_text(b: BaselineOutcome) -> &'static str {
    match b {
        BaselineOutcome::Regression => "passes (regression)",
        BaselineOutcome::PreExisting => "fails 3/3 (pre-existing)",
        BaselineOutcome::Flaky => "passes on a retry (flaky)",
        BaselineOutcome::Unavailable => "unavailable",
    }
}

/// `**Links** Trace · Video · Report · Pipeline · PR`, only those present.
fn links_paragraph(entries: Vec<(&str, Option<String>)>) -> Option<Value> {
    let links: Vec<Value> = entries
        .into_iter()
        .filter_map(|(label, url)| url.map(|u| link(label, u)))
        .collect();
    if links.is_empty() {
        return None;
    }
    let mut inlines = vec![strong("Links"), text(" ")];
    inlines.extend(adf::join(links, " · "));
    Some(paragraph(inlines))
}

fn evidence_url(bundle: Option<&EvidenceBundle>, kind: EvidenceKind) -> Option<String> {
    bundle?
        .files
        .iter()
        .find(|f| f.kind == kind)
        .and_then(|f| f.viewer_url.clone().or_else(|| f.url.clone()))
}

fn test_links(bundle: Option<&EvidenceBundle>, report: &GateReport) -> Option<Value> {
    links_paragraph(vec![
        ("Trace", evidence_url(bundle, EvidenceKind::Trace)),
        ("Video", evidence_url(bundle, EvidenceKind::Video)),
        ("Report", report.report_url.clone()),
        ("Pipeline", report.ci_url.clone()),
        ("PR", report.pr.as_ref().map(|p| p.url.clone())),
    ])
}

/// `[guard] <journey id, else title>: <normalized error>`, at most 255 characters.
pub fn bug_summary(test: &GateTest) -> String {
    let raw = test
        .evidence
        .as_ref()
        .map(|b| b.error.as_str())
        .or(test.error.as_deref())
        .unwrap_or("");
    let summary = format!("[guard] {}: {}", display_name(test), normalize_error(raw));
    if summary.chars().count() <= MAX_SUMMARY_CHARS {
        summary
    } else {
        let mut cut: String = summary.chars().take(MAX_SUMMARY_CHARS - 1).collect();
        cut.push('…');
        cut
    }
}

pub fn bug_description_adf(
    test: &GateTest,
    bundle: Option<&EvidenceBundle>,
    report: &GateReport,
) -> Value {
    let regression = test.baseline == Some(BaselineOutcome::Regression);
    let title = if regression {
        format!("Regression in {}", display_name(test))
    } else {
        format!("Already broken on {}", base_branch(report))
    };
    let mut blocks = vec![
        heading(3, vec![text(title)]),
        paragraph(vec![strong("Test"), text(" "), code(test.test_id.as_str())]),
        paragraph(vec![
            strong("Location"),
            text(" "),
            code(location(test, bundle)),
        ]),
        code_block(&error_line(test, bundle), None),
    ];

    let mut items: Vec<Vec<Value>> = bundle
        .map(|b| b.attempts.as_slice())
        .unwrap_or(&[])
        .iter()
        .map(|a| {
            vec![text(format!(
                "attempt {}: {} ({} ms)",
                a.retry + 1,
                a.status,
                a.duration_ms
            ))]
        })
        .collect();
    let baseline = bundle.and_then(|b| b.baseline).or(test.baseline);
    if let (Some(base), Some(outcome)) = (&report.base_sha, baseline) {
        items.push(vec![text(format!(
            "base commit {}: {}",
            short_sha(base),
            baseline_text(outcome)
        ))]);
    }
    if !items.is_empty() {
        blocks.push(bullet_list(items));
    }
    blocks.extend(test_links(bundle, report));

    let mut run_line = Vec::new();
    if let Some(seed) = bundle.and_then(|b| b.seed.as_deref()) {
        run_line.push(format!("Seed {seed}"));
    }
    run_line.push(format!("commit {}", short_sha(&report.head_sha)));
    run_line.push(format!("run {}", report.run_id));
    blocks.push(paragraph(vec![text(run_line.join(" · "))]));

    if regression {
        blocks.push(paragraph(vec![text(FLOW_CHANGE_SENTENCE)]));
    }
    doc(blocks)
}

pub fn occurrence_comment_adf(
    test: &GateTest,
    bundle: Option<&EvidenceBundle>,
    report: &GateReport,
) -> Value {
    let mut first = vec![
        strong("New occurrence"),
        text(" in run "),
        code(report.run_id.as_str()),
        text(" on commit "),
        code(short_sha(&report.head_sha)),
    ];
    if let Some(pr) = &report.pr {
        first.extend([
            text(" ("),
            link(format!("PR #{}", pr.number), pr.url.as_str()),
            text(")"),
        ]);
    }
    let mut blocks = vec![
        paragraph(first),
        code_block(&error_line(test, bundle), None),
    ];
    blocks.extend(test_links(bundle, report));
    doc(blocks)
}

pub fn gate_summary_adf(report: &GateReport, outcome: &ReportOutcome) -> Value {
    let c = &report.counts;
    let verdict = match report.verdict {
        GateVerdict::Green => "green",
        GateVerdict::Red => "red",
    };
    let mut blocks = vec![
        heading(3, vec![text(format!("guard: {verdict}"))]),
        paragraph(vec![text(format!(
            "passed {}, flaky {}, failed {} (regressions {}, pre-existing {}), skipped {}",
            c.passed, c.flaky, c.failed, c.regressions, c.pre_existing, c.skipped
        ))]),
    ];

    let slots = slotted(report);
    let bug_item = |test: &GateTest| -> Vec<Value> {
        let jira = test.fingerprint.as_ref().and_then(|fp| {
            outcome
                .bugs
                .iter()
                .find(|b| &b.fingerprint == fp)
                .map(|b| link(b.key.as_str(), b.url.as_str()))
        });
        let jira = jira.unwrap_or_else(|| {
            let limited = test
                .fingerprint
                .as_ref()
                .is_some_and(|fp| !slots.contains(fp));
            text(if limited {
                "not reported: limit reached"
            } else {
                "not reported"
            })
        });
        vec![
            text(format!("{} — ", display_name(test))),
            jira,
            text(" — "),
            code(location(test, None)),
        ]
    };

    let section = |blocks: &mut Vec<Value>, title: &str, items: Vec<Vec<Value>>| {
        if !items.is_empty() {
            blocks.push(heading(4, vec![text(title)]));
            blocks.push(bullet_list(items));
        }
    };

    let of_kind = |kind: BaselineOutcome| -> Vec<Vec<Value>> {
        report
            .tests
            .iter()
            .filter(|t| reportable_kind(t) == Some(kind))
            .map(bug_item)
            .collect()
    };
    section(
        &mut blocks,
        "Regressions",
        of_kind(BaselineOutcome::Regression),
    );
    section(
        &mut blocks,
        &format!("Already broken on {}", base_branch(report)),
        of_kind(BaselineOutcome::PreExisting),
    );

    let unconfirmed: Vec<Vec<Value>> = report
        .tests
        .iter()
        .filter_map(|t| {
            let label = match (t.status, t.baseline) {
                (TestStatus::Flaky, _) | (TestStatus::Failed, Some(BaselineOutcome::Flaky)) => {
                    "flaky"
                }
                (TestStatus::Failed, Some(BaselineOutcome::Unavailable) | None) => "unconfirmed",
                _ => return None,
            };
            Some(vec![
                text(format!("{} — {label} — ", display_name(t))),
                code(location(t, None)),
            ])
        })
        .collect();
    section(&mut blocks, "Unconfirmed or flaky", unconfirmed);

    let mut modified: Vec<Vec<Value>> = report
        .change_flags
        .iter()
        .map(|f| vec![text(f.as_str())])
        .collect();
    for change in &report.journey_changes {
        for flag in &change.flags {
            modified.push(vec![text(format!("{}: {flag}", change.journey))]);
        }
    }
    section(&mut blocks, "Modified journeys", modified);

    blocks.extend(links_paragraph(vec![
        ("Report", report.report_url.clone()),
        ("Pipeline", report.ci_url.clone()),
        ("PR", report.pr.as_ref().map(|p| p.url.clone())),
    ]));
    doc(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_is_stripped_from_the_raw_error_line() {
        assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[0m text"), "red text");
        assert_eq!(first_line("\n\n  a \nb"), "a");
    }

    #[test]
    fn short_sha_handles_short_input() {
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha("0123456789abcdef"), "0123456789ab");
    }

    #[tokio::test]
    async fn no_screenshots_source_returns_none() {
        let file = EvidenceFile {
            kind: EvidenceKind::Screenshot,
            name: "a.png".into(),
            content_type: "image/png".into(),
            key: None,
            url: None,
            viewer_url: None,
        };
        assert!(NoScreenshots.read(&file).await.is_none());
    }
}
