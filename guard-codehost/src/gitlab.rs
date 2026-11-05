//! GitLab REST (v4) adapter.
//!
//! | Port method | Requests |
//! | --- | --- |
//! | `pr_context` | `GET /projects/:id/merge_requests/:iid`, `GET .../merge_requests/:iid/diffs?per_page=100` (follows `X-Next-Page`; falls back to the legacy `GET .../changes` when `/diffs` is 404, GitLab < 15.7) |
//! | `publish_status` | `POST /projects/:id/statuses/:sha` |
//! | `comment_pr` | `GET .../merge_requests/:iid/notes?sort=asc&order_by=created_at&per_page=100` (paginated), then `PUT .../notes/:note_id` or `POST .../notes` |
//! | `verify_webhook` | `X-Gitlab-Token` equal to the secret, constant time |
//!
//! Auth: `PRIVATE-TOKEN: <GITLAB_TOKEN>` (`api` scope). `:id` is the numeric
//! project id or the URL-encoded full path (`group%2Fsub%2Fname`).

use crate::github::with_marker;
use crate::http::{self, MAX_PAGES};
use crate::{PR_COMMENT_MARKER, ticket_key, webhook};
use guard_core::ports::{
    CheckState, CodeHost, CodeHostError, CodeHostEvent, CodeHostProvider, CommentRef, GateStatus,
    PrAction, PrContext, PrRef, WebhookError, WebhookRequest,
};
use guard_core::ticket::Regex;
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::collections::BTreeSet;

pub const DEFAULT_API_URL: &str = "https://gitlab.com/api/v4";
/// GitLab limit of a commit status `description`.
pub const MAX_DESCRIPTION_CHARS: usize = 140;
const TOKEN_HEADER: &str = "X-Gitlab-Token";
const EVENT_HEADER: &str = "X-Gitlab-Event";

/// Connection settings. `Debug` redacts the token and the webhook secret.
#[derive(Debug, Clone)]
pub struct GitlabConfig {
    api_url: String,
    project: String,
    token: SecretString,
    webhook_secret: Option<SecretString>,
}

impl GitlabConfig {
    /// `api_url` including `/api/v4`; `project` is a numeric id or the full
    /// path `group/sub/name`.
    pub fn new(
        api_url: impl Into<String>,
        project: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            api_url: http::trim_base(&api_url.into()),
            project: project.into(),
            token: SecretString::from(token.into()),
            webhook_secret: None,
        }
    }

    /// The `GUARD_GITLAB_WEBHOOK_SECRET` used by `verify_webhook`.
    pub fn with_webhook_secret(mut self, secret: impl Into<String>) -> Self {
        self.webhook_secret = Some(SecretString::from(secret.into()));
        self
    }

    /// `GITLAB_TOKEN` (required); project from `GITLAB_PROJECT`, else
    /// `CI_PROJECT_ID`; API URL from `GITLAB_API_URL`, else `CI_API_V4_URL`,
    /// else `https://gitlab.com/api/v4`; `GUARD_GITLAB_WEBHOOK_SECRET`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, CodeHostError> {
        let get = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
        let token = get("GITLAB_TOKEN")
            .ok_or_else(|| CodeHostError::Unconfigured("GITLAB_TOKEN is not set".into()))?;
        let project = get("GITLAB_PROJECT")
            .or_else(|| get("CI_PROJECT_ID"))
            .ok_or_else(|| {
                CodeHostError::Unconfigured("GITLAB_PROJECT (or CI_PROJECT_ID) is not set".into())
            })?;
        let api_url = get("GITLAB_API_URL")
            .or_else(|| get("CI_API_V4_URL"))
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());
        let mut config = Self::new(api_url, project, token);
        if let Some(secret) = get("GUARD_GITLAB_WEBHOOK_SECRET") {
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

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn has_webhook_secret(&self) -> bool {
        self.webhook_secret
            .as_ref()
            .is_some_and(|s| !s.expose_secret().is_empty())
    }
}

/// `:id` path segment: numeric ids verbatim, paths percent-encoded.
pub(crate) fn project_segment(project: &str) -> String {
    if !project.is_empty() && project.bytes().all(|b| b.is_ascii_digit()) {
        return project.to_string();
    }
    let mut out = String::with_capacity(project.len() * 3);
    for b in project.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `CodeHost` for one GitLab project.
#[derive(Debug, Clone)]
pub struct GitlabCodeHost {
    config: GitlabConfig,
    client: reqwest::Client,
}

impl GitlabCodeHost {
    pub fn new(config: GitlabConfig) -> Result<Self, CodeHostError> {
        if config.project.trim().is_empty() {
            return Err(CodeHostError::Unconfigured(
                "GitLab project is empty".into(),
            ));
        }
        Ok(Self {
            config,
            client: http::client()?,
        })
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/projects/{}{path}",
            self.config.api_url,
            project_segment(&self.config.project)
        )
    }

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, url)
            .header("private-token", self.config.token.expose_secret())
    }

    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
        pr: Option<u64>,
    ) -> Result<reqwest::Response, CodeHostError> {
        let response = builder.send().await.map_err(http::transport)?;
        http::ok(response, pr).await
    }

    /// GETs `url_with_query&page=1`, then every `X-Next-Page`.
    async fn paginate<T: DeserializeOwned>(
        &self,
        url_with_query: &str,
        pr: Option<u64>,
    ) -> Result<Vec<T>, CodeHostError> {
        let mut items = Vec::new();
        let mut page = Some("1".to_string());
        let mut pages = 0;
        while let Some(current) = page.take() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(CodeHostError::Transport(format!(
                    "pagination exceeded {MAX_PAGES} pages"
                )));
            }
            let url = format!("{url_with_query}&page={current}");
            let response = self.send(self.request(Method::GET, &url), pr).await?;
            page = response
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            if let Some(next) = &page {
                if !next.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(CodeHostError::Transport(format!(
                        "invalid X-Next-Page header: {next:?}"
                    )));
                }
            }
            items.extend(http::json::<Vec<T>>(response).await?);
        }
        Ok(items)
    }

    async fn changed_files(&self, iid: u64) -> Result<Vec<String>, CodeHostError> {
        let diffs: Vec<Diff> = match self
            .paginate(
                &self.url(&format!("/merge_requests/{iid}/diffs?per_page=100")),
                None,
            )
            .await
        {
            Ok(diffs) => diffs,
            // GitLab < 15.7 has no `/diffs`: use the deprecated `/changes`.
            Err(CodeHostError::Rejected { status: 404, .. }) => {
                let response = self
                    .send(
                        self.request(
                            Method::GET,
                            &self.url(&format!("/merge_requests/{iid}/changes")),
                        ),
                        Some(iid),
                    )
                    .await?;
                http::json::<Changes>(response).await?.changes
            }
            Err(other) => return Err(other),
        };
        let paths: BTreeSet<String> = diffs
            .into_iter()
            .flat_map(|d| [d.old_path, d.new_path])
            .filter(|p| !p.is_empty())
            .collect();
        Ok(paths.into_iter().collect())
    }
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

fn status_state(state: CheckState) -> &'static str {
    match state {
        CheckState::Pending => "pending",
        CheckState::Running => "running",
        CheckState::Success => "success",
        CheckState::Failure | CheckState::Error => "failed",
    }
}

#[derive(Deserialize)]
struct MergeRequest {
    title: String,
    web_url: String,
    author: Author,
    source_branch: String,
    target_branch: String,
    sha: String,
    #[serde(default)]
    references: Option<References>,
    #[serde(default)]
    diff_refs: Option<DiffRefs>,
}

#[derive(Deserialize)]
struct Author {
    username: String,
}

#[derive(Deserialize)]
struct References {
    full: Option<String>,
}

#[derive(Deserialize)]
struct DiffRefs {
    base_sha: Option<String>,
    head_sha: Option<String>,
}

#[derive(Deserialize)]
struct Diff {
    #[serde(default)]
    old_path: String,
    #[serde(default)]
    new_path: String,
}

#[derive(Deserialize)]
struct Changes {
    #[serde(default)]
    changes: Vec<Diff>,
}

#[derive(Deserialize)]
struct Note {
    id: u64,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    system: bool,
}

#[derive(Deserialize)]
struct MergeRequestHook {
    object_attributes: HookAttributes,
}

#[derive(Deserialize)]
struct HookAttributes {
    iid: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    source_branch: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    last_commit: Option<HookCommit>,
    #[serde(default)]
    diff_refs: Option<DiffRefs>,
}

#[derive(Deserialize)]
struct HookCommit {
    id: String,
}

#[async_trait::async_trait]
impl CodeHost for GitlabCodeHost {
    fn provider(&self) -> CodeHostProvider {
        CodeHostProvider::Gitlab
    }

    async fn pr_context(
        &self,
        pr: PrRef,
        ticket_pattern: Option<&Regex>,
    ) -> Result<PrContext, CodeHostError> {
        let iid = pr.number;
        let response = self
            .send(
                self.request(Method::GET, &self.url(&format!("/merge_requests/{iid}"))),
                Some(iid),
            )
            .await?;
        let mr: MergeRequest = http::json(response).await?;
        let (base_sha, head_sha) = match &mr.diff_refs {
            Some(DiffRefs {
                base_sha: Some(base),
                head_sha,
            }) if !base.is_empty() => (
                base.clone(),
                head_sha.clone().unwrap_or_else(|| mr.sha.clone()),
            ),
            _ => {
                // Status 0: the response was 200, but GitLab has not computed the diff yet.
                return Err(CodeHostError::Rejected {
                    status: 0,
                    body: format!("merge request !{iid} has no diff_refs yet; retry later"),
                });
            }
        };
        let repo = mr
            .references
            .as_ref()
            .and_then(|r| r.full.as_deref())
            .and_then(|full| full.rsplit_once('!').map(|(path, _)| path.to_string()))
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| self.config.project.clone());
        let changed_files = self.changed_files(iid).await?;
        Ok(PrContext {
            provider: CodeHostProvider::Gitlab,
            repo,
            number: iid,
            ticket_key: ticket_key(ticket_pattern, &mr.source_branch, &mr.title),
            title: mr.title,
            url: mr.web_url,
            author: mr.author.username,
            branch: mr.source_branch,
            base_branch: mr.target_branch,
            base_sha,
            head_sha,
            changed_files,
        })
    }

    async fn publish_status(&self, status: &GateStatus) -> Result<(), CodeHostError> {
        validate_sha(&status.head_sha)?;
        let mut body = json!({
            "state": status_state(status.state),
            "name": status.name,
            "description": http::truncate_chars(&status.title, MAX_DESCRIPTION_CHARS),
        });
        if let Some(url) = &status.details_url {
            body["target_url"] = json!(url);
        }
        let response = self
            .request(
                Method::POST,
                &self.url(&format!("/statuses/{}", status.head_sha)),
            )
            .json(&body)
            .send()
            .await
            .map_err(http::transport)?;
        if response.status() == StatusCode::BAD_REQUEST {
            // Re-posting the current state is refused with "Cannot transition
            // status via :x from :y": the commit already shows what we wanted.
            let text = response.text().await.unwrap_or_default();
            if text.contains("Cannot transition status") {
                return Ok(());
            }
            return Err(CodeHostError::Rejected {
                status: 400,
                body: http::truncate_chars(&text, 2000),
            });
        }
        http::ok(response, None).await?;
        Ok(())
    }

    async fn comment_pr(
        &self,
        pr: PrRef,
        body_markdown: &str,
    ) -> Result<CommentRef, CodeHostError> {
        let iid = pr.number;
        let body = with_marker(body_markdown);
        let notes: Vec<Note> = self
            .paginate(
                &self.url(&format!(
                    "/merge_requests/{iid}/notes?sort=asc&order_by=created_at&per_page=100"
                )),
                Some(iid),
            )
            .await?;
        let existing = notes.into_iter().find(|n| {
            !n.system
                && n.body
                    .as_deref()
                    .is_some_and(|b| b.contains(PR_COMMENT_MARKER))
        });
        let request = match existing {
            Some(note) => self.request(
                Method::PUT,
                &self.url(&format!("/merge_requests/{iid}/notes/{}", note.id)),
            ),
            None => self.request(
                Method::POST,
                &self.url(&format!("/merge_requests/{iid}/notes")),
            ),
        };
        let response = self
            .send(request.json(&json!({ "body": body })), Some(iid))
            .await?;
        let saved: Note = http::json(response).await?;
        Ok(CommentRef {
            id: saved.id.to_string(),
            url: None,
        })
    }

    fn verify_webhook(&self, request: &WebhookRequest) -> Result<(), WebhookError> {
        let token = request
            .header(TOKEN_HEADER)
            .ok_or(WebhookError::MissingSignature(TOKEN_HEADER))?;
        // No configured secret: nothing can be verified, so nothing is accepted.
        let secret = self
            .config
            .webhook_secret
            .as_ref()
            .map(|s| s.expose_secret())
            .filter(|s| !s.is_empty())
            .ok_or(WebhookError::InvalidSignature)?;
        webhook::verify_token(secret.as_bytes(), token.as_bytes())
    }

    fn parse_webhook(&self, request: &WebhookRequest) -> Result<CodeHostEvent, WebhookError> {
        let event = request
            .header(EVENT_HEADER)
            .ok_or_else(|| WebhookError::Malformed(format!("missing {EVENT_HEADER} header")))?;
        if event != "Merge Request Hook" {
            return Ok(CodeHostEvent::Ignored {
                reason: format!("GitLab event {event:?} is not handled"),
            });
        }
        let hook: MergeRequestHook = serde_json::from_slice(&request.body)
            .map_err(|e| WebhookError::Malformed(e.to_string()))?;
        let attrs = hook.object_attributes;
        let action = match attrs.action.as_deref() {
            Some("open") => PrAction::Opened,
            Some("update") => PrAction::Synchronize,
            Some("reopen") => PrAction::Reopened,
            Some("close") => PrAction::Closed,
            Some("merge") => PrAction::Merged,
            other => {
                return Ok(CodeHostEvent::Ignored {
                    reason: format!("merge request action {other:?} is not handled"),
                });
            }
        };
        Ok(CodeHostEvent::PullRequest {
            action,
            number: attrs.iid,
            title: attrs.title,
            branch: attrs.source_branch,
            head_sha: attrs.last_commit.map(|c| c.id).unwrap_or_default(),
            // Merge request hooks usually carry no diff_refs; empty when absent.
            base_sha: attrs.diff_refs.and_then(|d| d.base_sha).unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_segment_encoding() {
        assert_eq!(project_segment("42"), "42");
        assert_eq!(project_segment("acme/shop"), "acme%2Fshop");
        assert_eq!(project_segment("a b/c.d"), "a%20b%2Fc.d");
    }

    #[test]
    fn state_mapping() {
        assert_eq!(status_state(CheckState::Error), "failed");
        assert_eq!(status_state(CheckState::Pending), "pending");
    }
}
