//! Environment variables passed to Playwright (contract: `runner-env.md`).
//!
//! Shard-only variables (`GUARD_SHARD_INDEX`, `GUARD_SHARD_PLAN`, `CI`) are
//! added by the caller; this module builds the variables every run gets.

use crate::config::TargetConfig;

#[cfg(test)]
mod tests;

/// Host that reaches the machine running Docker from inside a container.
pub const DOCKER_HOST_ALIAS: &str = "host.docker.internal";

pub struct RunnerEnvInput<'a> {
    pub config: &'a TargetConfig,
    /// Already formatted `E2E_TAGS` value.
    pub e2e_tags: &'a str,
    pub run_id: &'a str,
    /// Value of the variable named by `[testability].token_env`, when set.
    pub token: Option<&'a str>,
    /// Rewrites loopback hosts in `TARGET_BASE_URL` for `--docker`.
    pub docker: bool,
}

/// The variables in a fixed order. `GUARD_TEST_TOKEN` is present only when a token is.
pub fn runner_env(input: &RunnerEnvInput<'_>) -> Vec<(String, String)> {
    let config = input.config;
    let base_url = if input.docker {
        docker_base_url(&config.app.base_url)
    } else {
        config.app.base_url.clone()
    };
    let mut env = vec![
        ("E2E_TAGS".to_string(), input.e2e_tags.to_string()),
        ("TARGET_BASE_URL".to_string(), base_url),
        ("RUN_ID".to_string(), input.run_id.to_string()),
    ];
    if let Some(token) = input.token {
        env.push(("GUARD_TEST_TOKEN".to_string(), token.to_string()));
    }
    env.push((
        "GUARD_LOGIN_PATH".to_string(),
        config.testability.login_path.clone(),
    ));
    env.push((
        "GUARD_SEED_PATH".to_string(),
        config.testability.seed_path.clone(),
    ));
    env.push((
        "GUARD_VIDEO".to_string(),
        config.evidence.video.as_env().to_string(),
    ));
    env.push((
        "GUARD_ROLES".to_string(),
        config.testability.roles.join(","),
    ));
    env.push((
        "GUARD_BLOCKED_DOMAINS".to_string(),
        config.network.blocked_domains.join(","),
    ));
    env
}

/// Rewrites a `localhost` or `127.0.0.1` host to [`DOCKER_HOST_ALIAS`].
pub fn docker_base_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host != "localhost" && host != "127.0.0.1" {
        return url.to_string();
    }
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    format!("{scheme}://{DOCKER_HOST_ALIAS}{port}{path}")
}

/// `r<unix-seconds>-<pid>`.
pub fn generate_run_id(unix_seconds: u64, pid: u32) -> String {
    format!("r{unix_seconds}-{pid}")
}

/// `^[A-Za-z0-9_.-]{1,64}$`.
pub fn is_valid_run_id(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// An inherited non-empty `RUN_ID` is forwarded unchanged; otherwise one is generated.
pub fn resolve_run_id(inherited: Option<String>, unix_seconds: u64, pid: u32) -> String {
    inherited
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| generate_run_id(unix_seconds, pid))
}
