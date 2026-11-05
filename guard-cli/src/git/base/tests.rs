use super::*;
use std::collections::HashMap;

fn g(dir: &Path, args: &[&str]) -> String {
    let mut all = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "protocol.file.allow=always",
    ];
    all.extend_from_slice(args);
    super::super::text(dir, &all).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A repository on branch `branch` with one commit.
fn repo_on(branch: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    g(dir.path(), &["init", "-q", "-b", branch]);
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    g(dir.path(), &["add", "-A"]);
    g(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

/// A clone of a repository whose default branch is `default`; the clone is
/// left on a feature branch and has no local branch named `default`.
fn clone_of(default: &str) -> (tempfile::TempDir, tempfile::TempDir) {
    let origin = repo_on(default);
    let parent = tempfile::TempDir::new().unwrap();
    let clone = parent.path().join("c");
    g(
        parent.path(),
        &["clone", "-q", &slash(origin.path()), &slash(&clone)],
    );
    g(&clone, &["checkout", "-q", "-b", "feature"]);
    g(&clone, &["branch", "-q", "-D", default]);
    (origin, parent)
}

fn clone_root(parent: &tempfile::TempDir) -> PathBuf {
    parent.path().join("c")
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |k| map.get(k).cloned().filter(|v| !v.trim().is_empty())
}

fn no_env() -> impl Fn(&str) -> Option<String> + use<> {
    env_of(&[])
}

fn resolve(
    root: &Path,
    explicit: Option<&str>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResolvedBase, BaseError> {
    let mut warnings = Vec::new();
    let result = resolve_base(root, explicit, env, &mut |w| warnings.push(w));
    assert!(warnings.is_empty(), "{warnings:?}");
    result
}

fn found(root: &Path, env: &dyn Fn(&str) -> Option<String>) -> (String, BaseSource) {
    let base = resolve(root, None, env).unwrap();
    assert_eq!(base.sha.len(), 40);
    (base.reference, base.source)
}

#[test]
fn explicit_base_wins_and_unknown_explicit_is_error_with_hint() {
    let dir = repo_on("main");
    let root = dir.path();
    g(root, &["checkout", "-q", "-b", "feature"]);
    let base = resolve(root, Some("feature"), &no_env()).unwrap();
    assert_eq!(
        (base.reference.as_str(), base.source),
        ("feature", BaseSource::Flag)
    );
    match resolve(root, Some("nope"), &no_env()) {
        Err(BaseError::Unknown { reference, hint }) => {
            assert_eq!(reference, "nope");
            assert_eq!(hint.as_deref(), Some("main"));
        }
        other => panic!("{other:?}"),
    }
    // GUARD_BASE never replaces an explicit base.
    let env = env_of(&[("GUARD_BASE", "main")]);
    assert!(matches!(
        resolve(root, Some("nope"), &env),
        Err(BaseError::Unknown { .. })
    ));
}

#[test]
fn guard_base_env_is_strict() {
    let dir = repo_on("main");
    let root = dir.path();
    let env = env_of(&[("GUARD_BASE", "main")]);
    assert_eq!(found(root, &env), ("main".into(), BaseSource::Env));
    let env = env_of(&[("GUARD_BASE", "gone")]);
    assert!(matches!(
        resolve(root, None, &env),
        Err(BaseError::Unknown { reference, .. }) if reference == "gone"
    ));
}

#[test]
fn remote_head_trunk_when_no_local_main() {
    let (_origin, parent) = clone_of("claude/x");
    let root = clone_root(&parent);
    assert_eq!(
        found(&root, &no_env()),
        ("origin/claude/x".into(), BaseSource::RemoteHead)
    );
}

#[test]
fn remote_main_when_only_tracking_ref() {
    let (_origin, parent) = clone_of("main");
    let root = clone_root(&parent);
    g(
        &root,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    assert_eq!(
        found(&root, &no_env()),
        ("origin/main".into(), BaseSource::RemoteBranch)
    );
}

#[test]
fn unset_origin_head_falls_to_remote_main_then_local() {
    let (_origin, parent) = clone_of("trunk");
    let root = clone_root(&parent);
    g(
        &root,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    g(&root, &["config", "init.defaultBranch", "main"]);
    // No origin/HEAD, no origin/main or master: the local candidates.
    g(&root, &["branch", "-q", "trunk", "origin/trunk"]);
    assert_eq!(
        found(&root, &no_env()),
        ("trunk".into(), BaseSource::LocalBranch)
    );
    g(&root, &["branch", "-q", "master", "trunk"]);
    assert_eq!(
        found(&root, &no_env()),
        ("master".into(), BaseSource::LocalBranch)
    );
    g(&root, &["config", "init.defaultBranch", "trunk"]);
    assert_eq!(
        found(&root, &no_env()),
        ("trunk".into(), BaseSource::LocalBranch)
    );
}

#[test]
fn non_origin_remote_is_tried() {
    let (_origin, parent) = clone_of("dev");
    let root = clone_root(&parent);
    g(&root, &["remote", "rename", "origin", "upstream"]);
    assert_eq!(candidate_remotes(&root), vec!["upstream".to_string()]);
    assert_eq!(
        found(&root, &no_env()),
        ("upstream/dev".into(), BaseSource::RemoteHead)
    );
}

#[test]
fn candidate_remotes_are_origin_then_branch_remote_then_sole_remote() {
    let dir = repo_on("main");
    let root = dir.path();
    assert!(candidate_remotes(root).is_empty());
    g(
        root,
        &["remote", "add", "fork", "https://example.invalid/f.git"],
    );
    assert_eq!(candidate_remotes(root), ["fork"]);
    g(
        root,
        &["remote", "add", "origin", "https://example.invalid/o.git"],
    );
    g(
        root,
        &["remote", "add", "other", "https://example.invalid/x.git"],
    );
    assert_eq!(candidate_remotes(root), ["origin"]);
    g(root, &["config", "branch.main.remote", "other"]);
    assert_eq!(candidate_remotes(root), ["origin", "other"]);
    g(root, &["config", "branch.main.remote", "origin"]);
    assert_eq!(candidate_remotes(root), ["origin"]);
}

#[test]
fn dangling_origin_head_is_skipped() {
    let (_origin, parent) = clone_of("main");
    let root = clone_root(&parent);
    g(
        &root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/deleted",
        ],
    );
    assert_eq!(
        found(&root, &no_env()),
        ("origin/main".into(), BaseSource::RemoteBranch)
    );
}

#[test]
fn github_base_ref_and_event_sha_used_and_skipped_when_absent() {
    let (_origin, parent) = clone_of("main");
    let root = clone_root(&parent);
    g(
        &root,
        &["update-ref", "refs/remotes/origin/release", "HEAD"],
    );
    let env = env_of(&[("GITHUB_BASE_REF", "release")]);
    assert_eq!(
        found(&root, &env),
        ("origin/release".into(), BaseSource::Ci)
    );
    // A base ref the clone does not have is skipped with a warning; the event sha follows.
    let sha = g(&root, &["rev-parse", "HEAD"]);
    let event = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        event.path(),
        format!(r#"{{"pull_request": {{"base": {{"sha": "{sha}"}}}}}}"#),
    )
    .unwrap();
    let path = event.path().to_string_lossy().into_owned();
    let env = env_of(&[("GITHUB_BASE_REF", "gone"), ("GITHUB_EVENT_PATH", &path)]);
    let mut warnings = Vec::new();
    let base = resolve_base(&root, None, &env, &mut |w| warnings.push(w)).unwrap();
    assert_eq!(
        (base.reference.as_str(), base.source),
        (sha.as_str(), BaseSource::Ci)
    );
    assert_eq!(
        warnings,
        ["base candidate origin/gone from GITHUB_BASE_REF is not a commit of this clone"]
    );
    // Neither resolves: on to the remote default, with both warnings.
    std::fs::write(
        event.path(),
        r#"{"pull_request": {"base": {"sha": "1234567890123456789012345678901234567890"}}}"#,
    )
    .unwrap();
    let mut warnings = Vec::new();
    let base = resolve_base(&root, None, &env, &mut |w| warnings.push(w)).unwrap();
    assert_eq!(base.source, BaseSource::RemoteHead);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[1].contains("from GITHUB_EVENT_PATH"),
        "{warnings:?}"
    );
}

#[test]
fn gitlab_diff_base_sha_then_target_branch() {
    let (_origin, parent) = clone_of("main");
    let root = clone_root(&parent);
    let sha = g(&root, &["rev-parse", "HEAD"]);
    let env = env_of(&[
        ("CI_MERGE_REQUEST_DIFF_BASE_SHA", &sha),
        ("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main"),
    ]);
    assert_eq!(found(&root, &env), (sha.clone(), BaseSource::Ci));
    let env = env_of(&[("CI_MERGE_REQUEST_TARGET_BRANCH_NAME", "main")]);
    assert_eq!(found(&root, &env), ("origin/main".into(), BaseSource::Ci));
}

#[test]
fn nothing_found_lists_tried_candidates() {
    let dir = repo_on("feature");
    g(dir.path(), &["config", "init.defaultBranch", "main"]);
    match resolve(dir.path(), None, &no_env()) {
        Err(BaseError::NotFound { tried, .. }) => {
            assert_eq!(tried, ["main", "master", "trunk"]);
        }
        other => panic!("{other:?}"),
    }
    let (_origin, parent) = clone_of("x");
    let root = clone_root(&parent);
    g(
        &root,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );
    g(&root, &["config", "init.defaultBranch", "dev"]);
    match resolve(&root, None, &no_env()) {
        Err(BaseError::NotFound { tried, .. }) => assert_eq!(
            tried,
            [
                "origin/main",
                "origin/master",
                "dev",
                "main",
                "master",
                "trunk"
            ]
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn empty_and_dash_bases_rejected() {
    let dir = repo_on("main");
    for bad in ["", "  ", "-h", "--all"] {
        assert!(
            matches!(
                resolve(dir.path(), Some(bad), &no_env()),
                Err(BaseError::Unknown { .. })
            ),
            "{bad:?}"
        );
    }
}

#[test]
fn merge_base_checked_reports_shallow() {
    let origin = repo_on("main");
    std::fs::write(origin.path().join("a.txt"), "b\n").unwrap();
    g(origin.path(), &["commit", "-q", "-am", "second"]);
    let first = g(origin.path(), &["rev-parse", "HEAD~1"]);
    g(origin.path(), &["checkout", "-q", "-b", "side", &first]);
    std::fs::write(origin.path().join("s.txt"), "s\n").unwrap();
    g(origin.path(), &["add", "-A"]);
    g(origin.path(), &["commit", "-q", "-m", "side"]);
    g(origin.path(), &["checkout", "-q", "main"]);
    let parent = tempfile::TempDir::new().unwrap();
    let url = format!("file:///{}", slash(origin.path()).trim_start_matches('/'));
    g(
        parent.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            "--no-single-branch",
            &url,
            "c",
        ],
    );
    let root = parent.path().join("c");
    let side = g(&root, &["rev-parse", "origin/side"]);
    assert_eq!(
        merge_base_checked(&root, &side),
        Err(MergeBaseError::Shallow)
    );
}

#[test]
fn merge_base_checked_reports_unrelated() {
    let dir = repo_on("main");
    let root = dir.path();
    g(root, &["checkout", "-q", "--orphan", "other"]);
    g(root, &["commit", "-q", "-m", "orphan"]);
    let main = g(root, &["rev-parse", "main"]);
    assert_eq!(
        merge_base_checked(root, &main),
        Err(MergeBaseError::Unrelated)
    );
    let head = g(root, &["rev-parse", "HEAD"]);
    assert_eq!(merge_base_checked(root, &head), Ok(head));
}

#[test]
fn merge_base_checked_reports_no_commits() {
    let dir = tempfile::TempDir::new().unwrap();
    g(dir.path(), &["init", "-q", "-b", "main"]);
    assert!(!super::super::has_commits(dir.path()));
    assert_eq!(
        merge_base_checked(dir.path(), "1234567890123456789012345678901234567890"),
        Err(MergeBaseError::NoCommits)
    );
}

#[test]
fn detect_repo_default_uses_steps_four_to_six_only() {
    let dir = repo_on("main");
    let base = detect_repo_default(dir.path()).unwrap();
    assert_eq!(
        (base.reference.as_str(), base.source),
        ("main", BaseSource::LocalBranch)
    );
    assert!(detect_repo_default(repo_on("feature").path()).is_none());
}

#[test]
fn nothing_found_in_a_shallow_clone_says_so() {
    let origin = repo_on("main");
    g(origin.path(), &["checkout", "-q", "-b", "feature"]);
    std::fs::write(origin.path().join("f.txt"), "f\n").unwrap();
    g(origin.path(), &["add", "-A"]);
    g(origin.path(), &["commit", "-q", "-m", "feature"]);
    let url = format!("file:///{}", slash(origin.path()).trim_start_matches('/'));
    let clone = tempfile::TempDir::new().unwrap();
    let root = clone.path();
    g(root, &["init", "-q"]);
    g(root, &["remote", "add", "origin", &url]);
    g(root, &["fetch", "-q", "--depth", "1", "origin", "feature"]);
    g(root, &["checkout", "-q", "--detach", "origin/feature"]);
    let env = env_of(&[("GITHUB_BASE_REF", "main")]);
    let mut warnings = Vec::new();
    match resolve_base(root, None, &env, &mut |w| warnings.push(w)) {
        Err(BaseError::NotFound { shallow, .. }) => assert!(shallow),
        other => panic!("{other:?}"),
    }
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let dir = repo_on("feature");
    match resolve(dir.path(), None, &no_env()) {
        Err(BaseError::NotFound { shallow, .. }) => assert!(!shallow),
        other => panic!("{other:?}"),
    }
}
