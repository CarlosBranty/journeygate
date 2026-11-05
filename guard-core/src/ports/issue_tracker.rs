//! `IssueTracker`: Jira Cloud in production (REST v3).

use super::types::{
    Attachment, AttachmentRef, CommentRef, Issue, IssueRef, NewBug, WebhookRequest,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackerError {
    #[error("issue tracker not configured: {0}")]
    Unconfigured(String),
    #[error("issue {0} not found")]
    NotFound(String),
    /// 401 or 403.
    #[error("issue tracker refused the credentials (HTTP {0})")]
    Unauthorized(u16),
    /// 429.
    #[error("issue tracker rate limited the request")]
    RateLimited,
    #[error("issue tracker rejected the request (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("issue tracker transport error: {0}")]
    Transport(String),
}

/// A Jira webhook delivery the server acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackerEvent {
    /// `jira:issue_created`: triage it.
    IssueCreated {
        key: String,
    },
    /// `jira:issue_updated` with a change to `labels`: record the human label.
    LabelsChanged {
        key: String,
        labels: Vec<String>,
    },
    Ignored {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackerWebhookError {
    #[error("missing or invalid webhook secret")]
    Unauthorized,
    #[error("malformed webhook payload: {0}")]
    Malformed(String),
}

#[async_trait::async_trait]
pub trait IssueTracker: Send + Sync {
    /// Issues of the configured project carrying `label`, newest first
    /// (`POST /rest/api/3/search/jql`, at most 10). Done issues included.
    async fn find_by_label(&self, label: &str) -> Result<Vec<IssueRef>, TrackerError>;

    /// `POST /rest/api/3/issue` with issue type `Bug`.
    async fn create_bug(&self, bug: &NewBug) -> Result<IssueRef, TrackerError>;

    /// `POST /rest/api/3/issue/{key}/comment` with an ADF `body`.
    async fn comment_adf(
        &self,
        key: &str,
        body: &serde_json::Value,
    ) -> Result<CommentRef, TrackerError>;

    /// `POST /rest/api/3/issue/{key}/attachments`, multipart field `file`,
    /// header `X-Atlassian-Token: no-check`.
    async fn attach_file(
        &self,
        key: &str,
        file: &Attachment,
    ) -> Result<AttachmentRef, TrackerError>;

    /// Adds labels (`PUT /rest/api/3/issue/{key}` with `update.labels[].add`).
    /// Existing labels are kept; adding a present label is a no-op.
    async fn add_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError>;

    /// Shadow Mode: writes triage suggestions to the configured suggestion
    /// field (`JIRA_SUGGESTED_FIELD`, a labels-type custom field) instead of
    /// the real labels. Without that field configured, posts them as an ADF
    /// comment titled `jev-suggested`.
    async fn suggest_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError>;

    /// `GET /rest/api/3/issue/{key}?fields=summary,description,labels,issuetype,status`.
    async fn get_issue(&self, key: &str) -> Result<Issue, TrackerError>;

    /// Authenticates and parses a Jira webhook (server-api.md, `/webhooks/jira`).
    fn parse_webhook(&self, request: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError>;
}
