//! The base `--affected` compares against (cli.md "Base resolution"): an
//! explicit base is checked strictly; a missing one is detected in a fixed
//! order, and nothing found is an error naming every candidate tried.
//! guard never fetches and never runs `git remote set-head`.

use super::{GitError, resolve_commit, text};
use std::path::{Path, PathBuf};

pub use guard_mcp::types::BaseSource;

#[cfg(test)]
mod tests;

/// Environment lookup; empty values count as unset.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBase {
    /// The candidate that resolved, as written (`origin/trunk`, a sha).
    pub reference: String,
    pub sha: String,
    pub source: BaseSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseError {
    /// An explicit base (`--base`, `GUARD_BASE`) that is not a commit;
    /// `hint` is the detected default, when there is one.
    Unknown {
        reference: String,
        hint: Option<String>,
    },
    /// Nothing resolved; every candidate tried, in order. `shallow`: the
    /// clone is shallow, so the base was most likely never fetched.
    NotFound {
        tried: Vec<String>,
        shallow: bool,
    },
    Git(GitError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeBaseError {
    /// HEAD has no commit.
    NoCommits,
    /// No merge base in a shallow clone.
    Shallow,
    /// No merge base otherwise.
    Unrelated,
    Git(GitError),
}

/// A strict explicit base: non-empty, not an option, a commit of the clone.
fn explicit_base(root: &Path, reference: &str) -> Result<Option<String>, GitError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return Ok(None);
    }
    resolve_commit(root, trimmed)
}

fn resolved(reference: String, sha: String, source: BaseSource) -> ResolvedBase {
    ResolvedBase {
        reference,
        sha,
        source,
    }
}

/// Steps 1-7 of cli.md "Base resolution". `warn` receives each skipped CI
/// candidate (`base candidate <c> from <VAR> is not a commit of this clone`).
pub fn resolve_base(
    root: &Path,
    explicit: Option<&str>,
    env: Env<'_>,
    warn: &mut dyn FnMut(String),
) -> Result<ResolvedBase, BaseError> {
    let strict = |reference: &str, source: BaseSource| match explicit_base(root, reference) {
        Ok(Some(sha)) => Ok(resolved(reference.trim().to_string(), sha, source)),
        Ok(None) => Err(BaseError::Unknown {
            reference: reference.to_string(),
            hint: detect_repo_default(root).map(|d| d.reference),
        }),
        Err(e) => Err(BaseError::Git(e)),
    };
    if let Some(reference) = explicit {
        return strict(reference, BaseSource::Flag);
    }
    if let Some(reference) = env("GUARD_BASE") {
        return strict(&reference, BaseSource::Env);
    }
    let remotes = candidate_remotes(root);
    for (candidate, var) in ci_candidates(env, &remotes) {
        match resolve_commit(root, &candidate).map_err(BaseError::Git)? {
            Some(sha) => return Ok(resolved(candidate, sha, BaseSource::Ci)),
            None => warn(format!(
                "base candidate {candidate} from {var} is not a commit of this clone"
            )),
        }
    }
    let mut tried = Vec::new();
    match default_of(root, &remotes, &mut tried).map_err(BaseError::Git)? {
        Some(base) => Ok(base),
        None => Err(BaseError::NotFound {
            tried,
            shallow: super::is_shallow(root),
        }),
    }
}

/// CI candidates (step 3), each with the variable it came from.
fn ci_candidates(env: Env<'_>, remotes: &[String]) -> Vec<(String, &'static str)> {
    let mut out = Vec::new();
    if let Some(branch) = env("GITHUB_BASE_REF") {
        for remote in remotes {
            out.push((format!("{remote}/{}", branch.trim()), "GITHUB_BASE_REF"));
        }
    }
    if let Some(sha) = github_event_base_sha(env) {
        out.push((sha, "GITHUB_EVENT_PATH"));
    }
    if let Some(sha) = env("CI_MERGE_REQUEST_DIFF_BASE_SHA") {
        out.push((sha.trim().to_string(), "CI_MERGE_REQUEST_DIFF_BASE_SHA"));
    }
    if let Some(branch) = env("CI_MERGE_REQUEST_TARGET_BRANCH_NAME") {
        for remote in remotes {
            out.push((
                format!("{remote}/{}", branch.trim()),
                "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
            ));
        }
    }
    out
}

/// `pull_request.base.sha` of the GitHub event JSON at `GITHUB_EVENT_PATH`.
pub fn github_event_base_sha(env: Env<'_>) -> Option<String> {
    let path = PathBuf::from(env("GITHUB_EVENT_PATH")?);
    let event: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let sha = event["pull_request"]["base"]["sha"].as_str()?.trim();
    (!sha.is_empty()).then(|| sha.to_string())
}

/// Steps 4-6 only: the repository's own default base, without the
/// environment.
pub fn detect_repo_default(root: &Path) -> Option<ResolvedBase> {
    default_of(root, &candidate_remotes(root), &mut Vec::new())
        .ok()
        .flatten()
}

fn default_of(
    root: &Path,
    remotes: &[String],
    tried: &mut Vec<String>,
) -> Result<Option<ResolvedBase>, GitError> {
    let mut try_one =
        |candidate: String, source: BaseSource| -> Result<Option<ResolvedBase>, GitError> {
            if tried.contains(&candidate) {
                return Ok(None);
            }
            tried.push(candidate.clone());
            Ok(resolve_commit(root, &candidate)?.map(|sha| resolved(candidate, sha, source)))
        };
    // Step 4: each remote's HEAD target; a dangling one is skipped.
    for remote in remotes {
        let head = format!("refs/remotes/{remote}/HEAD");
        if let Ok(target) = text(root, &["symbolic-ref", "--quiet", "--short", &head]) {
            if !target.is_empty() {
                if let Some(base) = try_one(target, BaseSource::RemoteHead)? {
                    return Ok(Some(base));
                }
            }
        }
    }
    // Step 5.
    for remote in remotes {
        for branch in ["main", "master"] {
            if let Some(base) = try_one(format!("{remote}/{branch}"), BaseSource::RemoteBranch)? {
                return Ok(Some(base));
            }
        }
    }
    // Step 6.
    let configured = text(root, &["config", "--get", "init.defaultBranch"])
        .ok()
        .filter(|b| !b.is_empty());
    for branch in configured
        .into_iter()
        .chain(["main", "master", "trunk"].map(String::from))
    {
        if let Some(base) = try_one(branch, BaseSource::LocalBranch)? {
            return Ok(Some(base));
        }
    }
    Ok(None)
}

/// `origin`, then `branch.<current>.remote` (not for a detached HEAD), then
/// the only remote when there is exactly one; existing remotes only, no
/// duplicates.
pub fn candidate_remotes(root: &Path) -> Vec<String> {
    let all: Vec<String> = text(root, &["remote"])
        .map(|out| out.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if all.iter().any(|r| r == name) && !out.iter().any(|r| r == name) {
            out.push(name.to_string());
        }
    };
    add("origin");
    if let Ok(Some(branch)) = super::current_branch(root) {
        if let Ok(remote) = text(
            root,
            &["config", "--get", &format!("branch.{branch}.remote")],
        ) {
            add(&remote);
        }
    }
    if all.len() == 1 {
        add(&all[0]);
    }
    out
}

/// `git merge-base <base_sha> HEAD`, with the failure told apart.
pub fn merge_base_checked(root: &Path, base_sha: &str) -> Result<String, MergeBaseError> {
    if !super::has_commits(root) {
        return Err(MergeBaseError::NoCommits);
    }
    match super::merge_base_of(root, base_sha, "HEAD") {
        Ok(sha) if !sha.is_empty() => Ok(sha),
        Ok(_) | Err(GitError::Failed(_)) => Err(if super::is_shallow(root) {
            MergeBaseError::Shallow
        } else {
            MergeBaseError::Unrelated
        }),
        Err(e) => Err(MergeBaseError::Git(e)),
    }
}
