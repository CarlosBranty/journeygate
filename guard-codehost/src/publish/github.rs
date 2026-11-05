//! GitHub `PullRequestPublisher`.
//!
//! | Step | Request (REST API version `2022-11-28`) |
//! | --- | --- |
//! | push | `git push --force <push_url> <sha>:refs/heads/agent/<KEY>` (basic auth `x-access-token:<token>`) |
//! | base branch | configured, else `GET /repos/{repo}` -> `default_branch` |
//! | find | `GET /repos/{repo}/pulls?state=open&head={owner}:agent/<KEY>` |
//! | re-review | on an existing open PR, before the update: GraphQL `disablePullRequestAutoMerge` when `auto_merge` is set, and `convertPullRequestToDraft` when the request is a draft and the PR is not (`POST {graphql}`, `pullRequestId` = the PR `node_id`) |
//! | update | `PATCH /repos/{repo}/pulls/{n}` (`title`, `body`) |
//! | create | `POST /repos/{repo}/pulls` (`title`, `head`, `base`, `body`, `draft`, `maintainer_can_modify: false`); a 422 "already exists" re-runs find + update |
//! | labels | `POST /repos/{repo}/labels` per label (201, or 422 `already_exists`), then `POST /repos/{repo}/issues/{n}/labels` |
//!
//! Docs: <https://docs.github.com/en/rest/pulls/pulls>,
//! <https://docs.github.com/en/rest/issues/labels>,
//! <https://docs.github.com/en/rest/repos/repos#get-a-repository>,
//! <https://docs.github.com/en/graphql/reference/mutations#convertpullrequesttodraft>,
//! <https://docs.github.com/en/graphql/reference/mutations#disablepullrequestautomerge>.
//! The GraphQL endpoint is `https://api.github.com/graphql`, and
//! `https://<host>/api/graphql` on GHES (`/api/v3` REST base).
//! The merge endpoints and the GraphQL `enablePullRequestAutoMerge` mutation
//! are never called. A re-publish has already force-pushed new commits onto
//! `agent/<KEY>`, so an open PR a human marked ready (and maybe approved, with
//! auto-merge on) goes back to draft first: nothing merges until a human looks
//! at the new commits.

use super::{CommonEnv, common_env, git, render_body};
use crate::http;
use guard_core::ports::code_host::{
    AGENT_LABEL, AgentPullRequest, BranchPush, OpenedPullRequest, PullRequestPublisher,
    PushedBranch,
};
use guard_core::ports::{CodeHostError, CodeHostProvider};
use reqwest::{Method, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;

const API_VERSION: &str = "2022-11-28";
/// HTTP basic-auth user GitHub expects with a token as password.
const PUSH_USER: &str = "x-access-token";
const AGENT_LABEL_COLOR: &str = "5319e7";
const OTHER_LABEL_COLOR: &str = "ededed";
const AGENT_LABEL_DESCRIPTION: &str = "Opened by a guard agent; human approval required";

/// Publisher settings. `Debug` redacts the token.
#[derive(Debug, Clone)]
pub struct GithubPublisherConfig {
    api_url: String,
    repo: String,
    token: SecretString,
    push_url: Option<String>,
    base_branch: Option<String>,
    labels: Vec<String>,
}

impl GithubPublisherConfig {
    /// `api_url` as in `GithubConfig::new`; the push URL is derived for
    /// `https://api.github.com` (`https://github.com/{repo}.git`) and GHES
    /// `https://<host>/api/v3` (`https://<host>/{repo}.git`).
    pub fn new(
        api_url: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        let api_url = http::trim_base(&api_url.into());
        let repo = repo.into();
        let push_url = derive_push_url(&api_url, &repo);
        Self {
            api_url,
            repo,
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

    /// Extra labels on every agent PR (after `agent` and the request's).
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.labels = labels;
        self
    }

    /// Token from the variable named by `GUARD_PR_TOKEN_ENV` (default
    /// `GITHUB_TOKEN`), `GITHUB_REPOSITORY` (required), `GITHUB_API_URL`,
    /// `GUARD_PR_PUSH_URL`, `GUARD_PR_BASE_BRANCH`, `GUARD_PR_LABELS`.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, CodeHostError> {
        let get = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
        let repo = get("GITHUB_REPOSITORY")
            .ok_or_else(|| CodeHostError::Unconfigured("GITHUB_REPOSITORY is not set".into()))?;
        crate::github::validate_repo(&repo)?;
        let CommonEnv {
            token,
            push_url,
            base_branch,
            labels,
        } = common_env(&lookup, "GITHUB_TOKEN")?;
        let api_url =
            get("GITHUB_API_URL").unwrap_or_else(|| crate::github::DEFAULT_API_URL.to_string());
        let mut config = Self::new(api_url, repo, token).with_labels(labels);
        if let Some(url) = push_url {
            config = config.with_push_url(url);
        }
        if let Some(base) = base_branch {
            config = config.with_base_branch(base);
        }
        if config.push_url.is_none() {
            return Err(CodeHostError::Unconfigured(format!(
                "cannot derive the git remote from GITHUB_API_URL {:?}; set {}",
                config.api_url,
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

    pub fn repo(&self) -> &str {
        &self.repo
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

fn derive_push_url(api_url: &str, repo: &str) -> Option<String> {
    if api_url == crate::github::DEFAULT_API_URL {
        return Some(format!("https://github.com/{repo}.git"));
    }
    let web = api_url.strip_suffix("/api/v3")?;
    web.starts_with("https://")
        .then(|| format!("{web}/{repo}.git"))
}

/// Pushes `agent/<KEY>` and opens its draft PR on one GitHub repository.
#[derive(Debug, Clone)]
pub struct GithubPublisher {
    config: GithubPublisherConfig,
    client: reqwest::Client,
}

impl GithubPublisher {
    pub fn new(config: GithubPublisherConfig) -> Result<Self, CodeHostError> {
        crate::github::validate_repo(&config.repo)?;
        if let Some(url) = &config.push_url {
            git::validate_push_url(url)?;
        }
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
    ) -> Result<reqwest::Response, CodeHostError> {
        builder.send().await.map_err(http::transport)
    }

    async fn base_branch(&self, pr: &AgentPullRequest) -> Result<String, CodeHostError> {
        if let Some(base) = pr.base_branch.as_ref().or(self.config.base_branch.as_ref()) {
            return Ok(base.clone());
        }
        let response = self.send(self.request(Method::GET, &self.url(""))).await?;
        let repo: Repository = http::json(http::ok(response, None).await?).await?;
        Ok(repo.default_branch)
    }

    async fn find_open(&self, branch: &str) -> Result<Option<Pull>, CodeHostError> {
        let owner = self.config.repo.split('/').next().unwrap_or_default();
        let head = format!("{owner}:{branch}");
        let builder = self.request(Method::GET, &self.url("/pulls")).query(&[
            ("state", "open"),
            ("head", head.as_str()),
            ("per_page", "100"),
        ]);
        let response = http::ok(self.send(builder).await?, None).await?;
        let pulls: Vec<Pull> = http::json(response).await?;
        Ok(pulls.into_iter().next())
    }

    /// GraphQL endpoint next to the REST base.
    fn graphql_url(&self) -> String {
        match self.config.api_url.strip_suffix("/api/v3") {
            Some(web) => format!("{web}/api/graphql"),
            None => format!("{}/graphql", self.config.api_url),
        }
    }

    /// Runs one GraphQL mutation on the PR `node_id`; a 200 with `errors` is
    /// a rejection too.
    async fn mutate(&self, number: u64, query: &str, node_id: &str) -> Result<(), CodeHostError> {
        let builder = self
            .request(Method::POST, &self.graphql_url())
            .json(&json!({"query": query, "variables": {"id": node_id}}));
        let response = http::ok(self.send(builder).await?, Some(number)).await?;
        let value: serde_json::Value = http::json(response).await?;
        match value.get("errors") {
            Some(errors) if errors.as_array().is_some_and(|e| !e.is_empty()) => {
                Err(CodeHostError::Rejected {
                    status: 200,
                    body: errors.to_string(),
                })
            }
            _ => Ok(()),
        }
    }

    /// Before an existing PR is updated after a force-push: turns auto-merge
    /// off and, for a draft request, converts a ready PR back to draft.
    /// Returns whether the PR is a draft afterwards.
    async fn hold_for_review(&self, existing: &Pull, draft: bool) -> Result<bool, CodeHostError> {
        let node_id = existing.node_id.as_deref().unwrap_or_default();
        let needs_node = existing.auto_merge.is_some() || (draft && !existing.draft);
        if needs_node && node_id.is_empty() {
            return Err(CodeHostError::Rejected {
                status: 200,
                body: format!("pull request {} has no node_id", existing.number),
            });
        }
        if existing.auto_merge.is_some() {
            self.mutate(existing.number, DISABLE_AUTO_MERGE, node_id)
                .await?;
        }
        if draft && !existing.draft {
            self.mutate(existing.number, CONVERT_TO_DRAFT, node_id)
                .await?;
            return Ok(true);
        }
        Ok(existing.draft)
    }

    /// Holds an existing PR for review, then updates its title and body.
    async fn republish(
        &self,
        existing: &Pull,
        pr: &AgentPullRequest,
        body: &str,
    ) -> Result<Pull, CodeHostError> {
        let draft = self.hold_for_review(existing, pr.draft).await?;
        let mut pull = self.update(existing.number, &pr.title, body).await?;
        // The PATCH response may predate the conversion.
        pull.draft |= draft;
        Ok(pull)
    }

    async fn update(&self, number: u64, title: &str, body: &str) -> Result<Pull, CodeHostError> {
        let builder = self
            .request(Method::PATCH, &self.url(&format!("/pulls/{number}")))
            .json(&json!({"title": title, "body": body}));
        http::json(http::ok(self.send(builder).await?, Some(number)).await?).await
    }

    async fn ensure_labels(&self, number: u64, labels: &[String]) -> Result<(), CodeHostError> {
        for label in labels {
            let (color, description) = if label == AGENT_LABEL {
                (AGENT_LABEL_COLOR, AGENT_LABEL_DESCRIPTION)
            } else {
                (OTHER_LABEL_COLOR, "")
            };
            let builder = self
                .request(Method::POST, &self.url("/labels"))
                .json(&json!({
                    "name": label, "color": color, "description": description
                }));
            let response = self.send(builder).await?;
            // 422 = the label already exists (or is otherwise invalid; adding
            // it below then reports the real error).
            if response.status() != StatusCode::UNPROCESSABLE_ENTITY {
                http::ok(response, None).await?;
            }
        }
        let builder = self
            .request(Method::POST, &self.url(&format!("/issues/{number}/labels")))
            .json(&json!({"labels": labels}));
        http::ok(self.send(builder).await?, Some(number)).await?;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Repository {
    default_branch: String,
}

#[derive(Deserialize)]
struct Pull {
    number: u64,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    node_id: Option<String>,
    /// Set while auto-merge is enabled.
    #[serde(default)]
    auto_merge: Option<serde_json::Value>,
}

const DISABLE_AUTO_MERGE: &str = "mutation($id: ID!) { disablePullRequestAutoMerge(input: {pullRequestId: $id}) { pullRequest { number } } }";
const CONVERT_TO_DRAFT: &str = "mutation($id: ID!) { convertPullRequestToDraft(input: {pullRequestId: $id}) { pullRequest { number isDraft } } }";

#[async_trait::async_trait]
impl PullRequestPublisher for GithubPublisher {
    fn provider(&self) -> CodeHostProvider {
        CodeHostProvider::Github
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
        let (pull, created) = match self.find_open(&branch).await? {
            Some(existing) => (self.republish(&existing, pr, &body).await?, false),
            None => {
                let base = self.base_branch(pr).await?;
                let builder = self
                    .request(Method::POST, &self.url("/pulls"))
                    .json(&json!({
                        "title": pr.title,
                        "head": branch,
                        "base": base,
                        "body": body,
                        "draft": pr.draft,
                        "maintainer_can_modify": false,
                    }));
                let response = self.send(builder).await?;
                if response.status() == StatusCode::UNPROCESSABLE_ENTITY {
                    let error = http::error_for(response, None).await;
                    let raced = matches!(&error, CodeHostError::Rejected { body, .. }
                        if body.contains("already exists"));
                    // A concurrent publish created it between find and create.
                    let existing = if raced {
                        self.find_open(&branch).await?
                    } else {
                        None
                    };
                    match existing {
                        Some(existing) => (self.republish(&existing, pr, &body).await?, false),
                        None => return Err(error),
                    }
                } else {
                    (http::json(http::ok(response, None).await?).await?, true)
                }
            }
        };
        self.ensure_labels(pull.number, &labels).await?;
        Ok(OpenedPullRequest {
            provider: CodeHostProvider::Github,
            number: pull.number,
            url: pull.html_url,
            created,
            draft: pull.draft,
            labels,
        })
    }
}
