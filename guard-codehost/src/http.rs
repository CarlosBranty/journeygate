//! HTTP plumbing shared by both adapters: client, status mapping, pagination
//! guards and text limits.

use guard_core::ports::CodeHostError;
use reqwest::{Response, StatusCode};
use serde::de::DeserializeOwned;
use std::time::Duration;

/// Most pages followed for one listing (GitHub caps PR files at 3000 = 30
/// pages of 100); protects against a server that never stops paginating.
pub(crate) const MAX_PAGES: usize = 100;

/// Longest error body kept in `CodeHostError::Rejected`.
const MAX_ERROR_BODY: usize = 2000;

pub(crate) fn client() -> Result<reqwest::Client, CodeHostError> {
    reqwest::Client::builder()
        .user_agent(concat!("guard/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        // reqwest strips only Authorization/Cookie on a cross-host redirect,
        // not GitLab's PRIVATE-TOKEN: never follow one off the API's origin.
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same_origin = attempt
                .previous()
                .first()
                .is_some_and(|first| first.origin() == attempt.url().origin());
            if !same_origin {
                attempt.stop()
            } else if attempt.previous().len() > 10 {
                attempt.error("too many redirects")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|e| CodeHostError::Transport(e.to_string()))
}

/// Removes trailing slashes so paths can be appended with `/...`.
pub(crate) fn trim_base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

pub(crate) fn transport(error: reqwest::Error) -> CodeHostError {
    // reqwest errors carry the URL, never request headers, so no token leaks.
    CodeHostError::Transport(error.without_url().to_string())
}

/// Maps a non-2xx response to the port error. `pr` is the PR number when the
/// request addresses a PR, so a 404 becomes `NotFound(pr)`.
pub(crate) async fn error_for(response: Response, pr: Option<u64>) -> CodeHostError {
    let status = response.status();
    let rate_limit_exhausted = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0")
        || response.headers().contains_key("retry-after");
    match (status, pr) {
        (StatusCode::TOO_MANY_REQUESTS, _) => CodeHostError::RateLimited,
        (StatusCode::FORBIDDEN, _) if rate_limit_exhausted => CodeHostError::RateLimited,
        (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN, _) => {
            CodeHostError::Unauthorized(status.as_u16())
        }
        (StatusCode::NOT_FOUND, Some(number)) => CodeHostError::NotFound(number),
        _ => {
            let body = response.text().await.unwrap_or_default();
            CodeHostError::Rejected {
                status: status.as_u16(),
                body: truncate_chars(&body, MAX_ERROR_BODY),
            }
        }
    }
}

/// Returns the response if it is a success, else the mapped error.
pub(crate) async fn ok(response: Response, pr: Option<u64>) -> Result<Response, CodeHostError> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(error_for(response, pr).await)
    }
}

pub(crate) async fn json<T: DeserializeOwned>(response: Response) -> Result<T, CodeHostError> {
    let status = response.status().as_u16();
    let bytes = response.bytes().await.map_err(transport)?;
    serde_json::from_slice(&bytes).map_err(|e| CodeHostError::Rejected {
        status,
        body: format!("unexpected response body: {e}"),
    })
}

/// `true` when `next` points at the same scheme, host and port as `base`, so
/// a pagination link can never send the token to another host.
pub(crate) fn same_origin(base: &str, next: &str) -> bool {
    match (reqwest::Url::parse(base), reqwest::Url::parse(next)) {
        (Ok(a), Ok(b)) => {
            a.scheme() == b.scheme()
                && a.host_str() == b.host_str()
                && a.port_or_known_default() == b.port_or_known_default()
        }
        _ => false,
    }
}

/// At most `max` characters; a cut string ends with `…` (within `max`).
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate_chars("abc", 3), "abc");
        assert_eq!(truncate_chars("abcd", 3), "ab…");
        assert_eq!(truncate_chars("ééééé", 4), "ééé…");
    }

    #[test]
    fn same_origin_compares_scheme_host_and_port() {
        assert!(same_origin(
            "https://api.github.com",
            "https://api.github.com/x?page=2"
        ));
        assert!(same_origin(
            "https://api.github.com",
            "https://api.github.com:443/x"
        ));
        assert!(!same_origin(
            "https://api.github.com",
            "http://api.github.com/x"
        ));
        assert!(!same_origin(
            "https://api.github.com",
            "https://evil.example/x"
        ));
        assert!(!same_origin("http://127.0.0.1:1", "http://127.0.0.1:2/x"));
        assert!(!same_origin("https://api.github.com", "not a url"));
    }

    #[test]
    fn trim_base_drops_trailing_slashes() {
        assert_eq!(trim_base("https://x/api/v4//"), "https://x/api/v4");
    }
}
