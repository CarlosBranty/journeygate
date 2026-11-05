//! `CodeHost` adapters for GitHub and GitLab.
//!
//! Implements `guard_core::ports::CodeHost` twice: GitHub (check runs API,
//! issue comments, `X-Hub-Signature-256` HMAC-SHA256 webhooks) and GitLab
//! (commit status API, MR notes, `X-Gitlab-Token` webhooks). Comment and status
//! layouts follow `docs/contracts/reporting.md` ([`report`]).
//!
//! [`publish`] implements `guard_core::ports::code_host::PullRequestPublisher`
//! (push `agent/<KEY>` + open or update its draft PR / MR) for both hosts
//! (`docs/contracts/pr-publisher.md`).
//!
//! Both adapters take their base URL from configuration (`GITHUB_API_URL`,
//! `GITLAB_API_URL`), so tests drive them against a wiremock server.

pub mod github;
pub mod gitlab;
mod http;
pub mod publish;
pub mod report;
mod webhook;
pub mod whoami;

use guard_core::config::CodeHostKind;
use guard_core::ports::{CodeHost, CodeHostError};
use guard_core::ticket::Regex;
use std::sync::Arc;

/// Marker of the guard summary comment (reporting.md). The PR comment / MR
/// note containing it is edited on every run instead of posting a new one.
pub const PR_COMMENT_MARKER: &str = "<!-- guard:gate-summary -->";

/// `[tracker].ticket_pattern` applied to a PR: the first match in the branch
/// name, else in the title, else `None`.
pub fn ticket_key(pattern: Option<&Regex>, branch: &str, title: &str) -> Option<String> {
    let pattern = pattern?;
    guard_core::ticket::extract_ticket(pattern, Some(branch))
        .or_else(|| pattern.find(title).map(|m| m.as_str().to_string()))
}

/// Builds the adapter for `[code_host].kind` from the process environment
/// (variables in `docs/contracts/server-api.md`).
pub fn from_env(kind: CodeHostKind) -> Result<Arc<dyn CodeHost>, CodeHostError> {
    from_lookup(kind, |key| std::env::var(key).ok())
}

/// Like [`from_env`] with an injectable variable lookup.
pub fn from_lookup(
    kind: CodeHostKind,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Arc<dyn CodeHost>, CodeHostError> {
    Ok(match kind {
        CodeHostKind::Github => Arc::new(github::GithubCodeHost::new(
            github::GithubConfig::from_lookup(lookup)?,
        )?),
        CodeHostKind::Gitlab => Arc::new(gitlab::GitlabCodeHost::new(
            gitlab::GitlabConfig::from_lookup(lookup)?,
        )?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::ports::CodeHostProvider;

    #[test]
    fn ticket_key_prefers_the_branch_then_the_title() {
        let p = Regex::new(r"SHOP-\d+").unwrap();
        assert_eq!(
            ticket_key(Some(&p), "SHOP-1-x", "SHOP-2 y").as_deref(),
            Some("SHOP-1")
        );
        assert_eq!(
            ticket_key(Some(&p), "feature/x", "Fix SHOP-2 and SHOP-3").as_deref(),
            Some("SHOP-2")
        );
        assert_eq!(ticket_key(Some(&p), "feature/x", "no ticket"), None);
        assert_eq!(ticket_key(None, "SHOP-1", "SHOP-2"), None);
        // A detached HEAD has no ticket, but the title still can.
        assert_eq!(
            ticket_key(Some(&p), "HEAD", "SHOP-9").as_deref(),
            Some("SHOP-9")
        );
    }

    #[test]
    fn from_lookup_builds_the_configured_provider() {
        let env = |k: &str| match k {
            "GITHUB_TOKEN" | "GITLAB_TOKEN" => Some("t".to_string()),
            "GITHUB_REPOSITORY" | "GITLAB_PROJECT" => Some("acme/shop".to_string()),
            _ => None,
        };
        let gh = from_lookup(CodeHostKind::Github, env).unwrap();
        assert_eq!(gh.provider(), CodeHostProvider::Github);
        let gl = from_lookup(CodeHostKind::Gitlab, env).unwrap();
        assert_eq!(gl.provider(), CodeHostProvider::Gitlab);
        assert!(matches!(
            from_lookup(CodeHostKind::Github, |_| None),
            Err(CodeHostError::Unconfigured(_))
        ));
    }
}
