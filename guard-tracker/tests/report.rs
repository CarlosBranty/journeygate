//! The gate reporting use case and its ADF layouts (reporting.md).

use guard_core::fingerprint::Fingerprint;
use guard_core::ports::fakes::InMemoryTracker;
use guard_core::ports::*;
use guard_tracker::report::{
    MAX_BUGS_PER_RUN, ReportOptions, ScreenshotSource, bug_description_adf, bug_summary,
    gate_summary_adf, occurrence_comment_adf, report_gate,
};
use serde_json::{Value, json};
use std::sync::Mutex;

fn golden(name: &str) -> Value {
    let path = format!(
        "{}/tests/fixtures/golden/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn file(kind: EvidenceKind, name: &str, ct: &str) -> EvidenceFile {
    EvidenceFile {
        kind,
        name: name.into(),
        content_type: ct.into(),
        key: Some(format!("runs/12345-1/{name}")),
        url: Some(format!("https://minio.acme.test/{name}")),
        viewer_url: (kind == EvidenceKind::Trace)
            .then(|| "https://trace.playwright.dev/?trace=https%3A%2F%2Fminio".to_string()),
    }
}

fn attempt(retry: u32, ms: u64) -> AttemptSummary {
    AttemptSummary {
        retry,
        status: "failed".into(),
        duration_ms: ms,
        error: Some("Timeout 5000ms exceeded waiting for order 9f8e7d6c5b4a".into()),
    }
}

fn regression() -> GateTest {
    let fp = Fingerprint("1a2b3c4d".into());
    let test_id = "chromium::tests/journeys/checkout/customer-card.spec.ts::Checkout > pays";
    GateTest {
        test_id: test_id.into(),
        title: "Checkout > pays".into(),
        file: "tests/journeys/checkout/customer-card.spec.ts".into(),
        line: 14,
        project: "chromium".into(),
        tags: vec!["@smoke".into()],
        journey: Some("checkout.customer-card".into()),
        status: TestStatus::Failed,
        blocking: true,
        attempts: 3,
        duration_ms: 3300,
        error: Some(
            "\u{1b}[31mTimeout 5000ms exceeded waiting for order 9f8e7d6c5b4a\u{1b}[0m\n  at x"
                .into(),
        ),
        baseline: Some(BaselineOutcome::Regression),
        fingerprint: Some(fp.clone()),
        evidence: Some(EvidenceBundle {
            test_id: test_id.into(),
            fingerprint: fp,
            title: "Checkout > pays".into(),
            journey: Some("checkout.customer-card".into()),
            location: Some("tests/journeys/checkout/customer-card.spec.ts:21".into()),
            error: "Timeout 5000ms exceeded waiting for order 9f8e7d6c5b4a".into(),
            step: Some("click Pay".into()),
            seed: Some("42".into()),
            commit: "abcdef1234567890abcdef".into(),
            attempts: vec![attempt(0, 1000), attempt(1, 1100), attempt(2, 1200)],
            baseline: Some(BaselineOutcome::Regression),
            files: vec![
                file(EvidenceKind::Screenshot, "shot-1.png", "image/png"),
                file(EvidenceKind::Trace, "trace.zip", "application/zip"),
                file(EvidenceKind::Video, "video.webm", "video/webm"),
                file(EvidenceKind::Screenshot, "shot-2.png", "image/png"),
                file(EvidenceKind::Screenshot, "shot-3.png", "image/png"),
                file(EvidenceKind::Screenshot, "shot-4.png", "image/png"),
            ],
        }),
    }
}

fn pre_existing() -> GateTest {
    GateTest {
        test_id: "chromium::tests/journeys/catalog/stock.spec.ts::Catalog > badge".into(),
        title: "Catalog > badge".into(),
        file: "tests/journeys/catalog/stock.spec.ts".into(),
        line: 9,
        project: "chromium".into(),
        tags: vec![],
        journey: None,
        status: TestStatus::Failed,
        blocking: true,
        attempts: 3,
        duration_ms: 900,
        error: Some("expected 'Sold out' got 'In stock'".into()),
        baseline: Some(BaselineOutcome::PreExisting),
        fingerprint: Some(Fingerprint("5e6f7a8b".into())),
        evidence: None,
    }
}

fn other(title: &str, status: TestStatus, baseline: Option<BaselineOutcome>) -> GateTest {
    GateTest {
        test_id: format!("chromium::tests/x.spec.ts::{title}"),
        title: title.into(),
        file: "tests/x.spec.ts".into(),
        line: 3,
        project: "chromium".into(),
        tags: vec![],
        journey: None,
        status,
        blocking: false,
        attempts: 2,
        duration_ms: 10,
        error: Some("boom".into()),
        baseline,
        fingerprint: Some(Fingerprint("99999999".into())),
        evidence: None,
    }
}

fn report(tests: Vec<GateTest>) -> GateReport {
    GateReport {
        version: 2,
        run_id: "12345-1".into(),
        generated_at: "2026-10-30T12:00:00Z".into(),
        provider: Some(CodeHostProvider::Github),
        repo: Some("acme/shop".into()),
        pr: Some(PrContext {
            provider: CodeHostProvider::Github,
            repo: "acme/shop".into(),
            number: 12,
            title: "SHOP-123 pay by card".into(),
            url: "https://github.com/acme/shop/pull/12".into(),
            author: "dev".into(),
            ticket_key: Some("SHOP-123".into()),
            branch: "SHOP-123-pay".into(),
            base_branch: "main".into(),
            base_sha: "0123456789abcdef0123".into(),
            head_sha: "abcdef1234567890abcdef".into(),
            changed_files: vec![],
        }),
        ticket_key: Some("SHOP-123".into()),
        ticket_is_flow_change: false,
        head_sha: "abcdef1234567890abcdef".into(),
        base_sha: Some("0123456789abcdef0123".into()),
        verdict: GateVerdict::Red,
        counts: GateCounts {
            passed: 40,
            flaky: 2,
            failed: 3,
            skipped: 0,
            regressions: 1,
            pre_existing: 1,
        },
        tests,
        journey_changes: vec![JourneyChangeReport {
            journey: "checkout.customer-card".into(),
            file: "tests/journeys/checkout/customer-card.spec.ts".into(),
            flags: vec!["assertions 5 -> 3".into()],
        }],
        change_flags: vec!["journeys modified, but the ticket is not labeled flow-change".into()],
        report_url: Some("https://minio.acme.test/report/index.html".into()),
        ci_url: Some("https://github.com/acme/shop/actions/runs/1".into()),
    }
}

fn full_report() -> GateReport {
    report(vec![
        regression(),
        pre_existing(),
        other(
            "Search > unconfirmed",
            TestStatus::Failed,
            Some(BaselineOutcome::Unavailable),
        ),
        other("Cart > flaky", TestStatus::Flaky, None),
        other("Home > ok", TestStatus::Passed, None),
    ])
}

/// Serves `b"PNG:<name>"` for every screenshot, except names listed as missing or huge.
#[derive(Default)]
struct Shots {
    missing: Vec<&'static str>,
    huge: Vec<&'static str>,
    reads: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ScreenshotSource for Shots {
    async fn read(&self, file: &EvidenceFile) -> Option<Vec<u8>> {
        self.reads.lock().unwrap().push(file.name.clone());
        if self.missing.contains(&file.name.as_str()) {
            return None;
        }
        if self.huge.contains(&file.name.as_str()) {
            return Some(vec![0; 10 * 1024 * 1024 + 1]);
        }
        Some(format!("PNG:{}", file.name).into_bytes())
    }
}

fn seeded_tracker() -> InMemoryTracker {
    let t = InMemoryTracker::new("SHOP");
    t.insert_issue(issue("SHOP-123", "Story", StatusCategory::InProgress, &[]));
    t
}

fn issue(key: &str, ty: &str, cat: StatusCategory, labels: &[&str]) -> Issue {
    Issue {
        key: key.into(),
        url: format!("memory://jira/browse/{key}"),
        summary: "s".into(),
        description: String::new(),
        issue_type: ty.into(),
        status: "x".into(),
        status_category: cat,
        labels: labels.iter().map(|s| s.to_string()).collect(),
    }
}

// ------------------------------------------------------------ pure layouts

#[test]
fn bug_summary_uses_journey_and_normalized_error() {
    assert_eq!(
        bug_summary(&regression()),
        "[guard] checkout.customer-card: Timeout <n>ms exceeded waiting for order <id>"
    );
    assert_eq!(
        bug_summary(&pre_existing()),
        "[guard] Catalog > badge: expected 'Sold out' got 'In stock'"
    );
}

#[test]
fn bug_summary_is_cut_to_255_characters_with_an_ellipsis() {
    let mut t = pre_existing();
    t.error = Some("é".repeat(400));
    let s = bug_summary(&t);
    assert_eq!(s.chars().count(), 255);
    assert!(s.ends_with('…'));
    assert!(s.starts_with("[guard] Catalog > badge: éé"));
}

#[test]
fn bug_description_of_a_regression_matches_golden() {
    let t = regression();
    let r = full_report();
    assert_eq!(
        bug_description_adf(&t, t.evidence.as_ref(), &r),
        golden("bug_description_regression.json")
    );
}

#[test]
fn bug_description_of_a_pre_existing_failure_without_evidence() {
    let t = pre_existing();
    let mut r = full_report();
    r.pr = None;
    r.report_url = None;
    r.ci_url = None;
    assert_eq!(
        bug_description_adf(&t, None, &r),
        json!({"type": "doc", "version": 1, "content": [
            {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "Already broken on default branch"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "Test", "marks": [{"type": "strong"}]},
                {"type": "text", "text": " "},
                {"type": "text", "text": "chromium::tests/journeys/catalog/stock.spec.ts::Catalog > badge", "marks": [{"type": "code"}]}
            ]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "Location", "marks": [{"type": "strong"}]},
                {"type": "text", "text": " "},
                {"type": "text", "text": "tests/journeys/catalog/stock.spec.ts:9", "marks": [{"type": "code"}]}
            ]},
            {"type": "codeBlock", "content": [{"type": "text", "text": "expected 'Sold out' got 'In stock'"}]},
            {"type": "bulletList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "base commit 0123456789ab: fails 3/3 (pre-existing)"}]}]}
            ]},
            {"type": "paragraph", "content": [{"type": "text", "text": "commit abcdef123456 · run 12345-1"}]}
        ]})
    );
}

#[test]
fn occurrence_comment_layout() {
    let t = regression();
    let mut r = full_report();
    r.report_url = None;
    r.ci_url = None;
    let mut bundle = t.evidence.clone().unwrap();
    bundle.files.clear();
    assert_eq!(
        occurrence_comment_adf(&t, Some(&bundle), &r),
        json!({"type": "doc", "version": 1, "content": [
            {"type": "paragraph", "content": [
                {"type": "text", "text": "New occurrence", "marks": [{"type": "strong"}]},
                {"type": "text", "text": " in run "},
                {"type": "text", "text": "12345-1", "marks": [{"type": "code"}]},
                {"type": "text", "text": " on commit "},
                {"type": "text", "text": "abcdef123456", "marks": [{"type": "code"}]},
                {"type": "text", "text": " ("},
                {"type": "text", "text": "PR #12", "marks": [{"type": "link", "attrs": {"href": "https://github.com/acme/shop/pull/12"}}]},
                {"type": "text", "text": ")"}
            ]},
            {"type": "codeBlock", "content": [{"type": "text", "text": "Timeout 5000ms exceeded waiting for order 9f8e7d6c5b4a"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "Links", "marks": [{"type": "strong"}]},
                {"type": "text", "text": " "},
                {"type": "text", "text": "PR", "marks": [{"type": "link", "attrs": {"href": "https://github.com/acme/shop/pull/12"}}]}
            ]}
        ]})
    );
}

#[test]
fn gate_summary_matches_golden() {
    let r = full_report();
    let outcome = ReportOutcome {
        bugs: vec![
            ReportedBug {
                fingerprint: Fingerprint("1a2b3c4d".into()),
                key: "SHOP-211".into(),
                url: "https://acme.atlassian.net/browse/SHOP-211".into(),
                action: BugAction::Created,
            },
            ReportedBug {
                fingerprint: Fingerprint("5e6f7a8b".into()),
                key: "SHOP-150".into(),
                url: "https://acme.atlassian.net/browse/SHOP-150".into(),
                action: BugAction::Commented,
            },
        ],
        ..ReportOutcome::default()
    };
    assert_eq!(
        gate_summary_adf(&r, &outcome),
        golden("gate_summary_red.json")
    );
}

#[test]
fn green_gate_summary_has_only_heading_counts_and_links() {
    let mut r = report(vec![other("Home > ok", TestStatus::Passed, None)]);
    r.verdict = GateVerdict::Green;
    r.counts = GateCounts {
        passed: 43,
        ..GateCounts::default()
    };
    r.journey_changes.clear();
    r.change_flags.clear();
    r.pr = None;
    r.ci_url = None;
    assert_eq!(
        gate_summary_adf(&r, &ReportOutcome::default()),
        json!({"type": "doc", "version": 1, "content": [
            {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "guard: green"}]},
            {"type": "paragraph", "content": [{"type": "text", "text": "passed 43, flaky 0, failed 0 (regressions 0, pre-existing 0), skipped 0"}]},
            {"type": "paragraph", "content": [
                {"type": "text", "text": "Links", "marks": [{"type": "strong"}]},
                {"type": "text", "text": " "},
                {"type": "text", "text": "Report", "marks": [{"type": "link", "attrs": {"href": "https://minio.acme.test/report/index.html"}}]}
            ]}
        ]})
    );
}

// ------------------------------------------------------------ report_gate

#[tokio::test]
async fn new_failures_create_bugs_attach_screenshots_and_comment_the_ticket() {
    let tracker = seeded_tracker();
    let shots = Shots::default();
    let r = full_report();
    let outcome = report_gate(&tracker, &shots, &r, &ReportOptions::new("SHOP")).await;

    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert_eq!(outcome.bugs.len(), 2);
    assert_eq!(outcome.bugs[0].key, "SHOP-1001");
    assert_eq!(outcome.bugs[0].action, BugAction::Created);
    assert_eq!(outcome.bugs[0].fingerprint, Fingerprint("1a2b3c4d".into()));
    assert_eq!(outcome.bugs[1].key, "SHOP-1002");
    assert_eq!(outcome.ticket_comment.as_deref(), Some("SHOP-123"));
    assert!(!outcome.pr_comment);
    assert!(!outcome.status_published);

    let created = tracker.created_bugs();
    assert_eq!(created[0].project, "SHOP");
    assert_eq!(
        created[0].labels,
        vec!["guard", "guard-regression", "guard-fp-1a2b3c4d"]
    );
    assert_eq!(created[0].priority, None);
    assert_eq!(created[0].linked_issue.as_deref(), Some("SHOP-123"));
    assert_eq!(created[0].summary, bug_summary(&regression()));
    assert_eq!(
        created[1].labels,
        vec!["guard", "guard-pre-existing", "guard-fp-5e6f7a8b"]
    );
    assert_eq!(created[1].priority.as_deref(), Some("Highest"));
    assert_eq!(created[1].linked_issue, None);

    // Only the first 3 PNG screenshots; traces and videos are never attached.
    let att = tracker.attachments("SHOP-1001");
    let names: Vec<_> = att.iter().map(|a| a.file_name.as_str()).collect();
    assert_eq!(names, vec!["shot-1.png", "shot-2.png", "shot-3.png"]);
    assert_eq!(att[0].content_type, "image/png");
    assert_eq!(att[0].bytes, b"PNG:shot-1.png");

    let comments = tracker.comments("SHOP-123");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0], gate_summary_adf(&r, &outcome));
}

#[tokio::test]
async fn an_open_bug_with_the_fingerprint_gets_an_occurrence_comment() {
    let tracker = seeded_tracker();
    tracker.insert_issue(issue(
        "SHOP-150",
        "Bug",
        StatusCategory::Done,
        &["guard-fp-1a2b3c4d"],
    ));
    tracker.insert_issue(issue(
        "SHOP-210",
        "Bug",
        StatusCategory::InProgress,
        &["guard-fp-1a2b3c4d"],
    ));
    let r = report(vec![regression()]);
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;

    assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    assert!(tracker.created_bugs().is_empty());
    assert_eq!(outcome.bugs.len(), 1);
    assert_eq!(outcome.bugs[0].key, "SHOP-210");
    assert_eq!(outcome.bugs[0].action, BugAction::Commented);
    let t = regression();
    assert_eq!(
        tracker.comments("SHOP-210"),
        vec![occurrence_comment_adf(&t, t.evidence.as_ref(), &r)]
    );
    assert_eq!(tracker.attachments("SHOP-210").len(), 3);
}

#[tokio::test]
async fn only_a_closed_bug_creates_a_new_one_mentioning_it() {
    let tracker = seeded_tracker();
    tracker.insert_issue(issue(
        "SHOP-150",
        "Bug",
        StatusCategory::Done,
        &["guard-fp-1a2b3c4d"],
    ));
    let r = report(vec![regression()]);
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;
    assert_eq!(outcome.bugs[0].action, BugAction::Created);
    let description = tracker.created_bugs()[0].description_adf.to_string();
    assert!(
        description.contains("Previously reported as SHOP-150 (closed)."),
        "{description}"
    );
}

#[tokio::test]
async fn at_most_ten_bugs_per_run_and_the_rest_are_listed_as_limit_reached() {
    let tracker = seeded_tracker();
    let tests: Vec<GateTest> = (0..12)
        .map(|i| {
            let mut t = pre_existing();
            t.test_id = format!("chromium::a.spec.ts::t{i}");
            t.title = format!("t{i}");
            t.fingerprint = Some(Fingerprint(format!("{i:08x}")));
            t
        })
        .collect();
    let r = report(tests);
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;
    assert_eq!(MAX_BUGS_PER_RUN, 10);
    assert_eq!(outcome.bugs.len(), 10);
    assert_eq!(tracker.created_bugs().len(), 10);
    let summary = tracker.comments("SHOP-123")[0].to_string();
    assert_eq!(summary.matches("not reported: limit reached").count(), 2);
}

#[tokio::test]
async fn duplicate_fingerprints_in_one_run_report_once() {
    let tracker = seeded_tracker();
    let mut twin = regression();
    twin.project = "firefox".into();
    let r = report(vec![regression(), twin]);
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;
    assert_eq!(tracker.created_bugs().len(), 1);
    assert_eq!(outcome.bugs.len(), 1);
}

#[tokio::test]
async fn missing_or_oversized_screenshots_are_warnings() {
    let tracker = seeded_tracker();
    let shots = Shots {
        missing: vec!["shot-1.png"],
        huge: vec!["shot-2.png"],
        ..Shots::default()
    };
    let r = report(vec![regression()]);
    let outcome = report_gate(&tracker, &shots, &r, &ReportOptions::new("SHOP")).await;
    let names: Vec<_> = tracker
        .attachments("SHOP-1001")
        .into_iter()
        .map(|a| a.file_name)
        .collect();
    assert_eq!(names, vec!["shot-3.png"]);
    assert_eq!(outcome.warnings.len(), 2, "{:?}", outcome.warnings);
    assert!(outcome.warnings.iter().any(|w| w.contains("shot-1.png")));
    assert!(outcome.warnings.iter().any(|w| w.contains("shot-2.png")));
}

#[tokio::test]
async fn no_ticket_key_means_no_jira_summary_comment() {
    let tracker = seeded_tracker();
    let mut r = report(vec![other("Home > ok", TestStatus::Passed, None)]);
    r.ticket_key = None;
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;
    assert_eq!(outcome.ticket_comment, None);
    assert!(outcome.bugs.is_empty());
    assert!(tracker.comments("SHOP-123").is_empty());
}

#[tokio::test]
async fn unreportable_tests_never_reach_jira() {
    let tracker = seeded_tracker();
    let mut no_fp = regression();
    no_fp.fingerprint = None;
    let r = report(vec![
        other("a", TestStatus::Failed, Some(BaselineOutcome::Unavailable)),
        other("b", TestStatus::Failed, None),
        other("c", TestStatus::Failed, Some(BaselineOutcome::Flaky)),
        other("d", TestStatus::Flaky, Some(BaselineOutcome::Regression)),
        other("e", TestStatus::Skipped, None),
        no_fp,
    ]);
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;
    assert!(tracker.created_bugs().is_empty());
    assert!(outcome.bugs.is_empty());
    // A reportable failure without a fingerprint is a warning, not a guess.
    assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
}

/// A tracker whose every call fails with the given error.
struct Broken(TrackerError);

#[async_trait::async_trait]
impl IssueTracker for Broken {
    async fn find_by_label(&self, _: &str) -> Result<Vec<IssueRef>, TrackerError> {
        Err(self.0.clone())
    }
    async fn create_bug(&self, _: &NewBug) -> Result<IssueRef, TrackerError> {
        Err(self.0.clone())
    }
    async fn comment_adf(&self, _: &str, _: &Value) -> Result<CommentRef, TrackerError> {
        Err(self.0.clone())
    }
    async fn attach_file(&self, _: &str, _: &Attachment) -> Result<AttachmentRef, TrackerError> {
        Err(self.0.clone())
    }
    async fn add_labels(&self, _: &str, _: &[String]) -> Result<(), TrackerError> {
        Err(self.0.clone())
    }
    async fn suggest_labels(&self, _: &str, _: &[String]) -> Result<(), TrackerError> {
        Err(self.0.clone())
    }
    async fn get_issue(&self, _: &str) -> Result<Issue, TrackerError> {
        Err(self.0.clone())
    }
    fn parse_webhook(&self, _: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        Err(TrackerWebhookError::Unauthorized)
    }
}

#[tokio::test]
async fn tracker_failures_become_warnings_and_never_panic() {
    let r = full_report();
    let outcome = report_gate(
        &Broken(TrackerError::RateLimited),
        &Shots::default(),
        &r,
        &ReportOptions::new("SHOP"),
    )
    .await;
    assert!(outcome.bugs.is_empty());
    assert_eq!(outcome.ticket_comment, None);
    // Two reportable tests + the ticket comment.
    assert_eq!(outcome.warnings.len(), 3, "{:?}", outcome.warnings);
    assert!(outcome.warnings[0].contains("rate limited"));
}

#[test]
fn report_options_defaults() {
    let o = ReportOptions::new("SHOP");
    assert_eq!(o.project, "SHOP");
    assert_eq!(o.pre_existing_priority, "Highest");
}

/// Fails every call with a transport error and counts the calls.
#[derive(Default)]
struct Unreachable(std::sync::atomic::AtomicUsize);

impl Unreachable {
    fn fail<T>(&self) -> Result<T, TrackerError> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(TrackerError::Transport("connect timed out".into()))
    }
}

#[async_trait::async_trait]
impl IssueTracker for Unreachable {
    async fn find_by_label(&self, _: &str) -> Result<Vec<IssueRef>, TrackerError> {
        self.fail()
    }
    async fn create_bug(&self, _: &NewBug) -> Result<IssueRef, TrackerError> {
        self.fail()
    }
    async fn comment_adf(&self, _: &str, _: &Value) -> Result<CommentRef, TrackerError> {
        self.fail()
    }
    async fn attach_file(&self, _: &str, _: &Attachment) -> Result<AttachmentRef, TrackerError> {
        self.fail()
    }
    async fn add_labels(&self, _: &str, _: &[String]) -> Result<(), TrackerError> {
        self.fail()
    }
    async fn suggest_labels(&self, _: &str, _: &[String]) -> Result<(), TrackerError> {
        self.fail()
    }
    async fn get_issue(&self, _: &str) -> Result<Issue, TrackerError> {
        self.fail()
    }
    fn parse_webhook(&self, _: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        Err(TrackerWebhookError::Unauthorized)
    }
}

#[tokio::test]
async fn the_first_transport_error_stops_every_later_jira_call() {
    let tracker = Unreachable::default();
    let r = full_report();
    let outcome = report_gate(&tracker, &Shots::default(), &r, &ReportOptions::new("SHOP")).await;
    assert_eq!(tracker.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(outcome.bugs.is_empty());
    assert_eq!(outcome.ticket_comment, None);
    assert!(
        outcome
            .warnings
            .iter()
            .any(|w| w.contains("Jira unreachable")),
        "{:?}",
        outcome.warnings
    );
}
