//! `git push` of `agent/<KEY>` with the token kept out of argv.
//!
//! The token reaches git as an `http.<remote>.extraHeader` set through the
//! `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_n` / `GIT_CONFIG_VALUE_n` environment
//! (git >= 2.31), scoped to the remote URL, only for `https://` remotes.
//! Credential helpers, prompts and the local `pre-push` hook are disabled.
//!
//! - GitHub accepts HTTP basic auth with user `x-access-token` and the token
//!   as password (installation tokens and PATs):
//!   <https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-as-a-github-app-installation#about-authentication-as-a-github-app-installation>.
//! - GitLab accepts user `oauth2` and the token as password:
//!   <https://docs.gitlab.com/user/profile/personal_access_tokens/>.

use super::rejected_locally;
use base64::Engine;
use guard_core::ports::CodeHostError;
use guard_core::ports::code_host::{BranchPush, PushedBranch};
use secrecy::{ExposeSecret, SecretString};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const PUSH_TIMEOUT: Duration = Duration::from_secs(300);

/// Accepts `https://`, `ssh://`, `file://`, scp-like (`git@host:path`),
/// local paths and loopback `http://` (`127.0.0.1`, `[::1]`, `localhost`;
/// the token never leaves the host). Refuses any other cleartext `http://`
/// and URLs embedding credentials.
pub(crate) fn validate_push_url(url: &str) -> Result<(), CodeHostError> {
    let bad = |why: &str| {
        Err(CodeHostError::Unconfigured(format!(
            "{} {why}: {:?}",
            super::PUSH_URL_VAR,
            without_userinfo(url)
        )))
    };
    if url.is_empty()
        || url.starts_with('-')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return bad("is not a usable remote");
    }
    if url.to_ascii_lowercase().starts_with("http://") && !is_loopback_http(url) {
        return bad("must not be cleartext http (the token would travel unencrypted)");
    }
    if let Ok(parsed) = reqwest::Url::parse(url) {
        if parsed.password().is_some()
            || (parsed.scheme() == "https" && !parsed.username().is_empty())
        {
            return bad("must not embed credentials");
        }
    }
    Ok(())
}

/// `url` with the userinfo of a `scheme://` URL (`user:token@`) replaced by
/// `***@`, so an error message never repeats an embedded token. Works on
/// unparsable input too: the authority is everything after `://` up to the
/// next `/`, and the userinfo everything in it up to its last `@`.
fn without_userinfo(url: &str) -> String {
    let Some(start) = url.find("://").map(|i| i + 3) else {
        return url.to_string();
    };
    let authority_end = url[start..].find('/').map_or(url.len(), |i| start + i);
    match url[start..authority_end].rfind('@') {
        Some(at) => format!("{}***{}", &url[..start], &url[start + at..]),
        None => url.to_string(),
    }
}

fn is_loopback_http(url: &str) -> bool {
    reqwest::Url::parse(url).is_ok_and(|u| {
        let host = u.host_str().unwrap_or_default();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        u.scheme() == "http"
            && (host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()))
    })
}

/// `true` when git sends the auth header to `remote` (HTTPS or loopback).
fn sends_auth(remote: &str) -> bool {
    remote.to_ascii_lowercase().starts_with("https://") || is_loopback_http(remote)
}

/// `GIT_CONFIG_*` variables setting the header for `remote` only.
fn auth_env(remote: &str, header: &SecretString) -> Vec<(String, String)> {
    vec![
        ("GIT_CONFIG_COUNT".into(), "1".into()),
        (
            "GIT_CONFIG_KEY_0".into(),
            format!("http.{remote}.extraHeader"),
        ),
        (
            "GIT_CONFIG_VALUE_0".into(),
            header.expose_secret().to_string(),
        ),
    ]
}

pub(crate) fn auth_header(user: &str, token: &SecretString) -> SecretString {
    let raw = format!("{user}:{}", token.expose_secret());
    SecretString::from(format!(
        "Authorization: Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    ))
}

/// Resolves `push.rev` in `push.local_repo` and force-pushes it to
/// `refs/heads/agent/<ticket>` on `remote`.
pub(crate) async fn push(
    push: &BranchPush,
    remote: &str,
    user: &str,
    token: &SecretString,
) -> Result<PushedBranch, CodeHostError> {
    push.validate()?;
    validate_push_url(remote)?;
    let branch = push.branch();
    let commit = resolve(push).await?;
    let header = auth_header(user, token);
    let mut cmd = git_command();
    cmd.arg("-C")
        .arg(&push.local_repo)
        .args([
            "-c",
            "credential.helper=",
            "push",
            "--no-verify",
            "--porcelain",
            "--force",
        ])
        .arg(remote)
        .arg(format!("{commit}:refs/heads/{branch}"));
    if sends_auth(remote) {
        cmd.envs(auth_env(remote, &header));
    }
    let output = run(cmd).await?;
    if output.status.success() {
        return Ok(PushedBranch { branch, commit });
    }
    let stderr = redact(
        &String::from_utf8_lossy(&output.stderr),
        &[token.expose_secret(), header.expose_secret()],
    );
    Err(classify_push_failure(&stderr))
}

async fn resolve(push: &BranchPush) -> Result<String, CodeHostError> {
    let mut cmd = git_command();
    cmd.arg("-C")
        .arg(&push.local_repo)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{}^{{commit}}", push.rev));
    let output = run(cmd).await?;
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() && sha.len() >= 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(rejected_locally(format!(
            "revision {:?} is not a commit in {}",
            push.rev,
            push.local_repo.display()
        )))
    }
}

fn git_command() -> Command {
    let mut cmd = Command::new("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

async fn run(mut cmd: Command) -> Result<std::process::Output, CodeHostError> {
    let child = cmd
        .spawn()
        .map_err(|e| CodeHostError::Transport(format!("cannot run git: {e}")))?;
    match tokio::time::timeout(PUSH_TIMEOUT, child.wait_with_output()).await {
        Ok(result) => result.map_err(|e| CodeHostError::Transport(format!("git failed: {e}"))),
        Err(_) => Err(CodeHostError::Transport(format!(
            "git timed out after {}s",
            PUSH_TIMEOUT.as_secs()
        ))),
    }
}

fn classify_push_failure(stderr: &str) -> CodeHostError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("returned error: 403") || lower.contains("permission to") {
        CodeHostError::Unauthorized(403)
    } else if lower.contains("authentication failed")
        || lower.contains("returned error: 401")
        || lower.contains("terminal prompts disabled")
    {
        CodeHostError::Unauthorized(401)
    } else {
        CodeHostError::Transport(format!("git push failed: {}", stderr.trim()))
    }
}

fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(secret, "[redacted]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_urls_refuse_cleartext_and_embedded_credentials() {
        for ok in [
            "https://github.com/acme/shop.git",
            "ssh://git@github.com/acme/shop.git",
            "git@github.com:acme/shop.git",
            "file:///srv/shop.git",
            "C:/tmp/remote.git",
            "/srv/remote.git",
            "http://127.0.0.1:8080/acme/shop.git",
            "http://localhost/acme/shop.git",
            "http://[::1]:1/acme/shop.git",
        ] {
            validate_push_url(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "--upload-pack=x",
            "http://github.com/acme/shop.git",
            "http://127.0.0.2.example/acme/shop.git",
            "HTTP://github.com/acme/shop.git",
            "https://user:pass@github.com/acme/shop.git",
            "https://x-access-token@github.com/acme/shop.git",
            "https://github.com/acme/shop.git extra",
        ] {
            assert!(validate_push_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_rejected_push_url_never_echoes_its_credentials() {
        for (bad, secret) in [
            (
                "https://x-access-token:ghp_REALTOKEN@github.com/acme/shop.git",
                "ghp_REALTOKEN",
            ),
            ("ssh://user:pw-SECRET@host/acme/shop.git", "pw-SECRET"),
            (
                "http://user:tok-SECRET@github.com/acme/shop.git",
                "tok-SECRET",
            ),
            ("https://tok-SECRET@github.com/acme/shop.git", "tok-SECRET"),
            ("https://u:tok-SECRET@github.com/a b.git", "tok-SECRET"),
            ("https://u:tok-SECRET@exa mple.com/a.git", "tok-SECRET"),
        ] {
            let err = validate_push_url(bad).unwrap_err().to_string();
            assert!(!err.contains(secret), "{err}");
            assert!(err.contains("***@"), "{err}");
        }
    }

    #[test]
    fn auth_header_is_basic_with_the_user_and_token() {
        let header = auth_header("x-access-token", &SecretString::from("abc".to_string()));
        // base64("x-access-token:abc")
        assert_eq!(
            header.expose_secret(),
            "Authorization: Basic eC1hY2Nlc3MtdG9rZW46YWJj"
        );
    }

    #[test]
    fn the_header_is_scoped_to_the_remote_url() {
        let remote = "https://github.com/acme/shop.git";
        let header = auth_header("x-access-token", &SecretString::from("abc".to_string()));
        // Only the variables auth_env sets may count: outside any repository
        // (actions/checkout stores its own `http.https://github.com/.extraheader`
        // in the checkout) and without global or system config.
        let isolated = tempfile::tempdir().unwrap();
        let empty_global = isolated.path().join("gitconfig");
        std::fs::write(&empty_global, "").unwrap();
        let get = |url: &str| {
            let out = std::process::Command::new("git")
                .args(["config", "--get-urlmatch", "http.extraHeader", url])
                .current_dir(isolated.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", &empty_global)
                .env("GIT_CEILING_DIRECTORIES", isolated.path())
                .envs(auth_env(remote, &header))
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        assert_eq!(get(remote), header.expose_secret());
        assert_eq!(get("https://github.com/other/repo.git"), "");
        assert_eq!(get("https://evil.example/acme/shop.git"), "");
        assert!(sends_auth(remote));
        assert!(sends_auth("http://127.0.0.1:1/x.git"));
        assert!(!sends_auth("C:/tmp/remote.git"));
        assert!(!sends_auth("git@github.com:acme/shop.git"));
    }

    #[test]
    fn failures_are_classified_and_redacted() {
        assert_eq!(
            classify_push_failure("fatal: Authentication failed for 'https://x/'"),
            CodeHostError::Unauthorized(401)
        );
        assert_eq!(
            classify_push_failure("remote: Permission to acme/shop.git denied to bot."),
            CodeHostError::Unauthorized(403)
        );
        assert!(matches!(
            classify_push_failure("fatal: unable to access"),
            CodeHostError::Transport(_)
        ));
        assert_eq!(
            redact("a tok b tok", &["tok", ""]),
            "a [redacted] b [redacted]"
        );
    }
}
