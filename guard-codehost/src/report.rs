//! PR comment and check status layouts (`docs/contracts/reporting.md`).
//!
//! Pure functions: `render_pr_comment` builds the Markdown that
//! `CodeHost::comment_pr` upserts, `gate_status` the `GateStatus` that
//! `CodeHost::publish_status` publishes.

use crate::PR_COMMENT_MARKER;
use crate::http::truncate_chars;
use guard_core::ports::{
    BaselineOutcome, CheckState, GateReport, GateStatus, GateTest, GateVerdict, ReportOutcome,
    TestStatus,
};
use guard_core::routing::SMOKE_TAG;

#[cfg(test)]
mod tests;

/// Rows shown in the PR comment table; the rest are counted.
pub const MAX_ROWS: usize = 30;
/// Name of the check run / commit status (branch protection depends on it).
pub const STATUS_NAME: &str = "guard";
/// GitHub `output.title` / GitLab `description` limit.
pub const MAX_TITLE_CHARS: usize = 140;

/// How a test appears in the comment table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Regression,
    Unconfirmed,
    PreExisting,
    FlakyBlockingSmoke,
    FlakyBlockingAffected,
    Flaky,
}

impl Row {
    fn of(test: &GateTest) -> Option<Row> {
        let unstable = || {
            if !test.blocking {
                Row::Flaky
            } else if test
                .tags
                .iter()
                .any(|t| t == SMOKE_TAG || t.trim_start_matches('@') == "smoke")
            {
                Row::FlakyBlockingSmoke
            } else {
                Row::FlakyBlockingAffected
            }
        };
        match test.status {
            TestStatus::Passed | TestStatus::Skipped => None,
            TestStatus::Flaky => Some(unstable()),
            TestStatus::Failed => Some(match test.baseline {
                Some(BaselineOutcome::Regression) => Row::Regression,
                Some(BaselineOutcome::PreExisting) => Row::PreExisting,
                Some(BaselineOutcome::Flaky) => unstable(),
                Some(BaselineOutcome::Unavailable) | None => Row::Unconfirmed,
            }),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Row::Regression => "❌ regression",
            Row::Unconfirmed => "❌ unconfirmed",
            Row::PreExisting => "🟡 pre-existing",
            Row::FlakyBlockingSmoke => "⚠️ flaky (blocking: smoke)",
            Row::FlakyBlockingAffected => "⚠️ flaky (blocking: affected)",
            Row::Flaky => "⚠️ flaky",
        }
    }

    /// 0 = blocking, 1 = pre-existing, 2 = the rest (non-blocking flaky).
    fn group(self, test: &GateTest) -> u8 {
        match self {
            _ if test.blocking => 0,
            Row::PreExisting => 1,
            _ => 2,
        }
    }
}

/// The rows of the table, in contract order (stable within each group).
fn rows(report: &GateReport) -> Vec<(Row, &GateTest)> {
    let mut rows: Vec<(Row, &GateTest)> = report
        .tests
        .iter()
        .filter_map(|t| Row::of(t).map(|r| (r, t)))
        .collect();
    rows.sort_by_key(|(row, test)| row.group(test));
    rows
}

fn verdict_word(verdict: GateVerdict) -> &'static str {
    match verdict {
        GateVerdict::Green => "green",
        GateVerdict::Red => "red",
    }
}

/// Escapes a table cell (`|` would end the cell, newlines the row).
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace(['\r', '\n'], " ")
}

fn code_cell(text: &str) -> String {
    format!("`{}`", cell(&text.replace('`', "'")))
}

fn location(test: &GateTest) -> String {
    test.evidence
        .as_ref()
        .and_then(|e| e.location.clone())
        .unwrap_or_else(|| format!("{}:{}", test.file, test.line))
}

fn jira_cell(test: &GateTest, outcome: &ReportOutcome) -> String {
    test.fingerprint
        .as_ref()
        .and_then(|fp| outcome.bugs.iter().find(|b| &b.fingerprint == fp))
        .map(|bug| format!("[{}]({})", cell(&bug.key), bug.url))
        .unwrap_or_else(|| "—".to_string())
}

/// The guard summary comment. The first line is [`PR_COMMENT_MARKER`].
pub fn render_pr_comment(report: &GateReport, outcome: &ReportOutcome) -> String {
    let c = &report.counts;
    let mut blocks: Vec<String> = vec![
        format!(
            "{PR_COMMENT_MARKER}\n### guard: {}",
            verdict_word(report.verdict)
        ),
        format!(
            "passed {} · flaky {} · failed {} (regressions {}, pre-existing {}) · skipped {}",
            c.passed, c.flaky, c.failed, c.regressions, c.pre_existing, c.skipped
        ),
    ];

    let rows = rows(report);
    if !rows.is_empty() {
        let mut table = String::from("| | Test | Location | Jira |\n| --- | --- | --- | --- |");
        for (row, test) in rows.iter().take(MAX_ROWS) {
            let name = test.journey.as_deref().unwrap_or(&test.title);
            table.push_str(&format!(
                "\n| {} | {} | {} | {} |",
                row.label(),
                code_cell(name),
                code_cell(&location(test)),
                jira_cell(test, outcome)
            ));
        }
        blocks.push(table);
        if rows.len() > MAX_ROWS {
            blocks.push(format!("… and {} more", rows.len() - MAX_ROWS));
        }
    }

    let mut modified: Vec<String> = report.change_flags.clone();
    for change in &report.journey_changes {
        modified.extend(
            change
                .flags
                .iter()
                .map(|flag| format!("{}: {flag}", change.journey)),
        );
    }
    if !modified.is_empty() {
        let mut block = String::from("**Modified journeys** (QA review via CODEOWNERS)");
        for item in modified {
            block.push_str(&format!("\n- {item}"));
        }
        blocks.push(block);
    }

    if rows.iter().any(|(row, _)| *row == Row::Regression) {
        blocks.push(
            "If the flow change is intentional, update the failing journey in this PR.".into(),
        );
    }

    let mut links: Vec<String> = Vec::new();
    if let Some(url) = &report.report_url {
        links.push(format!("[Report]({url})"));
    }
    if let Some(url) = &report.ci_url {
        links.push(format!("[Pipeline]({url})"));
    }
    links.push(format!("run `{}`", report.run_id));
    blocks.push(links.join(" · "));

    let mut out = blocks.join("\n\n");
    out.push('\n');
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn title(report: &GateReport) -> String {
    let c = &report.counts;
    let text = match report.verdict {
        GateVerdict::Green => {
            let mut parts = vec![format!("{} passed", c.passed)];
            if c.flaky > 0 {
                parts.push(format!("{} flaky", c.flaky));
            }
            format!("guard: green — {}", parts.join(", "))
        }
        GateVerdict::Red => {
            let rows = rows(report);
            let count = |pred: fn(Row) -> bool| {
                rows.iter()
                    .filter(|(row, test)| test.blocking && pred(*row))
                    .count()
            };
            let regressions = count(|r| r == Row::Regression);
            let unconfirmed = count(|r| r == Row::Unconfirmed);
            let flaky =
                count(|r| matches!(r, Row::FlakyBlockingSmoke | Row::FlakyBlockingAffected));
            let mut parts = Vec::new();
            if regressions > 0 {
                parts.push(plural(regressions, "regression", "regressions"));
            }
            if unconfirmed > 0 {
                parts.push(format!("{unconfirmed} unconfirmed"));
            }
            if flaky > 0 {
                parts.push(format!("{flaky} blocking flaky"));
            }
            if parts.is_empty() {
                "guard: red".to_string()
            } else {
                format!("guard: red — {}", parts.join(", "))
            }
        }
    };
    truncate_chars(&text, MAX_TITLE_CHARS)
}

/// The `guard` check run / commit status for `report.head_sha`.
/// `infrastructure_error` (finalize exit 3) wins over the verdict.
pub fn gate_status(report: &GateReport, infrastructure_error: Option<&str>) -> GateStatus {
    let details_url = report.ci_url.clone().or_else(|| report.report_url.clone());
    let (state, title, summary) = match infrastructure_error {
        Some(reason) => (
            CheckState::Error,
            "guard: infrastructure error".to_string(),
            format!(
                "### guard: infrastructure error\n\nThe verdict could not be computed: {reason}\n"
            ),
        ),
        None => {
            let comment = render_pr_comment(report, &ReportOutcome::default());
            let summary = comment
                .strip_prefix(PR_COMMENT_MARKER)
                .map(|s| s.strip_prefix('\n').unwrap_or(s))
                .unwrap_or(&comment)
                .to_string();
            let state = match report.verdict {
                GateVerdict::Green => CheckState::Success,
                GateVerdict::Red => CheckState::Failure,
            };
            (state, title(report), summary)
        }
    };
    GateStatus {
        head_sha: report.head_sha.clone(),
        name: STATUS_NAME.to_string(),
        state,
        title,
        summary,
        details_url,
    }
}
