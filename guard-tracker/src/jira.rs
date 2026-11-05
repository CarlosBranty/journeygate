//! `IssueTracker` for Jira Cloud REST v3.
//!
//! Requests and mappings: `docs/contracts/reporting.md`, "Jira calls".
//!
//! Search uses the enhanced JQL endpoint `POST /rest/api/3/search/jql`: the
//! legacy `GET/POST /rest/api/3/search` was deprecated by Atlassian
//! (CHANGE-2046) and removed from Jira Cloud. The enhanced endpoint returns
//! only issue ids unless `fields` is given, and paginates with
//! `nextPageToken`; one page of at most 10 results is all dedup needs.

use crate::adf;
use crate::config::JiraConfig;
use crate::webhook::{self, JiraWebhook};
use guard_core::ports::{
    Attachment, AttachmentRef, CommentRef, Issue, IssueRef, IssueTracker, NewBug, StatusCategory,
    TrackerError, TrackerEvent, TrackerWebhookError, WebhookRequest,
};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use secrecy::ExposeSecret;
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

/// Longest response body kept in `TrackerError::Rejected`.
const MAX_ERROR_BODY: usize = 2_000;
const TIMEOUT: Duration = Duration::from_secs(30);
/// A Jira that does not accept the connection fails fast.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The account behind a set of Jira credentials (`GET /rest/api/3/myself`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraAccount {
    pub account_id: String,
    pub display_name: String,
    /// Absent when the account hides its email.
    pub email: Option<String>,
}

pub struct JiraTracker {
    config: JiraConfig,
    http: reqwest::Client,
    self_account_id: OnceLock<String>,
}

impl JiraTracker {
    pub fn new(config: JiraConfig) -> Result<Self, TrackerError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .user_agent(concat!("guard-tracker/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        Ok(Self {
            config,
            http,
            self_account_id: OnceLock::new(),
        })
    }

    /// Sets guard's own Jira account id without calling `/myself`.
    pub fn with_self_account_id(self, account_id: impl Into<String>) -> Self {
        let _ = self.self_account_id.set(account_id.into());
        self
    }

    pub fn config(&self) -> &JiraConfig {
        &self.config
    }

    /// `<JIRA_BASE_URL>/browse/<key>`.
    pub fn issue_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.config.base_url)
    }

    /// guard's own account id (`GET /rest/api/3/myself`), fetched once and
    /// cached. The server calls this at startup so `parse_webhook` (sync) can
    /// ignore label changes made by guard itself.
    pub async fn load_self_account_id(&self) -> Result<String, TrackerError> {
        if let Some(id) = self.self_account_id.get() {
            return Ok(id.clone());
        }
        let v = self
            .json(self.request(Method::GET, "/rest/api/3/myself"), "myself")
            .await?;
        let id = v["accountId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| rejected_shape("myself response without accountId"))?
            .to_string();
        Ok(self.self_account_id.get_or_init(|| id).clone())
    }

    /// The account the credentials belong to (`GET /rest/api/3/myself`, not
    /// cached). `guard connect jira` uses it to validate the credentials.
    pub async fn myself(&self) -> Result<JiraAccount, TrackerError> {
        let v = self
            .json(self.request(Method::GET, "/rest/api/3/myself"), "myself")
            .await?;
        let text = |key: &str| {
            v[key]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let account_id =
            text("accountId").ok_or_else(|| rejected_shape("myself response without accountId"))?;
        Ok(JiraAccount {
            display_name: text("displayName").unwrap_or_else(|| account_id.clone()),
            email: text("emailAddress"),
            account_id,
        })
    }

    /// Authenticates and parses a Jira webhook with the full ticket (summary,
    /// description, acceptance criteria from `JIRA_ACCEPTANCE_CRITERIA_FIELD`).
    pub fn parse_ticket_webhook(
        &self,
        request: &WebhookRequest,
    ) -> Result<JiraWebhook, TrackerWebhookError> {
        let secret = self
            .config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose_secret());
        webhook::authenticate(request, secret)?;
        webhook::parse_payload(
            &request.body,
            self.config.acceptance_criteria_field.as_deref(),
        )
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.config.base_url))
            .basic_auth(
                &self.config.email,
                Some(self.config.api_token.expose_secret()),
            )
            .header("Accept", "application/json")
    }

    /// Sends and maps non-2xx statuses. `subject` names the issue for 404s.
    async fn send(&self, req: RequestBuilder, subject: &str) -> Result<Response, TrackerError> {
        let resp = req
            .send()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        Err(match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                TrackerError::Unauthorized(status.as_u16())
            }
            StatusCode::NOT_FOUND => TrackerError::NotFound(subject.to_string()),
            StatusCode::TOO_MANY_REQUESTS => TrackerError::RateLimited,
            _ => {
                let body = resp.text().await.unwrap_or_default();
                TrackerError::Rejected {
                    status: status.as_u16(),
                    body: truncate(&body),
                }
            }
        })
    }

    async fn json(&self, req: RequestBuilder, subject: &str) -> Result<Value, TrackerError> {
        let resp = self.send(req, subject).await?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| TrackerError::Transport(e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| TrackerError::Rejected {
            status,
            body: format!("invalid JSON response: {e}"),
        })
    }

    async fn put_issue(&self, key: &str, body: Value) -> Result<(), TrackerError> {
        check_key(key)?;
        self.send(
            self.request(Method::PUT, &format!("/rest/api/3/issue/{key}"))
                .json(&body),
            key,
        )
        .await
        .map(drop)
    }
}

fn truncate(body: &str) -> String {
    if body.chars().count() <= MAX_ERROR_BODY {
        body.to_string()
    } else {
        let mut s: String = body.chars().take(MAX_ERROR_BODY).collect();
        s.push('…');
        s
    }
}

fn rejected_shape(msg: &str) -> TrackerError {
    TrackerError::Rejected {
        status: 0,
        body: msg.to_string(),
    }
}

/// Issue keys go into URL paths: accept only `PROJ-123` or a numeric id.
fn check_key(key: &str) -> Result<(), TrackerError> {
    let valid = if key.bytes().all(|b| b.is_ascii_digit()) {
        !key.is_empty()
    } else {
        match key.rsplit_once('-') {
            Some((project, number)) => {
                project.starts_with(|c: char| c.is_ascii_alphabetic())
                    && project
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && !number.is_empty()
                    && number.bytes().all(|b| b.is_ascii_digit())
            }
            None => false,
        }
    };
    if valid {
        Ok(())
    } else {
        Err(rejected_shape(&format!("invalid issue key {key:?}")))
    }
}

/// JQL string literal with `"` and `\` escaped.
fn jql_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn status_category(key: &str) -> StatusCategory {
    match key {
        "done" => StatusCategory::Done,
        "indeterminate" => StatusCategory::InProgress,
        // "new" and the rare "undefined".
        _ => StatusCategory::ToDo,
    }
}

#[async_trait::async_trait]
impl IssueTracker for JiraTracker {
    async fn find_by_label(&self, label: &str) -> Result<Vec<IssueRef>, TrackerError> {
        let jql = format!(
            "project = {} AND labels = {} ORDER BY created DESC",
            jql_string(&self.config.project),
            jql_string(label)
        );
        let body = json!({
            "jql": jql,
            "fields": ["summary", "status", "labels"],
            "maxResults": 10
        });
        let v = self
            .json(
                self.request(Method::POST, "/rest/api/3/search/jql")
                    .json(&body),
                label,
            )
            .await?;
        let issues = v["issues"]
            .as_array()
            .ok_or_else(|| rejected_shape("search response without issues"))?;
        Ok(issues
            .iter()
            .filter_map(|i| {
                let key = i["key"].as_str()?;
                Some(IssueRef {
                    key: key.to_string(),
                    url: self.issue_url(key),
                    status_category: status_category(
                        i["fields"]["status"]["statusCategory"]["key"]
                            .as_str()
                            .unwrap_or_default(),
                    ),
                })
            })
            .collect())
    }

    async fn create_bug(&self, bug: &NewBug) -> Result<IssueRef, TrackerError> {
        let mut fields = json!({
            "project": {"key": bug.project},
            "issuetype": {"name": "Bug"},
            "summary": bug.summary,
            "description": bug.description_adf,
            "labels": bug.labels,
        });
        if let Some(priority) = &bug.priority {
            fields["priority"] = json!({"name": priority});
        }
        let mut body = json!({"fields": fields});
        if let Some(linked) = &bug.linked_issue {
            body["update"] = json!({"issuelinks": [
                {"add": {"type": {"name": "Relates"}, "outwardIssue": {"key": linked}}}
            ]});
        }
        let subject = bug.linked_issue.as_deref().unwrap_or(&bug.project);
        let v = self
            .json(
                self.request(Method::POST, "/rest/api/3/issue").json(&body),
                subject,
            )
            .await?;
        let key = v["key"]
            .as_str()
            .ok_or_else(|| rejected_shape("create response without key"))?;
        Ok(IssueRef {
            key: key.to_string(),
            url: self.issue_url(key),
            status_category: StatusCategory::ToDo,
        })
    }

    async fn comment_adf(&self, key: &str, body: &Value) -> Result<CommentRef, TrackerError> {
        check_key(key)?;
        let v = self
            .json(
                self.request(Method::POST, &format!("/rest/api/3/issue/{key}/comment"))
                    .json(&json!({"body": body})),
                key,
            )
            .await?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| rejected_shape("comment response without id"))?;
        Ok(CommentRef {
            id: id.to_string(),
            url: Some(format!("{}?focusedCommentId={id}", self.issue_url(key))),
        })
    }

    async fn attach_file(
        &self,
        key: &str,
        file: &Attachment,
    ) -> Result<AttachmentRef, TrackerError> {
        check_key(key)?;
        let part = reqwest::multipart::Part::bytes(file.bytes.clone())
            .file_name(file.file_name.clone())
            .mime_str(&file.content_type)
            .map_err(|e| rejected_shape(&format!("invalid content type: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let v = self
            .json(
                self.request(
                    Method::POST,
                    &format!("/rest/api/3/issue/{key}/attachments"),
                )
                .header("X-Atlassian-Token", "no-check")
                .multipart(form),
                key,
            )
            .await?;
        let first = v
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| TrackerError::Rejected {
                status: 200,
                body: "attachment response is empty".into(),
            })?;
        Ok(AttachmentRef {
            id: first["id"].as_str().unwrap_or_default().to_string(),
            file_name: first["filename"]
                .as_str()
                .unwrap_or(&file.file_name)
                .to_string(),
        })
    }

    async fn add_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        if labels.is_empty() {
            return Ok(());
        }
        let ops: Vec<Value> = labels.iter().map(|l| json!({"add": l})).collect();
        self.put_issue(key, json!({"update": {"labels": ops}}))
            .await
    }

    async fn suggest_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        match &self.config.suggested_field {
            Some(field) => {
                self.put_issue(key, json!({"fields": {field.as_str(): labels}}))
                    .await
            }
            None => {
                let body = adf::doc(vec![
                    adf::heading(3, vec![adf::text("jev-suggested")]),
                    adf::bullet_list(labels.iter().map(|l| vec![adf::text(l.as_str())]).collect()),
                ]);
                self.comment_adf(key, &body).await.map(drop)
            }
        }
    }

    async fn get_issue(&self, key: &str) -> Result<Issue, TrackerError> {
        check_key(key)?;
        let v = self
            .json(
                self.request(Method::GET, &format!("/rest/api/3/issue/{key}"))
                    .query(&[("fields", "summary,description,labels,issuetype,status")]),
                key,
            )
            .await?;
        let f = &v["fields"];
        let key = v["key"].as_str().unwrap_or(key).to_string();
        Ok(Issue {
            url: self.issue_url(&key),
            key,
            summary: f["summary"].as_str().unwrap_or_default().to_string(),
            description: adf::to_plain_text(&f["description"]),
            issue_type: f["issuetype"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            status: f["status"]["name"].as_str().unwrap_or_default().to_string(),
            status_category: status_category(
                f["status"]["statusCategory"]["key"]
                    .as_str()
                    .unwrap_or_default(),
            ),
            labels: f["labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    fn parse_webhook(&self, request: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        let hook = self.parse_ticket_webhook(request)?;
        Ok(webhook::to_event(
            &hook,
            self.self_account_id.get().map(String::as_str),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_keys() {
        for ok in ["SHOP-1", "AB_2-30", "10007"] {
            assert!(check_key(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "SHOP",
            "-1",
            "SHOP-",
            "1A-2",
            "SHOP-1/comment",
            "../x-1",
            "SH OP-1",
        ] {
            assert!(check_key(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn jql_literals_are_escaped() {
        assert_eq!(jql_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn status_categories() {
        assert_eq!(status_category("new"), StatusCategory::ToDo);
        assert_eq!(status_category("indeterminate"), StatusCategory::InProgress);
        assert_eq!(status_category("done"), StatusCategory::Done);
        assert_eq!(status_category("undefined"), StatusCategory::ToDo);
    }

    #[test]
    fn long_error_bodies_are_truncated() {
        let s = truncate(&"x".repeat(MAX_ERROR_BODY + 5));
        assert_eq!(s.chars().count(), MAX_ERROR_BODY + 1);
    }
}
