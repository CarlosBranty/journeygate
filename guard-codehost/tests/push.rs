//! `push_branch` against local bare repositories (no network).

use guard_codehost::publish::github::{GithubPublisher, GithubPublisherConfig};
use guard_codehost::publish::gitlab::{GitlabPublisher, GitlabPublisherConfig};
use guard_codehost::publish::publish_agent_pr;
use guard_core::ports::code_host::{AgentPullRequest, BranchPush, PullRequestPublisher};
use guard_core::ports::{CodeHostError, SealedSpec};
use serde_json::json;
use std::path::Path;
use std::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOKEN: &str = "ghs_secret_push_token";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A work repo with two commits and an empty bare "remote".
fn repos() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    let bare = tmp.path().join("remote.git");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "--quiet", "-b", "main"]);
    std::fs::write(work.join("a.txt"), "1\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "--quiet", "-m", "one"]);
    std::fs::write(work.join("a.txt"), "2\n").unwrap();
    git(&work, &["commit", "--quiet", "-am", "two"]);
    git(tmp.path(), &["init", "--quiet", "--bare", "remote.git"]);
    (tmp, work, bare)
}

fn remote_url(bare: &Path) -> String {
    bare.to_string_lossy().replace('\\', "/")
}

fn github(bare: &Path) -> GithubPublisher {
    GithubPublisher::new(
        GithubPublisherConfig::new("https://api.github.com", "acme/shop", TOKEN)
            .with_push_url(remote_url(bare)),
    )
    .unwrap()
}

fn push(work: &Path, rev: &str) -> BranchPush {
    BranchPush {
        local_repo: work.to_path_buf(),
        rev: rev.into(),
        ticket: "SHOP-1".into(),
    }
}

#[tokio::test]
async fn pushes_the_commit_to_the_agent_branch_and_force_updates_it() {
    let (_tmp, work, bare) = repos();
    let head = git(&work, &["rev-parse", "HEAD"]);
    let parent = git(&work, &["rev-parse", "HEAD~1"]);
    let publisher = github(&bare);

    let pushed = publisher.push_branch(&push(&work, "main")).await.unwrap();
    assert_eq!(pushed.branch, "agent/SHOP-1");
    assert_eq!(pushed.commit, head);
    assert_eq!(git(&bare, &["rev-parse", "refs/heads/agent/SHOP-1"]), head);

    // A retry rewrites the agent branch (non fast-forward), by full sha.
    let pushed = publisher.push_branch(&push(&work, &parent)).await.unwrap();
    assert_eq!(pushed.commit, parent);
    assert_eq!(
        git(&bare, &["rev-parse", "refs/heads/agent/SHOP-1"]),
        parent
    );
    // Nothing else was written on the remote.
    assert_eq!(
        git(&bare, &["for-each-ref", "--format=%(refname)"]),
        "refs/heads/agent/SHOP-1"
    );
}

#[tokio::test]
async fn gitlab_pushes_the_same_way() {
    let (_tmp, work, bare) = repos();
    let publisher = GitlabPublisher::new(
        GitlabPublisherConfig::new("https://gitlab.example.com/api/v4", "acme/shop", "glpat-x")
            .with_push_url(remote_url(&bare)),
    )
    .unwrap();
    let pushed = publisher.push_branch(&push(&work, "HEAD")).await.unwrap();
    assert_eq!(
        git(&bare, &["rev-parse", "refs/heads/agent/SHOP-1"]),
        pushed.commit
    );
}

#[tokio::test]
async fn an_unknown_revision_or_remote_fails_without_leaking_the_token() {
    let (tmp, work, bare) = repos();
    let err = github(&bare)
        .push_branch(&push(&work, "no-such-branch"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, CodeHostError::Rejected { status: 0, .. }),
        "{err:?}"
    );

    let missing = tmp.path().join("missing.git");
    let err = github(&missing)
        .push_branch(&push(&work, "HEAD"))
        .await
        .unwrap_err();
    assert!(matches!(err, CodeHostError::Transport(_)), "{err:?}");
    assert!(!err.to_string().contains(TOKEN));

    let err = github(&bare)
        .push_branch(&push(&work, "--mirror"))
        .await
        .unwrap_err();
    assert!(matches!(err, CodeHostError::Rejected { status: 0, .. }));
}

#[tokio::test]
async fn publish_agent_pr_pushes_then_opens_the_pr() {
    let (_tmp, work, bare) = repos();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 3, "html_url": "https://github.com/acme/shop/pull/3", "draft": true
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/labels"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"name": "agent"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/acme/shop/issues/3/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{"name": "agent"}])))
        .expect(1)
        .mount(&server)
        .await;
    let publisher = GithubPublisher::new(
        GithubPublisherConfig::new(server.uri(), "acme/shop", TOKEN)
            .with_push_url(remote_url(&bare))
            .with_base_branch("main"),
    )
    .unwrap();
    let pr = AgentPullRequest::new("SHOP-1", "SHOP-1: x", SealedSpec::seal("t.spec.ts", b"s"));
    let (pushed, opened) = publish_agent_pr(&publisher, &push(&work, "HEAD"), &pr)
        .await
        .unwrap();
    assert_eq!(
        git(&bare, &["rev-parse", "refs/heads/agent/SHOP-1"]),
        pushed.commit
    );
    assert_eq!(opened.number, 3);

    // The push and the PR must name the same ticket.
    let mut other = pr.clone();
    other.ticket = "SHOP-2".into();
    let err = publish_agent_pr(&publisher, &push(&work, "HEAD"), &other)
        .await
        .unwrap_err();
    assert!(matches!(err, CodeHostError::Rejected { status: 0, .. }));
}

#[tokio::test]
async fn the_token_reaches_the_git_server_only_as_a_basic_auth_header() {
    use base64::Engine;
    let (_tmp, work, _bare) = repos();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/acme/shop.git/info/refs"))
        .respond_with(
            ResponseTemplate::new(401).insert_header("www-authenticate", "Basic realm=\"git\""),
        )
        .mount(&server)
        .await;
    let publisher = GithubPublisher::new(
        GithubPublisherConfig::new("https://api.github.com", "acme/shop", TOKEN)
            .with_push_url(format!("{}/acme/shop.git", server.uri())),
    )
    .unwrap();
    let err = publisher
        .push_branch(&push(&work, "HEAD"))
        .await
        .unwrap_err();
    assert_eq!(err, CodeHostError::Unauthorized(401), "{err}");

    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{TOKEN}"))
    );
    let requests = server.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    for r in &requests {
        assert_eq!(r.url.query(), Some("service=git-receive-pack"));
        assert_eq!(
            r.headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some(expected.as_str())
        );
        assert!(!r.url.as_str().contains(TOKEN));
    }
}
