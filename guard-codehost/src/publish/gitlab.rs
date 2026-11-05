//! GitLab `PullRequestPublisher`.
//!
//! | Step | Request (REST v4) |
//! | --- | --- |
//! | push | `git push --force <push_url> <sha>:refs/heads/agent/<KEY>` (basic auth `oauth2:<token>`) |
//! | base branch | configured, else `GET /projects/:id` -> `default_branch` |
//! | find | `GET /projects/:id/merge_requests?state=opened&source_branch=agent/<KEY>` |
//! | re-review | on an existing open MR with `merge_when_pipeline_succeeds`, before the update: `POST /projects/:id/merge_requests/:iid/cancel_merge_when_pipeline_succeeds` |
//! | update | `PUT /projects/:id/merge_requests/:iid` (`title`, `description`, `add_labels`; a draft request puts the `Draft: ` prefix back on an MR a human un-drafted) |
//! | create | `POST /projects/:id/merge_requests` (`source_branch`, `target_branch`, `title` with `Draft: `, `description`, `labels`); a 409 re-runs find + update |
//!
//! Docs: <https://docs.gitlab.com/api/merge_requests/>,
//! <https://docs.gitlab.com/user/project/merge_requests/drafts/> (the
//! `Draft:` title prefix), <https://docs.gitlab.com/api/projects/#get-a-single-project>.
//! `labels` / `add_labels` are comma-separated; unknown labels are created.
//! The merge endpoint (and so `merge_when_pipeline_succeeds`) is never called.
//! A re-publish has already force-pushed new commits, so an MR set to merge
//! when its pipeline succeeds has that cancelled first
//! (<https://docs.gitlab.com/api/merge_requests/#cancel-merge-when-pipeline-succeeds>).

use super::{CommonEnv, common_env, git, render_body};
use crate::gitlab::project_segment;
use crate::http;
use guard_core::ports::code_host::{
    AgentPullRequest, BranchPush, OpenedPullRequest, PullRequestPublisher, PushedBranch,
};
use guard_core::ports::{CodeHostError, CodeHostProvider};
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;

/// HTTP basic-auth user GitLab accepts with a token as password.
const PUSH_USER: &str = "oauth2";
const DRAFT_PREFIX: &str = "Draft: ";

/// Publisher settings. `Debug` redacts the token.
#[derive(Debug, Clone)]
pub struct GitlabPublisherConfig {
    api_url: String,
    project: String,
    token: SecretString,
    push_url: Option<String>,
    base_branch: Option<String>,
    labels: Vec<String>,
}

impl GitlabPublisherConfig {
    /// `api_url` including `/api/v4`. For a project path the push URL is
    /// `<api_url without /api/v4>/<path>.git`; a numeric id needs
    /// [`with_push_url`](Self::with_push_url).
    pub fn new(
        api_url: impl Into<String>,
        project: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        let api_url = http::trim_base(&api_url.into());
        let project = project.into();
        let push_url = derive_push_url(&api_url, &project);
        Self {
            api_url,
            project,
            token: SecretString::from(token.into()),
            push_url,
            base_branch: None,
            labels: Vec::new(),
        }
    }

    pub fn with_push_url(mut self, url: impl Into<String>) -> Self {
        self.push_url = Some(url.into());
        self
    }

    pub fn with_base_branch(mut self, branch: impl Into<String>) -> Self {
        self.base_branch = Some(branch.into());
        self
    }

    /// Extra labels on every agent MR (after `agent` and the request's).
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    /// Token from the variable named by `GUARD_PR_TOKEN_ENV` (default
    /// `GITLAB_TOKEN`); project from `GITLAB_PROJECT`, else `CI_PROJECT_PATH`,
    /// else `CI_PROJECT_ID`; API URL from `GITLAB_API_URL`, else
    /// `CI_API_V4_URL`, else `https://gitlab.com/api/v4`; `GUARD_PR_PUSH_URL`,
    /// `GUARD_PR_BASE_BRANCH`, `GUARD_PR_LABELS`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, CodeHostError> {
        let get = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
        let project = get("GITLAB_PROJECT")
            .or_else(|| get("CI_PROJECT_PATH"))
            .or_else(|| get("CI_PROJECT_ID"))
            .ok_or_else(|| {
                CodeHostError::Unconfigured(
                    "GITLAB_PROJECT (or CI_PROJECT_PATH / CI_PROJECT_ID) is not set".into(),
                )
            })?;
        let CommonEnv {
            token,
            push_url,
            base_branch,
            labels,
        } = common_env(&lookup, "GITLAB_TOKEN")?;
        let api_url = get("GITLAB_API_URL")
            .or_else(|| get("CI_API_V4_URL"))
            .unwrap_or_else(|| crate::gitlab::DEFAULT_API_URL.to_string());
        let mut config = Self::new(api_url, project, token).with_labels(labels);
        if let Some(url) = push_url {
            config = config.with_push_url(url);
        }
        if let Some(base) = base_branch {
            config = config.with_base_branch(base);
        }
        if config.push_url.is_none() {
            return Err(CodeHostError::Unconfigured(format!(
                "cannot derive the git remote for project {:?}; set {}",
                config.project,
                super::PUSH_URL_VAR
            )));
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

    pub fn push_url(&self) -> Option<&str> {
        self.push_url.as_deref()
    }

    pub fn base_branch(&self) -> Option<&str> {
        self.base_branch.as_deref()
    }

    pub fn labels(&self) -> &[String] {
        &self.labels
    }
}

fn derive_push_url(api_url: &str, project: &str) -> Option<String> {
    if project.is_empty() || project.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let web = api_url.strip_suffix("/api/v4")?;
    web.starts_with("https://")
        .then(|| format!("{web}/{}.git", project.trim_matches('/')))
}

/// `true` when GitLab treats the title as a draft.
fn has_draft_prefix(title: &str) -> bool {
    let lower = title.trim_start().to_ascii_lowercase();
    ["draft:", "[draft]", "(draft)"]
        .iter()
        .any(|p| lower.starts_with(p))
}

fn draft_title(title: &str, draft: bool) -> String {
    if draft && !has_draft_prefix(title) {
        format!("{DRAFT_PREFIX}{title}")
    } else {
        title.to_string()
    }
}

/// Pushes `agent/<KEY>` and opens its draft MR on one GitLab project.
#[derive(Debug, Clone)]
pub struct GitlabPublisher {
    config: GitlabPublisherConfig,
    client: reqwest::Client,
}

impl GitlabPublisher {
    pub fn new(config: GitlabPublisherConfig) -> Result<Self, CodeHostError> {
        if config.project.trim().is_empty() {
            return Err(CodeHostError::Unconfigured(
                "GitLab project is empty".into(),
            ));
        }
        if let Some(url) = &config.push_url {
            git::validate_push_url(url)?;
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
            .header("PRIVATE-TOKEN", self.config.token.expose_secret())
    }

    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, CodeHostError> {
        builder.send().await.map_err(http::transport)
    }

    async fn base_branch(&self, pr: &AgentPullRequest) -> Result<String, CodeHostError> {
        if let Some(base) = pr.base_branch.as_ref().or(self.config.base_branch.as_ref()) {
            return Ok(base.clone());
        }
        let response = self.send(self.request(Method::GET, &self.url(""))).await?;
        let project: Project = http::json(http::ok(response, None).await?).await?;
        project
            .default_branch
            .ok_or_else(|| CodeHostError::Rejected {
                status: 200,
                body: "project has no default branch (empty repository)".into(),
            })
    }

    async fn find_open(&self, branch: &str) -> Result<Option<MergeRequest>, CodeHostError> {
        let builder = self
            .request(Method::GET, &self.url("/merge_requests"))
            .query(&[
                ("state", "opened"),
                ("source_branch", branch),
                ("per_page", "100"),
            ]);
        let response = http::ok(self.send(builder).await?, None).await?;
        let mrs: Vec<MergeRequest> = http::json(response).await?;
        Ok(mrs.into_iter().next())
    }

    async fn update(
        &self,
        existing: &MergeRequest,
        pr: &AgentPullRequest,
        body: &str,
        labels: &str,
    ) -> Result<MergeRequest, CodeHostError> {
        if existing.merge_when_pipeline_succeeds {
            let url = self.url(&format!(
                "/merge_requests/{}/cancel_merge_when_pipeline_succeeds",
                existing.iid
            ));
            let response = self.send(self.request(Method::POST, &url)).await?;
            // 405/406: nothing to cancel any more (merged, closed, or unset
            // meanwhile).
            if !matches!(response.status().as_u16(), 405 | 406) {
                http::ok(response, Some(existing.iid)).await?;
            }
        }
        // Never un-draft: keep the prefix if the MR already is a draft.
        let title = draft_title(&pr.title, pr.draft || existing.is_draft());
        let builder = self
            .request(
                Method::PUT,
                &self.url(&format!("/merge_requests/{}", existing.iid)),
            )
            .json(&json!({"title": title, "description": body, "add_labels": labels}));
        http::json(http::ok(self.send(builder).await?, Some(existing.iid)).await?).await
    }
}

#[derive(Deserialize)]
struct Project {
    #[serde(default)]
    default_branch: Option<String>,
}

#[derive(Deserialize)]
struct MergeRequest {
    iid: u64,
    web_url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    draft: Option<bool>,
    /// Deprecated alias of `draft` (older GitLab).
    #[serde(default)]
    work_in_progress: Option<bool>,
    #[serde(default)]
    merge_when_pipeline_succeeds: bool,
}

impl MergeRequest {
    fn is_draft(&self) -> bool {
        self.draft
            .or(self.work_in_progress)
            .unwrap_or_else(|| has_draft_prefix(&self.title))
    }
}

#[async_trait::async_trait]
impl PullRequestPublisher for GitlabPublisher {
    fn provider(&self) -> CodeHostProvider {
        CodeHostProvider::Gitlab
    }

    async fn push_branch(&self, push: &BranchPush) -> Result<PushedBranch, CodeHostError> {
        let remote = self.config.push_url.as_deref().ok_or_else(|| {
            CodeHostError::Unconfigured(format!("no git remote; set {}", super::PUSH_URL_VAR))
        })?;
        git::push(push, remote, PUSH_USER, &self.config.token).await
    }

    async fn open_pull_request(
        &self,
        pr: &AgentPullRequest,
    ) -> Result<OpenedPullRequest, CodeHostError> {
        pr.validate()?;
        let branch = pr.branch();
        let body = render_body(pr);
        let labels = pr.all_labels(&self.config.labels);
        let joined = labels.join(",");
        let (mr, created) = match self.find_open(&branch).await? {
            Some(existing) => (self.update(&existing, pr, &body, &joined).await?, false),
            None => {
                let base = self.base_branch(pr).await?;
                let builder = self
                    .request(Method::POST, &self.url("/merge_requests"))
                    .json(&json!({
                        "source_branch": branch,
                        "target_branch": base,
                        "title": draft_title(&pr.title, pr.draft),
                        "description": body,
                        "labels": joined,
                    }));
                let response = self.send(builder).await?;
                if response.status() == StatusCode::CONFLICT {
                    // Another open MR exists for the source branch (a
                    // concurrent publish): update it instead.
                    let error = http::error_for(response, None).await;
                    match self.find_open(&branch).await? {
                        Some(existing) => {
                            (self.update(&existing, pr, &body, &joined).await?, false)
                        }
                        None => return Err(error),
                    }
                } else {
                    (http::json(http::ok(response, None).await?).await?, true)
                }
            }
        };
        Ok(OpenedPullRequest {
            provider: CodeHostProvider::Gitlab,
            number: mr.iid,
            draft: mr.is_draft(),
            url: mr.web_url,
            created,
            labels,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_titles_are_prefixed_once() {
        assert_eq!(draft_title("x", true), "Draft: x");
        assert_eq!(draft_title("Draft: x", true), "Draft: x");
        assert_eq!(draft_title("[Draft] x", true), "[Draft] x");
        assert_eq!(draft_title("x", false), "x");
        assert!(has_draft_prefix("(draft) x"));
        assert!(!has_draft_prefix("drafting x"));
    }

    #[test]
    fn push_urls_derive_from_https_api_urls_and_paths_only() {
        assert_eq!(
            derive_push_url("https://gitlab.com/api/v4", "acme/shop").as_deref(),
            Some("https://gitlab.com/acme/shop.git")
        );
        assert_eq!(derive_push_url("https://gitlab.com/api/v4", "42"), None);
        assert_eq!(derive_push_url("http://127.0.0.1:1", "acme/shop"), None);
    }
}
