//! `/webhooks/jira`: authentication and payload parsing.
//!
//! Auth (server-api.md): `Authorization: Bearer <GUARD_JIRA_WEBHOOK_SECRET>`
//! (Jira Automation "Send web request") or query `?secret=<...>` (Jira admin
//! webhooks, which cannot set headers). No configured secret fails closed.
//!
//! Payload: the Jira Cloud webhook shape (`webhookEvent`, `user`, `issue`,
//! `changelog`). `issue.fields.description` and the acceptance-criteria custom
//! field arrive as a plain/wiki string (admin webhooks) or as ADF (Automation
//! with `{{issue.fields.description.asJsonString}}`-style bodies); both are
//! accepted.

use crate::adf;
use guard_core::ports::{TrackerEvent, TrackerWebhookError, WebhookRequest};
use serde_json::Value;
use std::collections::BTreeSet;

/// The ticket as carried by a webhook delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraTicket {
    pub key: String,
    pub summary: String,
    /// Description flattened to plain text (`adf::to_plain_text` for ADF).
    pub description: String,
    /// From the configured acceptance-criteria field; empty when the field is
    /// not configured or empty (triage then extracts them from the description).
    pub acceptance_criteria: Vec<String>,
    pub labels: Vec<String>,
    pub issue_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JiraWebhookEvent {
    IssueCreated,
    IssueUpdated,
    /// Any other `webhookEvent` (`comment_created`, `jira:issue_deleted`, ...).
    Other(String),
}

/// A parsed delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraWebhook {
    pub event: JiraWebhookEvent,
    /// Present for issue events (`issue.key` is required for them).
    pub ticket: Option<JiraTicket>,
    /// `changelog.items[]` contains a `labels` change.
    pub labels_changed: bool,
    /// Labels after the change: `issue.fields.labels`, else the changelog
    /// `toString` (space-separated). Sorted, deduplicated.
    pub labels_after: Vec<String>,
    /// `labels_after` without the guard-managed label families (`area:`,
    /// `type:`, `lane:`, `flow-change`) that this change did not touch, per
    /// the changelog `fromString`/`toString` diff. In active mode guard writes
    /// those labels itself; one left untouched is not a human verdict.
    pub human_labels: Vec<String>,
    /// `user.accountId`: who triggered the event.
    pub actor_account_id: Option<String>,
}

/// Checks the shared secret. Comparison is constant-time in the secret length.
pub fn authenticate(
    request: &WebhookRequest,
    secret: Option<&str>,
) -> Result<(), TrackerWebhookError> {
    let secret = match secret {
        Some(s) if !s.is_empty() => s,
        _ => return Err(TrackerWebhookError::Unauthorized),
    };
    let from_header = request.header("authorization").and_then(|v| {
        let (scheme, token) = v.trim().split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("bearer")
            .then(|| token.trim().to_string())
    });
    let from_query = request.query.as_deref().and_then(|q| {
        form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "secret")
            .map(|(_, v)| v.into_owned())
    });
    let ok = [from_header, from_query]
        .into_iter()
        .flatten()
        .any(|candidate| constant_time_eq(candidate.as_bytes(), secret.as_bytes()));
    if ok {
        Ok(())
    } else {
        Err(TrackerWebhookError::Unauthorized)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Parses a delivery body. `acceptance_criteria_field` is the custom field id
/// to read criteria from (e.g. `customfield_10050`).
pub fn parse_payload(
    body: &[u8],
    acceptance_criteria_field: Option<&str>,
) -> Result<JiraWebhook, TrackerWebhookError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|e| TrackerWebhookError::Malformed(format!("invalid JSON: {e}")))?;
    let event_name = v["webhookEvent"]
        .as_str()
        .ok_or_else(|| TrackerWebhookError::Malformed("missing webhookEvent".into()))?;
    let event = match event_name {
        "jira:issue_created" => JiraWebhookEvent::IssueCreated,
        "jira:issue_updated" => JiraWebhookEvent::IssueUpdated,
        other => JiraWebhookEvent::Other(other.to_string()),
    };
    let is_issue_event = !matches!(event, JiraWebhookEvent::Other(_));

    let ticket = match v.get("issue") {
        Some(issue) => match issue["key"].as_str() {
            Some(key) if !key.is_empty() => {
                Some(ticket(key, &issue["fields"], acceptance_criteria_field))
            }
            _ if is_issue_event => {
                return Err(TrackerWebhookError::Malformed("missing issue.key".into()));
            }
            _ => None,
        },
        None if is_issue_event => {
            return Err(TrackerWebhookError::Malformed("missing issue".into()));
        }
        None => None,
    };

    let label_items: Vec<&Value> = v["changelog"]["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|i| i["field"] == "labels" || i["fieldId"] == "labels")
                .collect()
        })
        .unwrap_or_default();
    let labels_changed = !label_items.is_empty();
    let mut labels_after: Vec<String> = match v["issue"]["fields"]["labels"].as_array() {
        Some(labels) => labels
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        None => label_items
            .last()
            .and_then(|i| i["toString"].as_str())
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
    };
    labels_after.sort();
    labels_after.dedup();
    let words = |item: &Value, field: &str| -> BTreeSet<String> {
        item[field]
            .as_str()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    let mut touched: BTreeSet<&'static str> = BTreeSet::new();
    for item in &label_items {
        let (from, to) = (words(item, "fromString"), words(item, "toString"));
        for label in from.symmetric_difference(&to) {
            touched.extend(label_family(label));
        }
    }
    let human_labels = labels_after
        .iter()
        .filter(|l| label_family(l).is_none_or(|family| touched.contains(family)))
        .cloned()
        .collect();

    Ok(JiraWebhook {
        event,
        ticket,
        labels_changed,
        labels_after,
        human_labels,
        actor_account_id: v["user"]["accountId"].as_str().map(str::to_string),
    })
}

/// The guard-managed family of a label (see `guard_core::triage::jira_labels`).
fn label_family(label: &str) -> Option<&'static str> {
    ["area:", "type:", "lane:"]
        .into_iter()
        .find(|prefix| label.starts_with(prefix))
        .or((label == "flow-change").then_some("flow-change"))
}

fn ticket(key: &str, fields: &Value, ac_field: Option<&str>) -> JiraTicket {
    JiraTicket {
        key: key.to_string(),
        summary: fields["summary"].as_str().unwrap_or_default().to_string(),
        description: adf::to_plain_text(&fields["description"])
            .trim()
            .to_string(),
        acceptance_criteria: ac_field.map(|f| criteria(&fields[f])).unwrap_or_default(),
        labels: fields["labels"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        issue_type: fields["issuetype"]["name"].as_str().map(str::to_string),
    }
}

/// Acceptance criteria from a custom field value: an array of strings, a
/// string or an ADF document (one item per non-empty line, bullets removed).
fn criteria(value: &Value) -> Vec<String> {
    let lines: Vec<String> = match value {
        Value::Array(items) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| adf::to_plain_text(i))
            })
            .collect(),
        Value::Null => vec![],
        other => adf::to_plain_text(other)
            .lines()
            .map(str::to_string)
            .collect(),
    };
    lines
        .iter()
        .map(|l| strip_bullet(l))
        .filter(|l| !l.is_empty())
        .collect()
}

fn strip_bullet(line: &str) -> String {
    let mut s = line.trim();
    for prefix in ["[ ]", "[x]", "[X]", "- ", "* ", "# "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim_start();
        }
    }
    // `1.` / `1)` numbering.
    let digits = s.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let rest = &s[digits..];
        if let Some(r) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
            s = r.trim_start();
        }
    }
    // A bare `-` / `*` bullet glued to the text.
    if let Some(r) = s.strip_prefix(['-', '*']) {
        s = r.trim_start();
    }
    s.trim().to_string()
}

/// Maps a parsed delivery to the port event. `self_account_id` is guard's own
/// Jira account (`GET /rest/api/3/myself`): label changes it made are
/// ignored, and while it is unknown label changes are ignored too (fail
/// closed: guard's own writes must never pollute the calibration ground truth).
pub fn to_event(webhook: &JiraWebhook, self_account_id: Option<&str>) -> TrackerEvent {
    let key = webhook.ticket.as_ref().map(|t| t.key.clone());
    match (&webhook.event, key) {
        (JiraWebhookEvent::IssueCreated, Some(key)) => TrackerEvent::IssueCreated { key },
        (JiraWebhookEvent::IssueUpdated, Some(key)) => {
            if !webhook.labels_changed {
                return ignored("issue updated without a labels change");
            }
            match self_account_id {
                None => ignored("guard's own Jira account id is not loaded"),
                Some(own) if webhook.actor_account_id.as_deref() == Some(own) => {
                    ignored("label change made by guard's own Jira user")
                }
                Some(_) => TrackerEvent::LabelsChanged {
                    key,
                    labels: webhook.human_labels.clone(),
                },
            }
        }
        (JiraWebhookEvent::Other(name), _) => ignored(&format!("unsupported event {name}")),
        (_, None) => ignored("issue event without an issue key"),
    }
}

fn ignored(reason: &str) -> TrackerEvent {
    TrackerEvent::Ignored {
        reason: reason.to_string(),
    }
}
