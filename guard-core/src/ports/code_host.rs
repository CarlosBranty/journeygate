//! `CodeHost`: GitHub or GitLab.

use super::types::{
    CodeHostEvent, CodeHostProvider, CommentRef, GateStatus, PrContext, PrRef, WebhookRequest,
};
use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeHostError {
    #[error("code host not configured: {0}")]
    Unconfigured(String),
    #[error("pull request {0} not found")]
    NotFound(u64),
    /// 401 or 403.
    #[error("code host refused the credentials (HTTP {0})")]
    Unauthorized(u16),
    /// 429, or 403 with a rate-limit header exhausted.
    #[error("code host rate limited the request")]
    RateLimited,
    #[error("code host rejected the request (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("code host transport error: {0}")]
    Transport(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebhookError {
    /// The signature/token header is absent.
    #[error("missing webhook signature header {0}")]
    MissingSignature(&'static str),
    /// Signature or token does not match (compared in constant time).
    #[error("invalid webhook signature")]
    InvalidSignature,
    #[error("malformed webhook payload: {0}")]
    Malformed(String),
}

#[async_trait::async_trait]
pub trait CodeHost: Send + Sync {
    fn provider(&self) -> CodeHostProvider;

    /// PR metadata plus its changed files. `ticket_pattern` is
    /// `[tracker].ticket_pattern`: `ticket_key` is its first match in the
    /// branch name, else in the title, else `None`.
    async fn pr_context(
        &self,
        pr: PrRef,
        ticket_pattern: Option<&Regex>,
    ) -> Result<PrContext, CodeHostError>;

    /// GitHub: create a check run named `status.name` on `head_sha` (or update
    /// the latest one with that name). GitLab: `POST
    /// /projects/:id/statuses/:sha` with `name = status.name`.
    async fn publish_status(&self, status: &GateStatus) -> Result<(), CodeHostError>;

    /// Upserts the guard summary comment: the PR comment / MR note whose body
    /// contains the marker `<!-- guard:gate-summary -->` is edited; if none
    /// exists one is created. `body_markdown` must contain the marker.
    async fn comment_pr(&self, pr: PrRef, body_markdown: &str)
    -> Result<CommentRef, CodeHostError>;

    /// Verifies the delivery: GitHub `X-Hub-Signature-256` (`sha256=` + hex
    /// HMAC-SHA256 of the raw body with the webhook secret); GitLab
    /// `X-Gitlab-Token` equal to the secret. Constant-time comparison.
    fn verify_webhook(&self, request: &WebhookRequest) -> Result<(), WebhookError>;

    /// Parses a verified delivery. Callers must call `verify_webhook` first.
    fn parse_webhook(&self, request: &WebhookRequest) -> Result<CodeHostEvent, WebhookError>;
}

// ---------------------------------------------------------------------------
// Agent pull requests (contract: docs/contracts/pr-publisher.md)
// ---------------------------------------------------------------------------

/// Label every agent PR / MR carries.
pub const AGENT_LABEL: &str = "agent";

/// Prefix of every branch an agent PR is opened from: `agent/<KEY>`.
pub const AGENT_BRANCH_PREFIX: &str = "agent/";

/// `agent/<KEY>`.
pub fn agent_branch(ticket: &str) -> String {
    format!("{AGENT_BRANCH_PREFIX}{ticket}")
}

/// A ticket key usable in a branch name: ASCII letters, digits, `-`, `_`
/// and `.`, starting with a letter or digit, no `..`, not ending with `.`,
/// at most 100 characters.
pub fn is_valid_ticket_key(ticket: &str) -> bool {
    !ticket.is_empty()
        && ticket.len() <= 100
        && ticket.as_bytes()[0].is_ascii_alphanumeric()
        && !ticket.ends_with('.')
        && !ticket.contains("..")
        && ticket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// Status 0: rejected locally, nothing was sent.
fn rejected_locally(body: String) -> CodeHostError {
    CodeHostError::Rejected { status: 0, body }
}

/// Pushes a local commit to `agent/<ticket>` on the code host remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPush {
    /// A local git repository (work tree or bare) holding `rev`.
    pub local_repo: std::path::PathBuf,
    /// Any revision resolving to a commit (sha, local branch, tag).
    pub rev: String,
    pub ticket: String,
}

impl BranchPush {
    pub fn branch(&self) -> String {
        agent_branch(&self.ticket)
    }

    pub fn validate(&self) -> Result<(), CodeHostError> {
        if !is_valid_ticket_key(&self.ticket) {
            return Err(rejected_locally(format!(
                "ticket key is not usable in a branch name: {:?}",
                self.ticket
            )));
        }
        if self.rev.is_empty()
            || self.rev.starts_with('-')
            || self.rev.contains(char::is_whitespace)
        {
            return Err(rejected_locally(format!(
                "not a usable revision: {:?}",
                self.rev
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedBranch {
    /// `agent/<KEY>`.
    pub branch: String,
    /// Full sha of the pushed commit.
    pub commit: String,
}

/// The PR / MR the orchestrator opens for one lane-A ticket. Its head is
/// always `agent/<ticket>`; it is a draft by default and is never merged by
/// guard (every agent PR requires a human approval).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPullRequest {
    pub ticket: String,
    pub title: String,
    /// Markdown summary of the change (first section of the body).
    pub summary: String,
    /// Target branch; `None` uses the publisher's configured base branch,
    /// else the repository default branch.
    pub base_branch: Option<String>,
    /// Commit the Coder diff was applied on (shown in the body).
    pub base_commit: String,
    pub sealed_spec: super::agent::SealedSpec,
    /// Coder attempts spent on the ticket.
    pub attempts: u32,
    /// Total model spend of the ticket in USD.
    pub cost_usd: f64,
    /// Free-form coder description, e.g. "coder (claude-code, sonnet)".
    pub coder: Option<String>,
    /// Extra labels; [`AGENT_LABEL`] is always added.
    pub labels: Vec<String>,
    pub draft: bool,
}

impl AgentPullRequest {
    /// A draft PR with no extra labels, zero attempts and zero cost.
    pub fn new(
        ticket: impl Into<String>,
        title: impl Into<String>,
        sealed_spec: super::agent::SealedSpec,
    ) -> Self {
        Self {
            ticket: ticket.into(),
            title: title.into(),
            summary: String::new(),
            base_branch: None,
            base_commit: String::new(),
            sealed_spec,
            attempts: 0,
            cost_usd: 0.0,
            coder: None,
            labels: Vec::new(),
            draft: true,
        }
    }

    pub fn branch(&self) -> String {
        agent_branch(&self.ticket)
    }

    /// [`AGENT_LABEL`] first, then `labels` and `extra` in order, trimmed,
    /// without empties or duplicates.
    pub fn all_labels(&self, extra: &[String]) -> Vec<String> {
        let mut out = vec![AGENT_LABEL.to_string()];
        for label in self.labels.iter().chain(extra) {
            let label = label.trim();
            if !label.is_empty() && !out.iter().any(|l| l == label) {
                out.push(label.to_string());
            }
        }
        out
    }

    pub fn validate(&self) -> Result<(), CodeHostError> {
        if !is_valid_ticket_key(&self.ticket) {
            return Err(rejected_locally(format!(
                "ticket key is not usable in a branch name: {:?}",
                self.ticket
            )));
        }
        if self.title.trim().is_empty() {
            return Err(rejected_locally("pull request title is empty".into()));
        }
        if !super::agent::SealedSpec::is_valid_digest(&self.sealed_spec.sha256) {
            return Err(rejected_locally(format!(
                "sealed spec sha256 is not 64 lowercase hex characters: {:?}",
                self.sealed_spec.sha256
            )));
        }
        if !self.cost_usd.is_finite() || self.cost_usd < 0.0 {
            return Err(rejected_locally(format!(
                "cost_usd is not a valid amount: {}",
                self.cost_usd
            )));
        }
        if let Some(base) = &self.base_branch {
            if base.trim().is_empty() || base.starts_with('-') || base.contains(char::is_whitespace)
            {
                return Err(rejected_locally(format!(
                    "not a usable base branch: {base:?}"
                )));
            }
        }
        if self.labels.iter().any(|l| l.contains(',')) {
            return Err(rejected_locally("labels must not contain commas".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedPullRequest {
    pub provider: CodeHostProvider,
    /// PR number (GitHub) or MR IID (GitLab).
    pub number: u64,
    pub url: String,
    /// `false` when an open PR / MR for `agent/<KEY>` already existed and was
    /// updated instead.
    pub created: bool,
    pub draft: bool,
    /// Labels guard applied (added to whatever the PR already had).
    pub labels: Vec<String>,
}

/// Publishes agent PRs: pushes `agent/<KEY>` and opens (or updates) its PR /
/// MR. Separate from [`CodeHost`] because only the orchestrator holds a
/// token with push rights. Implementations never merge and never enable
/// auto-merge.
#[async_trait::async_trait]
pub trait PullRequestPublisher: Send + Sync {
    fn provider(&self) -> CodeHostProvider;

    /// Force-pushes `push.rev` to `refs/heads/agent/<ticket>` on the remote.
    /// Only `agent/` branches are ever written.
    async fn push_branch(&self, push: &BranchPush) -> Result<PushedBranch, CodeHostError>;

    /// Idempotent: an open PR / MR whose head is `agent/<ticket>` is updated
    /// (title, body, labels added); otherwise one is created (draft unless
    /// `pr.draft` is false). Never un-drafts an existing one.
    async fn open_pull_request(
        &self,
        pr: &AgentPullRequest,
    ) -> Result<OpenedPullRequest, CodeHostError>;
}

#[cfg(test)]
mod agent_pr_tests {
    use super::*;
    use crate::ports::agent::SealedSpec;

    fn pr() -> AgentPullRequest {
        AgentPullRequest::new(
            "SHOP-1",
            "SHOP-1: add cart",
            SealedSpec::seal("t.spec.ts", b"x"),
        )
    }

    #[test]
    fn new_is_a_draft_on_the_agent_branch() {
        let p = pr();
        assert!(p.draft);
        assert_eq!(p.branch(), "agent/SHOP-1");
        assert_eq!(p.all_labels(&[]), vec!["agent".to_string()]);
        p.validate().unwrap();
    }

    #[test]
    fn labels_are_agent_first_trimmed_and_deduplicated() {
        let mut p = pr();
        p.labels = vec![" lane-a ".into(), "agent".into(), "".into()];
        assert_eq!(
            p.all_labels(&["lane-a".into(), "guard".into()]),
            vec!["agent", "lane-a", "guard"]
        );
    }

    #[test]
    fn validation_rejects_unsafe_input_locally() {
        for ticket in ["", "-x", "a/b", "a b", "a..b", "a.", "\u{fc}"] {
            let mut p = pr();
            p.ticket = ticket.into();
            assert!(
                matches!(p.validate(), Err(CodeHostError::Rejected { status: 0, .. })),
                "{ticket:?}"
            );
        }
        let mut p = pr();
        p.sealed_spec.sha256 = "ABC".into();
        assert!(p.validate().is_err());
        let mut p = pr();
        p.cost_usd = f64::NAN;
        assert!(p.validate().is_err());
        let mut p = pr();
        p.labels = vec!["a,b".into()];
        assert!(p.validate().is_err());
        let mut p = pr();
        p.base_branch = Some("--upload-pack=x".into());
        assert!(p.validate().is_err());
    }

    #[test]
    fn branch_push_validates_ticket_and_revision() {
        let mut push = BranchPush {
            local_repo: ".".into(),
            rev: "HEAD".into(),
            ticket: "SHOP-1".into(),
        };
        push.validate().unwrap();
        assert_eq!(push.branch(), "agent/SHOP-1");
        push.rev = "--all".into();
        assert!(push.validate().is_err());
        push.rev = "HEAD".into();
        push.ticket = "../x".into();
        assert!(push.validate().is_err());
    }
}
