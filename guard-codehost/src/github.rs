//! GitHub REST adapter.
//!
//! | Port method | Requests |
//! | --- | --- |
//! | `pr_context` | `GET /repos/{repo}/pulls/{n}`, `GET /repos/{repo}/pulls/{n}/files?per_page=100` (follows `Link: rel="next"`) |
//! | `publish_status` | `GET /repos/{repo}/commits/{sha}/check-runs?check_name=<name>&filter=latest`, then `PATCH /repos/{repo}/check-runs/{id}` or `POST /repos/{repo}/check-runs` |
//! | `comment_pr` | `GET /repos/{repo}/issues/{n}/comments?per_page=100` (paginated), then `PATCH /repos/{repo}/issues/comments/{id}` or `POST /repos/{repo}/issues/{n}/comments` |
//! | `verify_webhook` | `X-Hub-Signature-256` HMAC-SHA256, constant time |
//!
//! Auth: `Authorization: Bearer <GITHUB_TOKEN>`, API version `2022-11-28`.

use crate::http::{self, MAX_PAGES};
use crate::{PR_COMMENT_MARKER, ticket_key, webhook};
use guard_core::ports::{
    CheckState, CodeHost, CodeHostError, CodeHostEvent, CodeHostProvider, CommentRef, GateStatus,
    PrAction, PrContext, PrRef, WebhookError, WebhookRequest,
};
use guard_core::ticket::Regex;
use reqwest::Method;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const DEFAULT_API_URL: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
/// GitHub limit of a check run `output.summary`.
const MAX_SUMMARY_CHARS: usize = 65_535;
const SIGNATURE_HEADER: &str = "X-Hub-Signature-256";
const EVENT_HEADER: &str = "X-GitHub-Event";

/// Connection settings. `Debug` redacts the token and the webhook secret.
#[derive(Debug, Clone)]
pub struct GithubConfig {
    api_url: String,
    repo: String,
    token: SecretString,
    webhook_secret: Option<SecretString>,
}

impl GithubConfig {
    /// `api_url` without the trailing slash (e.g. `https://api.github.com`,
    /// GHES `https://ghe.example/api/v3`), `repo` as `owner/name`.
    pub fn new(
        api_url: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            api_url: http::trim_base(&api_url.into()),
            repo: repo.into(),
            token: SecretString::from(token.into()),
            webhook_secret: None,
        }
    }

    /// The `GUARD_GITHUB_WEBHOOK_SECRET` used by `verify_webhook`.
    pub fn with_webhook_secret(mut self, secret: impl Into<String>) -> Self {
        self.webhook_secret = Some(SecretString::from(secret.into()));
        self
    }

    /// `GITHUB_TOKEN`, `GITHUB_REPOSITORY` (required), `GITHUB_API_URL`
    /// (default `https://api.github.com`), `GUARD_GITHUB_WEBHOOK_SECRET`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, CodeHostError> {
        let get = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
        let token = get("GITHUB_TOKEN")
            .ok_or_else(|| CodeHostError::Unconfigured("GITHUB_TOKEN is not set".into()))?;
        let repo = get("GITHUB_REPOSITORY")
            .ok_or_else(|| CodeHostError::Unconfigured("GITHUB_REPOSITORY is not set".into()))?;
        validate_repo(&repo)?;
        let api_url = get("GITHUB_API_URL").unwrap_or_else(|| DEFAULT_API_URL.to_string());
        let mut config = Self::new(api_url, repo, token);
        if let Some(secret) = get("GUARD_GITHUB_WEBHOOK_SECRET") {
            config = config.with_webhook_secret(secret);
        }
        Ok(config)
    }

    pub fn from_env() -> Result<Self, CodeHostError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    pub fn has_webhook_secret(&self) -> bool {
        self.webhook_secret
            .as_ref()
            .is_some_and(|s| !s.expose_secret().is_empty())
    }
}

pub(crate) fn validate_repo(repo: &str) -> Result<(), CodeHostError> {
    let valid = matches!(repo.split_once('/'), Some((owner, name))
        if !owner.is_empty() && !name.is_empty() && !name.contains('/')
            && repo.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)));
    if valid {
        Ok(())
    } else {
        Err(CodeHostError::Unconfigured(format!(
            "GITHUB_REPOSITORY must be owner/name, got {repo:?}"
        )))
    }
}

/// `CodeHost` for one GitHub repository.
#[derive(Debug, Clone)]
pub struct GithubCodeHost {
    config: GithubConfig,
    client: reqwest::Client,
}

impl GithubCodeHost {
    pub fn new(config: GithubConfig) -> Result<Self, CodeHostError> {
        validate_repo(&config.repo)?;
        Ok(Self {
            config,
            client: http::client()?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}/repos/{}{path}", self.config.api_url, self.config.repo)
    }

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .bearer_auth(self.config.token.expose_secret())
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", API_VERSION)
    }

    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        pr: Option<u64>,
    ) -> Result<reqwest::Response, CodeHostError> {
        let response = builder.send().await.map_err(http::transport)?;
        http::ok(response, pr).await
    }

    /// GETs `first_url` and every `Link: rel="next"` page after it.
    async fn paginate<T: DeserializeOwned>(
        &self,
        first_url: String,
        pr: Option<u64>,
    ) -> Result<Vec<T>, CodeHostError> {
        let mut items = Vec::new();
        let mut next = Some(first_url);
        let mut pages = 0;
        while let Some(url) = next.take() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(CodeHostError::Transport(format!(
                    "pagination exceeded {MAX_PAGES} pages"
                )));
            }
            let response = self.send(self.request(Method::GET, &url), pr).await?;
            next = response
                .headers()
                .get("link")
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            if let Some(link) = &next {
                if !http::same_origin(&self.config.api_url, link) {
                    return Err(CodeHostError::Transport(format!(
                        "refusing to follow a pagination link to another host: {link}"
                    )));
                }
            }
            items.extend(http::json::<Vec<T>>(response).await?);
        }
        Ok(items)
    }
}

/// The `rel="next"` URL of an RFC 8288 `Link` header.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (target, params) = part.split_once(';')?;
        let is_next = params.split(';').any(|p| {
            let p = p.trim();
            p == "rel=\"next\"" || p == "rel=next"
        });
        let target = target.trim();
        (is_next && target.starts_with('<') && target.ends_with('>'))
            .then(|| target[1..target.len() - 1].to_string())
    })
}

fn validate_sha(sha: &str) -> Result<(), CodeHostError> {
    if !sha.is_empty() && sha.len() <= 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        // Status 0: rejected locally, nothing was sent.
        Err(CodeHostError::Rejected {
            status: 0,
            body: format!("head_sha is not a hex commit id: {sha:?}"),
        })
    }
}

/// Body of the check run, without `head_sha` (only `POST` takes it).
fn check_run_body(status: &GateStatus) -> Value {
    let (state, conclusion) = match status.state {
        CheckState::Pending => ("queued", None),
        CheckState::Running => ("in_progress", None),
        CheckState::Success => ("completed", Some("success")),
        CheckState::Failure | CheckState::Error => ("completed", Some("failure")),
    };
    let summary = if status.summary.trim().is_empty() {
        status.title.clone()
    } else {
        http::truncate_chars(&status.summary, MAX_SUMMARY_CHARS)
    };
    let mut body = json!({
        "name": status.name,
        "status": state,
        "output": {"title": status.title, "summary": summary},
    });
    if let Some(conclusion) = conclusion {
        body["conclusion"] = json!(conclusion);
    }
    if let Some(url) = &status.details_url {
        body["details_url"] = json!(url);
    }
    body
}

pub(crate) fn with_marker(body: &str) -> String {
    if body.contains(PR_COMMENT_MARKER) {
        body.to_string()
    } else {
        format!("{PR_COMMENT_MARKER}\n{body}")
    }
}

#[derive(Deserialize)]
struct Pull {
    title: String,
    html_url: String,
    user: User,
    head: GitRef,
    base: GitRef,
}

#[derive(Deserialize)]
struct User {
    login: String,
}

#[derive(Deserialize)]
struct GitRef {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
}

#[derive(Deserialize)]
struct PullFile {
    filename: String,
    previous_filename: Option<String>,
}

#[derive(Deserialize)]
struct IssueComment {
    id: u64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
}

#[derive(Deserialize)]
struct CheckRuns {
    #[serde(default)]
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct CheckRun {
    id: u64,
}

#[derive(Deserialize)]
struct PullRequestEvent {
    action: String,
    number: u64,
    pull_request: PullRequestPayload,
}

#[derive(Deserialize)]
struct PullRequestPayload {
    title: String,
    #[serde(default)]
    merged: bool,
    head: GitRef,
    base: GitRef,
}

#[async_trait::async_trait]
impl CodeHost for GithubCodeHost {
    fn provider(&self) -> CodeHostProvider {
        CodeHostProvider::Github
    }

    async fn pr_context(
        &self,
        pr: PrRef,
        ticket_pattern: Option<&Regex>,
    ) -> Result<PrContext, CodeHostError> {
        let n = pr.number;
        let response = self
            .send(
                self.request(Method::GET, &self.url(&format!("/pulls/{n}"))),
                Some(n),
            )
            .await?;
        let pull: Pull = http::json(response).await?;
        let files: Vec<PullFile> = self
            .paginate(
                self.url(&format!("/pulls/{n}/files?per_page=100&page=1")),
                Some(n),
            )
            .await?;
        let changed_files: BTreeSet<String> = files
            .into_iter()
            .flat_map(|f| std::iter::once(f.filename).chain(f.previous_filename))
            .collect();
        Ok(PrContext {
            provider: CodeHostProvider::Github,
            repo: self.config.repo.clone(),
            number: n,
            ticket_key: ticket_key(ticket_pattern, &pull.head.name, &pull.title),
            title: pull.title,
            url: pull.html_url,
            author: pull.user.login,
            branch: pull.head.name,
            base_branch: pull.base.name,
            base_sha: pull.base.sha,
            head_sha: pull.head.sha,
            changed_files: changed_files.into_iter().collect(),
        })
    }

    async fn publish_status(&self, status: &GateStatus) -> Result<(), CodeHostError> {
        validate_sha(&status.head_sha)?;
        let lookup = self
            .request(
                Method::GET,
                &self.url(&format!("/commits/{}/check-runs", status.head_sha)),
            )
            .query(&[("check_name", status.name.as_str()), ("filter", "latest")]);
        let existing: CheckRuns = http::json(self.send(lookup, None).await?).await?;
        let mut body = check_run_body(status);
        let request = match existing.check_runs.first() {
            Some(run) => self
                .request(Method::PATCH, &self.url(&format!("/check-runs/{}", run.id)))
                .json(&body),
            None => {
                body["head_sha"] = json!(status.head_sha);
                self.request(Method::POST, &self.url("/check-runs"))
                    .json(&body)
            }
        };
        self.send(request, None).await?;
        Ok(())
    }

    async fn comment_pr(
        &self,
        pr: PrRef,
        body_markdown: &str,
    ) -> Result<CommentRef, CodeHostError> {
        let n = pr.number;
        let body = with_marker(body_markdown);
        let comments: Vec<IssueComment> = self
            .paginate(
                self.url(&format!("/issues/{n}/comments?per_page=100&page=1")),
                Some(n),
            )
            .await?;
        let existing = comments.into_iter().find(|c| {
            c.body
                .as_deref()
                .is_some_and(|b| b.contains(PR_COMMENT_MARKER))
        });
        let request = match existing {
            Some(comment) => self.request(
                Method::PATCH,
                &self.url(&format!("/issues/comments/{}", comment.id)),
            ),
            None => self.request(Method::POST, &self.url(&format!("/issues/{n}/comments"))),
        };
        let response = self
            .send(request.json(&json!({ "body": body })), Some(n))
            .await?;
        let saved: IssueComment = http::json(response).await?;
        Ok(CommentRef {
            id: saved.id.to_string(),
            url: saved.html_url,
        })
    }

    fn verify_webhook(&self, request: &WebhookRequest) -> Result<(), WebhookError> {
        let header = request
            .header(SIGNATURE_HEADER)
            .ok_or(WebhookError::MissingSignature(SIGNATURE_HEADER))?;
        // No configured secret: nothing can be verified, so nothing is accepted.
        let secret = self
            .config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose_secret())
            .filter(|s| !s.is_empty())
            .ok_or(WebhookError::InvalidSignature)?;
        webhook::verify_hub_signature(secret.as_bytes(), &request.body, header.trim())
    }

    fn parse_webhook(&self, request: &WebhookRequest) -> Result<CodeHostEvent, WebhookError> {
        let event = request
            .header(EVENT_HEADER)
            .ok_or_else(|| WebhookError::Malformed(format!("missing {EVENT_HEADER} header")))?;
        match event {
            "ping" => Ok(CodeHostEvent::Ping),
            "pull_request" => {
                let payload: PullRequestEvent = serde_json::from_slice(&request.body)
                    .map_err(|e| WebhookError::Malformed(e.to_string()))?;
                let action = match payload.action.as_str() {
                    "opened" => PrAction::Opened,
                    "synchronize" => PrAction::Synchronize,
                    "reopened" => PrAction::Reopened,
                    "edited" => PrAction::Edited,
                    "closed" if payload.pull_request.merged => PrAction::Merged,
                    "closed" => PrAction::Closed,
                    other => {
                        return Ok(CodeHostEvent::Ignored {
                            reason: format!("pull_request action {other:?} is not handled"),
                        });
                    }
                };
                let pr = payload.pull_request;
                Ok(CodeHostEvent::PullRequest {
                    action,
                    number: payload.number,
                    title: pr.title,
                    branch: pr.head.name,
                    head_sha: pr.head.sha,
                    base_sha: pr.base.sha,
                })
            }
            other => Ok(CodeHostEvent::Ignored {
                reason: format!("GitHub event {other:?} is not handled"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_link_parsing() {
        let header = "<https://api.github.com/x?page=2>; rel=\"next\", <https://api.github.com/x?page=5>; rel=\"last\"";
        assert_eq!(
            next_link(header).as_deref(),
            Some("https://api.github.com/x?page=2")
        );
        assert_eq!(
            next_link("<https://api.github.com/x?page=1>; rel=\"prev\""),
            None
        );
        assert_eq!(next_link(""), None);
    }

    #[test]
    fn repo_validation() {
        assert!(validate_repo("acme/shop").is_ok());
        assert!(validate_repo("acme/my.repo-1_x").is_ok());
        for bad in [
            "acme",
            "/shop",
            "acme/",
            "a/b/c",
            "acme/shop?x=1",
            "acme/../x",
        ] {
            // `acme/../x` has two slashes.
            assert!(validate_repo(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sha_validation() {
        assert!(validate_sha(&"a".repeat(40)).is_ok());
        assert!(validate_sha("").is_err());
        assert!(validate_sha("../x").is_err());
    }

    #[test]
    fn empty_summary_falls_back_to_the_title() {
        let status = GateStatus {
            head_sha: "a".repeat(40),
            name: "guard".into(),
            state: CheckState::Failure,
            title: "guard: red".into(),
            summary: " ".into(),
            details_url: None,
        };
        let body = check_run_body(&status);
        assert_eq!(body["output"]["summary"], "guard: red");
        assert_eq!(body["conclusion"], "failure");
        assert!(body.get("details_url").is_none());
    }
}
