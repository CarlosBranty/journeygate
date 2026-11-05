//! Gate-result reporting to Jira (reporting.md), behind [`GateReporter`].
//!
//! The canonical implementation with the golden ADF layouts is
//! `guard_tracker::report::report_gate`; the integrator adapts it to
//! [`GateReporter`]. This crate depends only on the ports, so it ships
//! [`PortGateReporter`]: the same selection, dedup and limit rules built on
//! `IssueTracker` + `EvidenceStore`, with plain ADF bodies. It is the default
//! when no reporter is injected.
//!
//! The server never publishes the PR comment or the check: `guard gate
//! finalize` does that with the CI token (finalize-phase2.md), so
//! `pr_comment` and `status_published` are always `false` here.

use guard_core::config::TargetConfig;
use guard_core::fingerprint::normalize_error;
use guard_core::ports::{
    Attachment, BaselineOutcome, BugAction, EvidenceKind, EvidenceStore, Fingerprint, GateReport,
    GateTest, IssueTracker, NewBug, ReportOutcome, ReportedBug, StatusCategory, TestStatus,
};
use serde_json::{Value, json};
use std::sync::Arc;

#[async_trait::async_trait]
pub trait GateReporter: Send + Sync {
    /// Never fails: problems go to `ReportOutcome.warnings`.
    async fn report(&self, report: &GateReport) -> ReportOutcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReporterOptions {
    /// `[tracker].project`; without it no Bug is created.
    pub project: Option<String>,
    /// `JIRA_PRE_EXISTING_PRIORITY`, default `Highest`.
    pub pre_existing_priority: String,
    /// Bugs created or commented per gate run.
    pub max_bugs: usize,
    pub max_screenshots: usize,
    pub max_screenshot_bytes: usize,
}

impl ReporterOptions {
    pub fn from_config(config: &TargetConfig) -> Self {
        Self {
            project: config.tracker.as_ref().and_then(|t| t.project.clone()),
            pre_existing_priority: "Highest".into(),
            max_bugs: 10,
            max_screenshots: 3,
            max_screenshot_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Jira's summary limit.
const SUMMARY_MAX: usize = 255;

/// `[guard] <journey id, else title>: <normalized error>`, cut to 255 chars.
pub fn bug_summary(test: &GateTest) -> String {
    let subject = test.journey.as_deref().unwrap_or(&test.title);
    let error = normalize_error(test.error.as_deref().unwrap_or("failed"));
    let full = format!("[guard] {subject}: {error}");
    if full.chars().count() <= SUMMARY_MAX {
        return full;
    }
    let mut cut: String = full.chars().take(SUMMARY_MAX - 1).collect();
    cut.push('…');
    cut
}

/// Tests that get a Bug: failed 3/3 and confirmed by the baseline run.
pub fn reportable(test: &GateTest) -> Option<BaselineOutcome> {
    match (test.status, test.baseline) {
        (
            TestStatus::Failed,
            Some(b @ (BaselineOutcome::Regression | BaselineOutcome::PreExisting)),
        ) => Some(b),
        _ => None,
    }
}

fn text(t: impl Into<String>) -> Value {
    json!({"type": "text", "text": t.into()})
}

fn paragraph(t: impl Into<String>) -> Value {
    json!({"type": "paragraph", "content": [text(t)]})
}

fn heading(level: u8, t: impl Into<String>) -> Value {
    json!({"type": "heading", "attrs": {"level": level}, "content": [text(t)]})
}

fn code_block(t: impl Into<String>) -> Value {
    json!({"type": "codeBlock", "content": [text(t)]})
}

fn bullets(items: impl IntoIterator<Item = String>) -> Value {
    let items: Vec<Value> = items
        .into_iter()
        .map(|i| json!({"type": "listItem", "content": [paragraph(i)]}))
        .collect();
    json!({"type": "bulletList", "content": items})
}

fn doc(content: Vec<Value>) -> Value {
    json!({"type": "doc", "version": 1, "content": content})
}

/// The first 12 characters of a sha; a value whose byte 12 is not a char
/// boundary (it comes from the client) is kept whole rather than panicking.
fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

fn subject(test: &GateTest) -> &str {
    test.journey.as_deref().unwrap_or(&test.title)
}

fn links(test: &GateTest, report: &GateReport) -> Option<String> {
    let files = test.evidence.iter().flat_map(|e| e.files.iter());
    let mut parts: Vec<String> = Vec::new();
    for f in files {
        match (f.kind, f.viewer_url.as_ref().or(f.url.as_ref())) {
            (EvidenceKind::Trace, Some(u)) => parts.push(format!("Trace {u}")),
            (EvidenceKind::Video, Some(u)) => parts.push(format!("Video {u}")),
            _ => {}
        }
    }
    let extra = [
        ("Report", report.report_url.as_ref()),
        ("Pipeline", report.ci_url.as_ref()),
        ("PR", report.pr.as_ref().map(|p| &p.url)),
    ];
    parts.extend(
        extra
            .into_iter()
            .filter_map(|(n, u)| u.map(|u| format!("{n} {u}"))),
    );
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The PR's base branch; without one, the gate compared against the
/// detected default base (cli.md "Base resolution"), not necessarily `main`.
fn base_branch_of(pr: Option<&guard_core::ports::PrContext>) -> &str {
    pr.map(|p| p.base_branch.as_str())
        .filter(|b| !b.is_empty())
        .unwrap_or("default branch")
}

fn bug_description(test: &GateTest, baseline: BaselineOutcome, report: &GateReport) -> Value {
    let base_branch = base_branch_of(report.pr.as_ref());
    let title = match baseline {
        BaselineOutcome::PreExisting => format!("Already broken on {base_branch}"),
        _ => format!("Regression in {}", subject(test)),
    };
    let location = test
        .evidence
        .as_ref()
        .and_then(|e| e.location.clone())
        .unwrap_or_else(|| format!("{}:{}", test.file, test.line));
    let mut content = vec![
        heading(3, title),
        paragraph(format!("Test {}", test.test_id)),
        paragraph(format!("Location {location}")),
        code_block(
            test.error
                .as_deref()
                .and_then(|e| e.lines().find(|l| !l.trim().is_empty()))
                .unwrap_or_default()
                .trim()
                .to_string(),
        ),
    ];
    if let Some(l) = links(test, report) {
        content.push(paragraph(format!("Links: {l}")));
    }
    content.push(paragraph(format!(
        "commit {} · run {}",
        short(&report.head_sha),
        report.run_id
    )));
    if baseline == BaselineOutcome::Regression {
        content.push(paragraph(
            "If the flow change is intentional, update this journey in the same PR.",
        ));
    }
    doc(content)
}

fn occurrence_comment(test: &GateTest, report: &GateReport) -> Value {
    let mut content = vec![
        paragraph(format!(
            "New occurrence in run {} on commit {}",
            report.run_id,
            short(&report.head_sha)
        )),
        code_block(
            test.error
                .as_deref()
                .and_then(|e| e.lines().find(|l| !l.trim().is_empty()))
                .unwrap_or_default()
                .trim()
                .to_string(),
        ),
    ];
    if let Some(l) = links(test, report) {
        content.push(paragraph(format!("Links: {l}")));
    }
    doc(content)
}

fn gate_summary(report: &GateReport, outcome: &ReportOutcome) -> Value {
    let c = &report.counts;
    let verdict = match report.verdict {
        guard_core::ports::GateVerdict::Green => "green",
        guard_core::ports::GateVerdict::Red => "red",
    };
    let mut content = vec![
        heading(3, format!("guard: {verdict}")),
        paragraph(format!(
            "passed {}, flaky {}, failed {} (regressions {}, pre-existing {}), skipped {}",
            c.passed, c.flaky, c.failed, c.regressions, c.pre_existing, c.skipped
        )),
    ];
    let bug_for = |fp: &Option<Fingerprint>| {
        fp.as_ref()
            .and_then(|fp| outcome.bugs.iter().find(|b| &b.fingerprint == fp))
            .map_or("—".to_string(), |b| b.key.clone())
    };
    let section = |title: &str, items: Vec<String>, content: &mut Vec<Value>| {
        if !items.is_empty() {
            content.push(heading(4, title));
            content.push(bullets(items));
        }
    };
    let rows = |want: BaselineOutcome| -> Vec<String> {
        report
            .tests
            .iter()
            .filter(|t| reportable(t) == Some(want))
            .map(|t| format!("{} — {}", subject(t), bug_for(&t.fingerprint)))
            .collect()
    };
    section(
        "Regressions",
        rows(BaselineOutcome::Regression),
        &mut content,
    );
    section(
        "Already broken on main",
        rows(BaselineOutcome::PreExisting),
        &mut content,
    );
    let unconfirmed: Vec<String> = report
        .tests
        .iter()
        .filter(|t| {
            t.status == TestStatus::Flaky
                || (t.status == TestStatus::Failed && reportable(t).is_none())
        })
        .map(|t| subject(t).to_string())
        .collect();
    section("Unconfirmed or flaky", unconfirmed, &mut content);
    let modified: Vec<String> = report
        .change_flags
        .iter()
        .cloned()
        .chain(
            report
                .journey_changes
                .iter()
                .flat_map(|j| j.flags.iter().map(move |f| format!("{}: {f}", j.journey))),
        )
        .collect();
    section("Modified journeys", modified, &mut content);
    doc(content)
}

/// [`GateReporter`] over the ports.
pub struct PortGateReporter {
    tracker: Arc<dyn IssueTracker>,
    evidence: Option<Arc<dyn EvidenceStore>>,
    options: ReporterOptions,
}

impl PortGateReporter {
    pub fn new(
        tracker: Arc<dyn IssueTracker>,
        evidence: Option<Arc<dyn EvidenceStore>>,
        options: ReporterOptions,
    ) -> Self {
        Self {
            tracker,
            evidence,
            options,
        }
    }

    async fn report_one(
        &self,
        project: &str,
        test: &GateTest,
        fingerprint: &Fingerprint,
        baseline: BaselineOutcome,
        report: &GateReport,
        warnings: &mut Vec<String>,
    ) -> Result<ReportedBug, String> {
        let label = fingerprint.label();
        let found = self
            .tracker
            .find_by_label(&label)
            .await
            .map_err(|e| format!("{label}: search failed: {e}"))?;
        let bug = if let Some(open) = found
            .iter()
            .find(|i| i.status_category != StatusCategory::Done)
        {
            self.tracker
                .comment_adf(&open.key, &occurrence_comment(test, report))
                .await
                .map_err(|e| format!("{label}: comment on {} failed: {e}", open.key))?;
            ReportedBug {
                fingerprint: fingerprint.clone(),
                key: open.key.clone(),
                url: open.url.clone(),
                action: BugAction::Commented,
            }
        } else {
            let mut description = bug_description(test, baseline, report);
            if let Some(closed) = found.first() {
                if let Some(content) = description["content"].as_array_mut() {
                    content.push(paragraph(format!(
                        "Previously reported as {} (closed).",
                        closed.key
                    )));
                }
            }
            let (kind_label, priority, linked) = match baseline {
                BaselineOutcome::PreExisting => (
                    "guard-pre-existing",
                    Some(self.options.pre_existing_priority.clone()),
                    None,
                ),
                _ => ("guard-regression", None, report.ticket_key.clone()),
            };
            let created = self
                .tracker
                .create_bug(&NewBug {
                    project: project.to_string(),
                    summary: bug_summary(test),
                    description_adf: description,
                    labels: vec!["guard".into(), kind_label.into(), label.clone()],
                    priority,
                    linked_issue: linked,
                })
                .await
                .map_err(|e| format!("{label}: create bug failed: {e}"))?;
            ReportedBug {
                fingerprint: fingerprint.clone(),
                key: created.key,
                url: created.url,
                action: BugAction::Created,
            }
        };
        self.attach_screenshots(&bug.key, test, warnings).await;
        Ok(bug)
    }

    async fn attach_screenshots(&self, key: &str, test: &GateTest, warnings: &mut Vec<String>) {
        let Some(evidence) = &self.evidence else {
            return;
        };
        let shots = test
            .evidence
            .iter()
            .flat_map(|e| e.files.iter())
            .filter(|f| f.kind == EvidenceKind::Screenshot)
            .filter_map(|f| f.key.as_ref().map(|k| (f, k)))
            .take(self.options.max_screenshots);
        for (file, object_key) in shots {
            let bytes = match evidence.get_object(object_key).await {
                Ok(bytes) => bytes,
                Err(e) => {
                    warnings.push(format!("{key}: screenshot {object_key} unavailable: {e}"));
                    continue;
                }
            };
            if bytes.len() > self.options.max_screenshot_bytes {
                warnings.push(format!(
                    "{key}: screenshot {object_key} too large, linked only"
                ));
                continue;
            }
            let attachment = Attachment {
                file_name: file.name.clone(),
                content_type: "image/png".into(),
                bytes,
            };
            if let Err(e) = self.tracker.attach_file(key, &attachment).await {
                warnings.push(format!("{key}: attaching {} failed: {e}", file.name));
            }
        }
    }
}

#[async_trait::async_trait]
impl GateReporter for PortGateReporter {
    async fn report(&self, report: &GateReport) -> ReportOutcome {
        let mut outcome = ReportOutcome::default();
        let candidates: Vec<(&GateTest, BaselineOutcome)> = report
            .tests
            .iter()
            .filter_map(|t| reportable(t).map(|b| (t, b)))
            .collect();
        match self.options.project.as_deref() {
            None if !candidates.is_empty() => outcome.warnings.push(format!(
                "tracker.project is not configured: {} failures not reported",
                candidates.len()
            )),
            None => {}
            Some(project) => {
                let mut handled = 0;
                let mut over_limit = 0;
                for (test, baseline) in candidates {
                    let Some(fp) = &test.fingerprint else {
                        outcome
                            .warnings
                            .push(format!("{}: no fingerprint, not reported", test.test_id));
                        continue;
                    };
                    if handled >= self.options.max_bugs {
                        over_limit += 1;
                        continue;
                    }
                    handled += 1;
                    let mut warnings = Vec::new();
                    match self
                        .report_one(project, test, fp, baseline, report, &mut warnings)
                        .await
                    {
                        Ok(bug) => outcome.bugs.push(bug),
                        Err(w) => outcome.warnings.push(w),
                    }
                    outcome.warnings.extend(warnings);
                }
                if over_limit > 0 {
                    outcome
                        .warnings
                        .push(format!("{over_limit} failures not reported: limit reached"));
                }
            }
        }
        if let Some(key) = &report.ticket_key {
            match self
                .tracker
                .comment_adf(key, &gate_summary(report, &outcome))
                .await
            {
                Ok(_) => outcome.ticket_comment = Some(key.clone()),
                Err(e) => outcome
                    .warnings
                    .push(format!("{key}: gate summary comment failed: {e}")),
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(status: TestStatus, baseline: Option<BaselineOutcome>) -> GateTest {
        GateTest {
            test_id: "p::f::t".into(),
            title: "t".into(),
            file: "f".into(),
            line: 1,
            project: "p".into(),
            tags: vec![],
            journey: Some("checkout.card".into()),
            status,
            blocking: true,
            attempts: 3,
            duration_ms: 1,
            error: Some("Timeout 5000ms exceeded for 9f8e7d6c5b4a".into()),
            baseline,
            fingerprint: None,
            evidence: None,
        }
    }

    #[test]
    fn without_a_pr_the_base_is_the_default_branch() {
        assert_eq!(base_branch_of(None), "default branch");
    }

    #[test]
    fn summary_uses_the_journey_and_the_normalized_error() {
        let t = test(TestStatus::Failed, Some(BaselineOutcome::Regression));
        assert_eq!(
            bug_summary(&t),
            "[guard] checkout.card: Timeout <n>ms exceeded for <id>"
        );
    }

    #[test]
    fn summary_is_cut_to_255_chars() {
        let mut t = test(TestStatus::Failed, None);
        t.error = Some("x".repeat(400));
        let s = bug_summary(&t);
        assert_eq!(s.chars().count(), 255);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn only_confirmed_failures_are_reportable() {
        use BaselineOutcome::*;
        assert_eq!(
            reportable(&test(TestStatus::Failed, Some(Regression))),
            Some(Regression)
        );
        assert_eq!(
            reportable(&test(TestStatus::Failed, Some(PreExisting))),
            Some(PreExisting)
        );
        assert_eq!(reportable(&test(TestStatus::Failed, Some(Flaky))), None);
        assert_eq!(
            reportable(&test(TestStatus::Failed, Some(Unavailable))),
            None
        );
        assert_eq!(reportable(&test(TestStatus::Failed, None)), None);
        assert_eq!(reportable(&test(TestStatus::Flaky, Some(Regression))), None);
    }

    #[test]
    fn short_never_splits_a_character() {
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
        // Byte 12 falls inside a multi-byte character: no panic.
        let odd = "0123456789aé-tail";
        assert_eq!(short(odd), odd);
    }

    #[test]
    fn the_bug_code_block_holds_only_the_first_error_line() {
        let mut t = test(TestStatus::Failed, Some(BaselineOutcome::Regression));
        t.error = Some(
            "Timeout 5000ms exceeded
    at checkout.spec.ts:14
    at x"
                .into(),
        );
        let report = GateReport {
            version: 2,
            run_id: "r".into(),
            generated_at: "x".into(),
            provider: None,
            repo: None,
            pr: None,
            ticket_key: None,
            ticket_is_flow_change: false,
            head_sha: "h".into(),
            base_sha: None,
            verdict: guard_core::ports::GateVerdict::Red,
            counts: Default::default(),
            tests: vec![t.clone()],
            journey_changes: vec![],
            change_flags: vec![],
            report_url: None,
            ci_url: None,
        };
        let text = bug_description(&t, BaselineOutcome::Regression, &report).to_string();
        assert!(text.contains("Timeout 5000ms exceeded"), "{text}");
        assert!(!text.contains("checkout.spec.ts:14"), "{text}");
    }

    #[test]
    fn adf_bodies_are_docs() {
        let t = test(TestStatus::Failed, Some(BaselineOutcome::Regression));
        let report = GateReport {
            version: 2,
            run_id: "r".into(),
            generated_at: "x".into(),
            provider: None,
            repo: None,
            pr: None,
            ticket_key: None,
            ticket_is_flow_change: false,
            head_sha: "0123456789abcdef".into(),
            base_sha: None,
            verdict: guard_core::ports::GateVerdict::Red,
            counts: Default::default(),
            tests: vec![t.clone()],
            journey_changes: vec![],
            change_flags: vec!["flag".into()],
            report_url: Some("https://r".into()),
            ci_url: None,
        };
        for body in [
            bug_description(&t, BaselineOutcome::Regression, &report),
            occurrence_comment(&t, &report),
            gate_summary(&report, &ReportOutcome::default()),
        ] {
            assert_eq!(body["type"], "doc");
            assert_eq!(body["version"], 1);
            assert!(!body["content"].as_array().unwrap().is_empty());
        }
        let text = bug_description(&t, BaselineOutcome::Regression, &report).to_string();
        assert!(text.contains("commit 0123456789ab"));
        assert!(text.contains("Report https://r"));
    }
}
