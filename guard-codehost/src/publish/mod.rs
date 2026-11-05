//! Agent PR publishing: `guard_core::ports::code_host::PullRequestPublisher`
//! for GitHub ([`github`]) and GitLab ([`gitlab`]).
//!
//! Contract: `docs/contracts/pr-publisher.md`. Both adapters push
//! `agent/<KEY>` with `git push` over HTTPS (token from the env var named by
//! `GUARD_PR_TOKEN_ENV`, passed to git through `GIT_CONFIG_*` environment
//! variables, never argv), then open or update a draft PR / MR labelled
//! `agent`. Neither ever merges nor enables auto-merge.

mod git;
pub mod github;
pub mod gitlab;

use crate::http::truncate_chars;
use guard_core::config::{CodeHostKind, TargetConfig};
use guard_core::ports::CodeHostError;
use guard_core::ports::code_host::{
    AgentPullRequest, BranchPush, OpenedPullRequest, PullRequestPublisher, PushedBranch,
};
use std::sync::Arc;

/// Env var naming the variable that holds the push / API token.
pub const TOKEN_ENV_VAR: &str = "GUARD_PR_TOKEN_ENV";
/// Remote URL to push to (default derived from the API URL and repository).
pub const PUSH_URL_VAR: &str = "GUARD_PR_PUSH_URL";
/// Target branch (default: the repository default branch).
pub const BASE_BRANCH_VAR: &str = "GUARD_PR_BASE_BRANCH";
/// Extra comma-separated labels added to every agent PR.
pub const LABELS_VAR: &str = "GUARD_PR_LABELS";

/// First line of every agent PR body; identifies the ticket.
pub fn marker(ticket: &str) -> String {
    format!("<!-- guard:agent-pr {ticket} -->")
}

/// Longest summary kept in the body (GitHub caps PR bodies at 65 536 chars).
const MAX_SUMMARY_CHARS: usize = 50_000;

/// The PR / MR body: marker, approval notice, summary and a table with the
/// ticket key, branch, base commit, sealed spec path and sha256, attempts,
/// cost and coder.
pub fn render_body(pr: &AgentPullRequest) -> String {
    let mut out = format!(
        "{}\n**Agent PR for `{}`** (lane A). Human approval required: guard never merges agent PRs.\n\n",
        marker(&pr.ticket),
        cell(&pr.ticket)
    );
    let summary = pr.summary.trim();
    if !summary.is_empty() {
        out.push_str(&truncate_chars(summary, MAX_SUMMARY_CHARS));
        out.push_str("\n\n");
    }
    out.push_str("| Field | Value |\n| --- | --- |\n");
    let mut row = |k: &str, v: String| out.push_str(&format!("| {k} | {v} |\n"));
    row("Ticket", format!("`{}`", cell(&pr.ticket)));
    row("Branch", format!("`{}`", cell(&pr.branch())));
    if !pr.base_commit.trim().is_empty() {
        row("Base commit", format!("`{}`", cell(&pr.base_commit)));
    }
    row("Sealed spec", format!("`{}`", cell(&pr.sealed_spec.path)));
    row(
        "Sealed spec sha256",
        format!("`{}`", cell(&pr.sealed_spec.sha256)),
    );
    row("Attempts", pr.attempts.to_string());
    row("Cost", format!("${:.4}", pr.cost_usd));
    if let Some(coder) = pr.coder.as_deref().filter(|c| !c.trim().is_empty()) {
        row("Coder", cell(coder));
    }
    out
}

/// A Markdown table cell: no newlines, pipes and backticks escaped.
fn cell(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
        .replace('|', "\\|")
        .replace('`', "'")
}

/// Pushes the branch, then opens or updates its PR. `push.ticket` and
/// `pr.ticket` must match.
pub async fn publish_agent_pr(
    publisher: &dyn PullRequestPublisher,
    push: &BranchPush,
    pr: &AgentPullRequest,
) -> Result<(PushedBranch, OpenedPullRequest), CodeHostError> {
    if push.ticket != pr.ticket {
        return Err(CodeHostError::Rejected {
            status: 0,
            body: format!(
                "push ticket {:?} does not match PR ticket {:?}",
                push.ticket, pr.ticket
            ),
        });
    }
    push.validate()?;
    pr.validate()?;
    let pushed = publisher.push_branch(push).await?;
    let opened = publisher.open_pull_request(pr).await?;
    Ok((pushed, opened))
}

/// Builds the publisher for `[code_host].kind` from the process environment.
pub fn from_env(kind: CodeHostKind) -> Result<Arc<dyn PullRequestPublisher>, CodeHostError> {
    from_lookup(kind, |key| std::env::var(key).ok())
}

/// The publisher for `target.toml`'s `[code_host]` (required) from the
/// process environment.
pub fn from_target_env(
    target: &TargetConfig,
) -> Result<Arc<dyn PullRequestPublisher>, CodeHostError> {
    let kind = target
        .code_host
        .as_ref()
        .map(|c| c.kind)
        .ok_or_else(|| CodeHostError::Unconfigured("target.toml has no [code_host]".into()))?;
    from_env(kind)
}

/// Like [`from_env`] with an injectable variable lookup.
pub fn from_lookup(
    kind: CodeHostKind,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn PullRequestPublisher>, CodeHostError> {
    Ok(match kind {
        CodeHostKind::Github => Arc::new(github::GithubPublisher::new(
            github::GithubPublisherConfig::from_lookup(lookup)?,
        )?),
        CodeHostKind::Gitlab => Arc::new(gitlab::GitlabPublisher::new(
            gitlab::GitlabPublisherConfig::from_lookup(lookup)?,
        )?),
    })
}

/// Settings both providers read the same way.
pub(crate) struct CommonEnv {
    pub token: String,
    pub push_url: Option<String>,
    pub base_branch: Option<String>,
    pub labels: Vec<String>,
}

pub(crate) fn common_env(
    lookup: &impl Fn(&str) -> Option<String>,
    default_token_env: &str,
) -> Result<CommonEnv, CodeHostError> {
    let get = |key: &str| {
        lookup(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let token_env = get(TOKEN_ENV_VAR).unwrap_or_else(|| default_token_env.to_string());
    let valid_name = token_env
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !token_env.as_bytes()[0].is_ascii_digit();
    if !valid_name {
        return Err(CodeHostError::Unconfigured(format!(
            "{TOKEN_ENV_VAR} must name an environment variable, got {token_env:?}"
        )));
    }
    let token = get(&token_env)
        .ok_or_else(|| CodeHostError::Unconfigured(format!("{token_env} is not set")))?;
    let push_url = get(PUSH_URL_VAR);
    if let Some(url) = &push_url {
        git::validate_push_url(url)?;
    }
    Ok(CommonEnv {
        token,
        push_url,
        base_branch: get(BASE_BRANCH_VAR),
        labels: get(LABELS_VAR)
            .map(|v| split_labels(&v))
            .unwrap_or_default(),
    })
}

pub(crate) fn split_labels(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn rejected_locally(body: String) -> CodeHostError {
    CodeHostError::Rejected { status: 0, body }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::ports::SealedSpec;

    #[test]
    fn body_escapes_table_cells_and_omits_empty_rows() {
        let mut pr = AgentPullRequest::new("SHOP-1", "t", SealedSpec::seal("a|b.spec.ts", b"x"));
        pr.coder = Some("coder\n| `x`".into());
        let body = render_body(&pr);
        assert!(body.starts_with("<!-- guard:agent-pr SHOP-1 -->\n"));
        assert!(body.contains("| Sealed spec | `a\\|b.spec.ts` |"));
        assert!(body.contains("| Coder | coder \\| 'x' |"));
        assert!(!body.contains("Base commit"));
        assert!(body.contains("| Cost | $0.0000 |"));
    }

    #[test]
    fn labels_split_on_commas() {
        assert_eq!(split_labels(" a, ,b,"), vec!["a", "b"]);
    }

    #[test]
    fn from_lookup_builds_both_providers() {
        let env = |k: &str| match k {
            "GITHUB_TOKEN" | "GITLAB_TOKEN" => Some("t".to_string()),
            "GITHUB_REPOSITORY" | "GITLAB_PROJECT" => Some("acme/shop".to_string()),
            _ => None,
        };
        use guard_core::ports::CodeHostProvider;
        assert_eq!(
            from_lookup(CodeHostKind::Github, env).unwrap().provider(),
            CodeHostProvider::Github
        );
        assert_eq!(
            from_lookup(CodeHostKind::Gitlab, env).unwrap().provider(),
            CodeHostProvider::Gitlab
        );
    }
}
