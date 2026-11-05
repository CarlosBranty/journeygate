//! Triage use-case helpers: building the Jev `state` from a Jira issue and
//! turning a decision into Jira labels.
//!
//! The engine call itself is behind `ports::DecisionEngine`; the routing rule is
//! `gatekeeper::decide`. Server flow (`docs/contracts/server-api.md`):
//!
//! ```text
//! issue    = tracker.get_issue(key)
//! modules  = mentioned_modules(issue)            -> impact.resolve_mentions()
//! facts    = impact.fan_out(resolved paths)
//! state    = build_state(issue, modules, facts.blast_radius())
//! decided  = engine.triage(..).map(decide).unwrap_or(decide_without_engine)
//! log.record_decision(..)                        (always, also in shadow mode)
//! applied  = effective_lane(decided, mode)
//! labels   = jira_labels(answers, decided)       -> add_labels (active) / suggest_labels (shadow)
//! ```

use crate::ports::types::{
    Area, BlastRadius, HumanLabel, HumanReason, Issue, Kind, LabelSource, Lane, LaneKind,
    TriageAnswers, TriageState,
};
use crate::thresholds::Mode;
use regex::Regex;
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// Most acceptance criteria kept in the state; the rest are dropped.
pub const MAX_ACCEPTANCE_CRITERIA: usize = 30;
/// Most characters of the description kept in the state (after stripping).
pub const MAX_DESCRIPTION_CHARS: usize = 8_000;

const FLOW_CHANGE_LABEL: &str = "flow-change";

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

// HTML comments, including an unterminated one (dropped to the end of the text).
static HTML_COMMENT: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<!--.*?(?:-->|\z)"));
// A tag needs a letter right after `<` or `</`, so `a < b > c` survives.
static HTML_TAG: LazyLock<Regex> =
    LazyLock::new(|| re(r"</?[A-Za-z][A-Za-z0-9-]*(?:\s[^<>]*)?/?>"));
static JIRA_MACRO: LazyLock<Regex> =
    LazyLock::new(|| re(r"\{(?:code|noformat|quote|panel|color)(?::[^}\n]*)?\}"));
static JIRA_HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^([ \t]*)h[1-6]\.[ \t]+"));
static MD_HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^([ \t]*)#{1,6}[ \t]+"));
static MD_IMAGE_OR_LINK: LazyLock<Regex> = LazyLock::new(|| re(r"!?\[([^\]\n]*)\]\([^)\n]*\)"));
static JIRA_LINK: LazyLock<Regex> = LazyLock::new(|| re(r"\[([^\]|\n]+)\|[^\]\n]+\]"));
static BOLD_STARS: LazyLock<Regex> = LazyLock::new(|| re(r"\*\*([^*\n]+)\*\*"));
static STRIKE: LazyLock<Regex> = LazyLock::new(|| re(r"~~([^~\n]+)~~"));
static ITALIC_STAR: LazyLock<Regex> = LazyLock::new(|| re(r"\*([^*\s][^*\n]*)\*"));
// Underscore emphasis only at word edges, so `snake_case_names` survive.
static BOLD_UNDERSCORES: LazyLock<Regex> = LazyLock::new(|| re(r"(^|[^\w])__([^_\n]+)__([^\w]|$)"));
static ITALIC_UNDERSCORE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(^|[^\w])_([^_\s][^_\n]*)_([^\w]|$)"));

static AC_HEADING: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^\s*(?:acceptance criteria|ac)\s*:?\s*$"));
static ANY_HEADING: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:#{1,6}|h[1-6]\.)\s"));
static HEADING_MARKER: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:#{1,6}|h[1-6]\.)\s*"));
static LIST_ITEM: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:[-*+]|\d+[.)])(?:\s+(.*))?$"));
static CHECKBOX: LazyLock<Regex> = LazyLock::new(|| re(r"^\[[ xX]\]\s*"));

static MENTION_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"`([^`\n]+)`|[^\s`]+"));

/// Removes markup the classifier must not see: HTML tags, HTML comments
/// (`<!-- ... -->`, a prompt-injection vector), Jira wiki markup (`{code}`,
/// `{noformat}`, `h1.`), Markdown emphasis and link targets, and collapses
/// blank-line runs. Plain text in, plain text out.
pub fn strip_markup(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = HTML_COMMENT.replace_all(&text, "");
    let text = HTML_TAG.replace_all(&text, "");
    let text = JIRA_MACRO.replace_all(&text, "");
    let text = JIRA_HEADING.replace_all(&text, "$1");
    let text = MD_HEADING.replace_all(&text, "$1");
    let text = MD_IMAGE_OR_LINK.replace_all(&text, "$1");
    let text = JIRA_LINK.replace_all(&text, "$1");
    let text = BOLD_STARS.replace_all(&text, "$1");
    let text = STRIKE.replace_all(&text, "$1");
    let text = ITALIC_STAR.replace_all(&text, "$1");
    let text = BOLD_UNDERSCORES.replace_all(&text, "$1$2$3");
    let text = ITALIC_UNDERSCORE.replace_all(&text, "$1$2$3");
    collapse_blank_lines(&text)
}

/// Trims line ends, keeps at most one blank line in a row, trims the result.
fn collapse_blank_lines(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() && out.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// The acceptance-criteria section: line range `start..end` (heading included)
/// and its items. `None` when there is no heading followed by a list.
struct CriteriaSection {
    start: usize,
    end: usize,
    items: Vec<String>,
}

fn is_criteria_heading(line: &str) -> bool {
    let text = HEADING_MARKER.replace(line, "");
    let text = text.trim();
    let text = ["**", "__"]
        .iter()
        .find_map(|m| text.strip_prefix(m).and_then(|t| t.strip_suffix(m)))
        .unwrap_or(text);
    AC_HEADING.is_match(text)
}

fn find_criteria_section(lines: &[&str]) -> Option<CriteriaSection> {
    let start = lines.iter().position(|l| is_criteria_heading(l))?;
    let mut items: Vec<String> = Vec::new();
    let mut started = false;
    let mut end = start + 1;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            if started {
                break;
            }
            continue;
        }
        if ANY_HEADING.is_match(line) || is_criteria_heading(line) {
            break;
        }
        if let Some(caps) = LIST_ITEM.captures(line) {
            let body = caps.get(1).map_or("", |m| m.as_str()).trim();
            items.push(CHECKBOX.replace(body, "").trim().to_string());
            started = true;
        } else if started && line.starts_with([' ', '\t']) {
            if let Some(last) = items.last_mut() {
                let continuation = line.trim();
                if last.is_empty() {
                    last.push_str(continuation);
                } else {
                    last.push(' ');
                    last.push_str(continuation);
                }
            }
        } else {
            break;
        }
        end = i + 1;
    }
    if !started {
        return None;
    }
    items.retain(|item| !item.is_empty());
    Some(CriteriaSection { start, end, items })
}

/// Acceptance criteria from the issue description: the list items under the
/// first heading or line matching `(?i)^\s*(acceptance criteria|ac)\s*:?\s*$`
/// until the next heading or blank line after the list. Each item trimmed,
/// without its bullet (`-`, `*`, `1.`, `[ ]`). Empty when there is no such section.
///
/// Heading markers (`##`, `h3.`) and a `**bold**` wrapper around the heading
/// text are ignored when matching. An indented line inside the list continues
/// the previous item; any other non-list line ends the section.
pub fn extract_acceptance_criteria(description: &str) -> Vec<String> {
    let lines: Vec<&str> = description.lines().collect();
    find_criteria_section(&lines).map_or_else(Vec::new, |s| s.items)
}

/// Code module names mentioned in the issue: backticked tokens and tokens that
/// look like repo paths (contain `/` or a file extension). Deduplicated, in
/// order of first appearance.
///
/// The summary is scanned before the description. Backticked text containing
/// whitespace (a command, not a module) is ignored; URLs, e-mail addresses,
/// abbreviations (`e.g.`) and numbers are not modules.
pub fn mentioned_modules(issue: &Issue) -> Vec<String> {
    let text = format!("{}\n{}", issue.summary, issue.description);
    let mut seen = BTreeSet::new();
    let mut modules = Vec::new();
    for caps in MENTION_TOKEN.captures_iter(&text) {
        let candidate = match caps.get(1) {
            Some(code) => {
                let code = code.as_str();
                (!code.contains(char::is_whitespace)).then_some(code)
            }
            None => {
                let token = trim_token(&caps[0]);
                looks_like_path(token).then_some(token)
            }
        };
        if let Some(module) = candidate {
            if seen.insert(module.to_string()) {
                modules.push(module.to_string());
            }
        }
    }
    modules
}

fn trim_token(token: &str) -> &str {
    token
        .trim_start_matches(['(', '[', '{', '<', '"', '\'', '*'])
        .trim_end_matches([
            ')', ']', '}', '>', '"', '\'', ',', ';', ':', '!', '?', '.', '*',
        ])
}

fn looks_like_path(token: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '@');
    if token.is_empty()
        || !token.chars().all(allowed)
        || !token.chars().any(|c| c.is_ascii_alphabetic())
        || token.starts_with("www.")
        || token.char_indices().any(|(i, c)| c == '@' && i > 0)
    {
        return false;
    }
    if token.contains('/') {
        return true;
    }
    match token.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 10
                && ext.starts_with(|c: char| c.is_ascii_alphabetic())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                // `e.g`, `i.e`: a one-letter stem with a one-letter extension.
                && (stem.len() >= 2 || ext.len() >= 2)
        }
        None => false,
    }
}

/// The `state` sent to the engine. `description` is `strip_markup` of the issue
/// description with the acceptance-criteria section removed and cut to
/// [`MAX_DESCRIPTION_CHARS`]; criteria cut to [`MAX_ACCEPTANCE_CRITERIA`].
///
/// Each criterion also goes through `strip_markup`; criteria that end up empty
/// are dropped.
pub fn build_state(
    issue: &Issue,
    mentioned_modules: Vec<String>,
    blast_radius: BlastRadius,
) -> TriageState {
    let lines: Vec<&str> = issue.description.lines().collect();
    let (description, criteria) = match find_criteria_section(&lines) {
        Some(section) => {
            let remaining: Vec<&str> = lines[..section.start]
                .iter()
                .chain(&lines[section.end..])
                .copied()
                .collect();
            (remaining.join("\n"), section.items)
        }
        None => (issue.description.clone(), Vec::new()),
    };
    let description: String = strip_markup(&description)
        .chars()
        .take(MAX_DESCRIPTION_CHARS)
        .collect();
    let acceptance_criteria = criteria
        .iter()
        .map(|c| strip_markup(c))
        .filter(|c| !c.is_empty())
        .take(MAX_ACCEPTANCE_CRITERIA)
        .collect();
    TriageState {
        summary: strip_markup(&issue.summary),
        description,
        acceptance_criteria,
        mentioned_modules,
        blast_radius,
    }
}

/// The lane that is applied: in Shadow Mode always `Human([ShadowMode])`,
/// otherwise `decided` unchanged.
pub fn effective_lane(decided: &Lane, mode: Mode) -> Lane {
    if mode.is_shadow() {
        Lane::Human(vec![HumanReason::ShadowMode])
    } else {
        decided.clone()
    }
}

/// Jira labels for a decision, sorted: `area:<key>`, `type:<kind>`,
/// `lane:agent` or `lane:human`, plus `flow-change` when the decided lane has
/// `ChangesExistingFlow`. `answers == None` (engine failed) yields only the
/// lane label. In Shadow Mode the caller writes these with
/// `IssueTracker::suggest_labels` instead of `add_labels`.
pub fn jira_labels(answers: Option<&TriageAnswers>, decided: &Lane) -> Vec<String> {
    let lane = match decided {
        Lane::Agent => "lane:agent",
        Lane::Human(_) => "lane:human",
    };
    let mut labels = vec![lane.to_string()];
    if let Some(answers) = answers {
        labels.push(format!("area:{}", answers.area.value.as_str()));
        labels.push(format!("type:{}", answers.kind.value.as_str()));
        if matches!(decided, Lane::Human(r) if r.contains(&HumanReason::ChangesExistingFlow)) {
            labels.push(FLOW_CHANGE_LABEL.to_string());
        }
    }
    labels.sort();
    labels
}

/// The human label implied by an issue's labels (server-api.md, Jira
/// `labels_changed`): `area` from exactly one `area:<key>` label, `kind` from
/// exactly one `type:<kind>` label (`Kind::from_name`), `lane` from
/// `lane:agent` / `lane:human`, `flow_change = Some(true)` when `flow-change`
/// is present (else `None`). Zero or several labels of a family give `None`.
/// `labels` is stored sorted and deduplicated.
pub fn human_label_from_labels(
    ticket_key: &str,
    labels: &[String],
    labeled_at: &str,
    source: LabelSource,
) -> HumanLabel {
    let labels: Vec<String> = labels
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let only = |prefix: &str| -> Option<&str> {
        let mut values = labels.iter().filter_map(|l| l.strip_prefix(prefix));
        match (values.next(), values.next()) {
            (Some(value), None) => Some(value),
            _ => None,
        }
    };
    let area = only("area:").filter(|key| !key.is_empty()).map(Area::new);
    let kind = only("type:").and_then(Kind::from_name);
    let lane = only("lane:").and_then(|lane| match lane {
        "agent" => Some(LaneKind::Agent),
        "human" => Some(LaneKind::Human),
        _ => None,
    });
    let flow_change = labels
        .iter()
        .any(|l| l == FLOW_CHANGE_LABEL)
        .then_some(true);
    HumanLabel {
        ticket_key: ticket_key.to_string(),
        labeled_at: labeled_at.to_string(),
        area,
        kind,
        lane,
        flow_change,
        labels,
        source,
    }
}

#[cfg(test)]
mod tests;
