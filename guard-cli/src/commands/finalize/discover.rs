//! Context discovery of `guard gate finalize` (finalize-phase2.md, step 2).
//! Every function reads the environment through `Env`, so tests inject it.

use guard_core::config::TargetConfig;
use guard_core::ports::{CodeHostProvider, PrContext};
use serde_json::Value;

/// Environment lookup; empty values count as unset.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The GitHub event JSON at `GITHUB_EVENT_PATH`, if any.
pub fn github_event(env: Env<'_>) -> Option<Value> {
    let path = env("GITHUB_EVENT_PATH")?;
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// `--pr`, else `GUARD_PR_NUMBER`, else `pull_request.number` of the GitHub
/// event, else `CI_MERGE_REQUEST_IID`.
pub fn pr_number(flag: Option<u64>, env: Env<'_>, event: Option<&Value>) -> Option<u64> {
    let positive = |n: u64| (n > 0).then_some(n);
    flag.and_then(positive)
        .or_else(|| {
            env("GUARD_PR_NUMBER")?
                .trim()
                .parse()
                .ok()
                .and_then(positive)
        })
        .or_else(|| event?["pull_request"]["number"].as_u64().and_then(positive))
        .or_else(|| {
            env("CI_MERGE_REQUEST_IID")?
                .trim()
                .parse()
                .ok()
                .and_then(positive)
        })
}

/// `--base`, `GUARD_BASE_SHA`, `guard_base(GUARD_BASE)` (its merge base
/// with HEAD, the base `guard test --affected` selected from), `pr.base_sha`,
/// the GitHub event's `pull_request.base.sha`,
/// `CI_MERGE_REQUEST_DIFF_BASE_SHA`, then `merge_base()` (the fallback).
pub fn base_sha(
    flag: Option<&str>,
    env: Env<'_>,
    pr: Option<&PrContext>,
    event: Option<&Value>,
    guard_base: impl FnOnce(&str) -> Option<String>,
    merge_base: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let non_empty = |s: String| (!s.trim().is_empty()).then(|| s.trim().to_string());
    flag.map(str::to_string)
        .and_then(non_empty)
        .or_else(|| env("GUARD_BASE_SHA").and_then(non_empty))
        .or_else(|| {
            let reference = env("GUARD_BASE").and_then(non_empty)?;
            guard_base(&reference)
        })
        .or_else(|| pr.map(|p| p.base_sha.clone()).and_then(non_empty))
        .or_else(|| {
            event?["pull_request"]["base"]["sha"]
                .as_str()
                .map(str::to_string)
                .and_then(non_empty)
        })
        .or_else(|| env("CI_MERGE_REQUEST_DIFF_BASE_SHA").and_then(non_empty))
        .or_else(merge_base)
}

/// The commit the gate reports on: `pr.head_sha`, the GitHub event's
/// `pull_request.head.sha`, `CI_COMMIT_SHA`, then `git_head()` (`git rev-parse
/// HEAD`). On GitHub `pull_request` runs HEAD is the synthetic merge commit,
/// which branch protection never sees, so it comes last.
pub fn head_sha(
    pr: Option<&PrContext>,
    env: Env<'_>,
    event: Option<&Value>,
    git_head: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let non_empty = |s: String| (!s.trim().is_empty()).then(|| s.trim().to_string());
    pr.map(|p| p.head_sha.clone())
        .and_then(non_empty)
        .or_else(|| {
            event?["pull_request"]["head"]["sha"]
                .as_str()
                .map(str::to_string)
                .and_then(non_empty)
        })
        .or_else(|| env("CI_COMMIT_SHA").and_then(non_empty))
        .or_else(|| git_head().and_then(non_empty))
}

/// `GITHUB_HEAD_REF`, `CI_MERGE_REQUEST_SOURCE_BRANCH_NAME`, else the git branch.
pub fn branch(env: Env<'_>, git_branch: Option<String>) -> Option<String> {
    env("GITHUB_HEAD_REF")
        .or_else(|| env("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME"))
        .or(git_branch)
}

/// `GITHUB_SERVER_URL/GITHUB_REPOSITORY/actions/runs/GITHUB_RUN_ID`, else `CI_PIPELINE_URL`.
pub fn ci_url(env: Env<'_>) -> Option<String> {
    match (
        env("GITHUB_SERVER_URL"),
        env("GITHUB_REPOSITORY"),
        env("GITHUB_RUN_ID"),
    ) {
        (Some(server), Some(repo), Some(run)) => Some(format!(
            "{}/{repo}/actions/runs/{run}",
            server.trim_end_matches('/')
        )),
        _ => env("CI_PIPELINE_URL"),
    }
}

/// The repository name used in evidence keys and the report: the PR's repo,
/// `GITHUB_REPOSITORY`, `CI_PROJECT_PATH`, else `[app].name`.
pub fn repo_name(pr: Option<&PrContext>, env: Env<'_>, config: &TargetConfig) -> String {
    pr.map(|p| p.repo.clone())
        .filter(|r| !r.is_empty())
        .or_else(|| env("GITHUB_REPOSITORY"))
        .or_else(|| env("CI_PROJECT_PATH"))
        .unwrap_or_else(|| config.app.name.clone())
}

pub fn provider(config: &TargetConfig) -> Option<CodeHostProvider> {
    config.code_host.as_ref().map(|c| match c.kind {
        guard_core::config::CodeHostKind::Github => CodeHostProvider::Github,
        guard_core::config::CodeHostKind::Gitlab => CodeHostProvider::Gitlab,
    })
}

/// Code host credentials for the PR comment and the status
/// (`GITHUB_TOKEN` + `GITHUB_REPOSITORY`, or `GITLAB_TOKEN` + `CI_PROJECT_ID`/`GITLAB_PROJECT`).
pub fn has_code_host_credentials(config: &TargetConfig, env: Env<'_>) -> bool {
    match provider(config) {
        Some(CodeHostProvider::Github) => {
            env("GITHUB_TOKEN").is_some() && env("GITHUB_REPOSITORY").is_some()
        }
        Some(CodeHostProvider::Gitlab) => {
            env("GITLAB_TOKEN").is_some()
                && (env("CI_PROJECT_ID").is_some() || env("GITLAB_PROJECT").is_some())
        }
        None => false,
    }
}

/// Jira credentials for direct reporting.
pub fn has_jira_credentials(env: Env<'_>) -> bool {
    ["JIRA_BASE_URL", "JIRA_EMAIL", "JIRA_API_TOKEN"]
        .iter()
        .all(|v| env(v).is_some())
}

/// Authors exempt from `require_ticket = "block"`.
pub fn is_bot(author: Option<&str>) -> bool {
    matches!(
        author,
        Some("dependabot[bot]" | "renovate[bot]" | "renovate-bot")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned().filter(|v| !v.trim().is_empty())
    }

    fn pr(base: &str) -> PrContext {
        PrContext {
            provider: CodeHostProvider::Github,
            repo: "acme/shop".into(),
            number: 12,
            title: "t".into(),
            url: "u".into(),
            author: "dev".into(),
            ticket_key: None,
            branch: "b".into(),
            base_branch: "main".into(),
            base_sha: base.into(),
            head_sha: "h".into(),
            changed_files: vec![],
        }
    }

    #[test]
    fn pr_number_discovery_order() {
        let event = serde_json::json!({"pull_request": {"number": 7, "base": {"sha": "evt"}}});
        let all = env_of(&[("GUARD_PR_NUMBER", "5"), ("CI_MERGE_REQUEST_IID", "9")]);
        assert_eq!(pr_number(Some(3), &all, Some(&event)), Some(3));
        assert_eq!(pr_number(None, &all, Some(&event)), Some(5));
        let gitlab = env_of(&[("CI_MERGE_REQUEST_IID", "9")]);
        assert_eq!(pr_number(None, &gitlab, Some(&event)), Some(7));
        assert_eq!(pr_number(None, &gitlab, None), Some(9));
        let junk = env_of(&[("GUARD_PR_NUMBER", "x")]);
        assert_eq!(pr_number(None, &junk, None), None);
        assert_eq!(pr_number(Some(0), &env_of(&[]), None), None);
    }

    #[test]
    fn base_sha_discovery_order() {
        let event = serde_json::json!({"pull_request": {"base": {"sha": "evt"}}});
        let env = env_of(&[
            ("GUARD_BASE_SHA", "envsha"),
            ("CI_MERGE_REQUEST_DIFF_BASE_SHA", "gl"),
        ]);
        let none = || None;
        assert_eq!(
            base_sha(
                Some("flag"),
                &env,
                Some(&pr("p")),
                Some(&event),
                |_| None,
                none
            )
            .as_deref(),
            Some("flag")
        );
        assert_eq!(
            base_sha(None, &env, Some(&pr("p")), Some(&event), |_| None, none).as_deref(),
            Some("envsha")
        );
        let gl = env_of(&[("CI_MERGE_REQUEST_DIFF_BASE_SHA", "gl")]);
        assert_eq!(
            base_sha(None, &gl, Some(&pr("p")), Some(&event), |_| None, none).as_deref(),
            Some("p")
        );
        assert_eq!(
            base_sha(None, &gl, Some(&pr("")), Some(&event), |_| None, none).as_deref(),
            Some("evt")
        );
        assert_eq!(
            base_sha(None, &gl, None, None, |_| None, none).as_deref(),
            Some("gl")
        );
        assert_eq!(
            base_sha(
                None,
                &env_of(&[]),
                None,
                None,
                |_| None,
                || Some("mb".into())
            )
            .as_deref(),
            Some("mb")
        );
        assert_eq!(
            base_sha(None, &env_of(&[]), None, None, |_| None, none),
            None
        );
    }

    #[test]
    fn guard_base_comes_after_guard_base_sha_and_before_the_pr() {
        let event = serde_json::json!({"pull_request": {"base": {"sha": "evt"}}});
        let both = env_of(&[
            ("GUARD_BASE_SHA", "envsha"),
            ("GUARD_BASE", "origin/develop"),
        ]);
        let mb = |r: &str| Some(format!("mb({r})"));
        let none = || None;
        assert_eq!(
            base_sha(None, &both, Some(&pr("p")), Some(&event), mb, none).as_deref(),
            Some("envsha")
        );
        let env = env_of(&[("GUARD_BASE", " origin/develop ")]);
        assert_eq!(
            base_sha(Some("flag"), &env, None, None, mb, none).as_deref(),
            Some("flag")
        );
        assert_eq!(
            base_sha(None, &env, Some(&pr("p")), Some(&event), mb, none).as_deref(),
            Some("mb(origin/develop)")
        );
        // No usable merge base: the next candidates still apply.
        assert_eq!(
            base_sha(None, &env, Some(&pr("p")), None, |_| None, none).as_deref(),
            Some("p")
        );
    }

    #[test]
    fn head_sha_prefers_the_pr_head_over_the_merge_commit() {
        let event = serde_json::json!({"pull_request": {"head": {"sha": "evthead"}}});
        let env = env_of(&[("CI_COMMIT_SHA", "glsha")]);
        let git = || Some("merge".to_string());
        assert_eq!(
            head_sha(Some(&pr("p")), &env, Some(&event), git).as_deref(),
            Some("h")
        );
        let mut no_head = pr("p");
        no_head.head_sha = " ".into();
        assert_eq!(
            head_sha(Some(&no_head), &env, Some(&event), git).as_deref(),
            Some("evthead")
        );
        assert_eq!(head_sha(None, &env, None, git).as_deref(), Some("glsha"));
        assert_eq!(
            head_sha(None, &env_of(&[]), None, git).as_deref(),
            Some("merge")
        );
        assert_eq!(head_sha(None, &env_of(&[]), None, || None), None);
    }

    #[test]
    fn ci_url_and_branch() {
        let gh = env_of(&[
            ("GITHUB_SERVER_URL", "https://github.com/"),
            ("GITHUB_REPOSITORY", "acme/shop"),
            ("GITHUB_RUN_ID", "42"),
            ("GITHUB_HEAD_REF", "SHOP-1-x"),
        ]);
        assert_eq!(
            ci_url(&gh).as_deref(),
            Some("https://github.com/acme/shop/actions/runs/42")
        );
        assert_eq!(
            branch(&gh, Some("local".into())).as_deref(),
            Some("SHOP-1-x")
        );
        let gl = env_of(&[("CI_PIPELINE_URL", "https://gl/p/1")]);
        assert_eq!(ci_url(&gl).as_deref(), Some("https://gl/p/1"));
        assert_eq!(branch(&gl, Some("local".into())).as_deref(), Some("local"));
    }

    #[test]
    fn bots_are_exempt_from_require_ticket() {
        assert!(is_bot(Some("dependabot[bot]")));
        assert!(is_bot(Some("renovate-bot")));
        assert!(!is_bot(Some("dev")));
        assert!(!is_bot(None));
    }
}
