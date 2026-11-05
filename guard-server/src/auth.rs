//! Authentication: the bearer token of `/api/*` and the pluggable dashboard
//! login.
//!
//! The dashboard asks an [`AuthProvider`]. Phase 2 ships two:
//! [`TokenAuthProvider`] (the `GUARD_DASHBOARD_TOKEN` dev login of
//! server-api.md) and [`OpenAuthProvider`] (no token configured: deploy behind
//! the internal network). GitHub/GitLab OAuth (Phase 3) is another
//! implementation of the same trait, wired by the integrator.

use axum::http::HeaderMap;
use axum::http::header::{AUTHORIZATION, COOKIE};
use subtle::ConstantTimeEq;

/// Cookie set by the token login.
pub const DASHBOARD_COOKIE: &str = "guard_dashboard";

/// Who is looking at the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub login: String,
    /// `token`, `open`, `github`, `gitlab`, ...
    pub provider: String,
}

/// What the provider sees of a dashboard request.
pub struct AuthRequest<'a> {
    pub path: &'a str,
    /// Raw query string without `?`.
    pub query: Option<&'a str>,
    pub headers: &'a HeaderMap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Allowed(Principal),
    /// `303 See Other` to `location`, optionally setting a cookie (login
    /// handshakes: the token query, an OAuth callback).
    Redirect {
        location: String,
        set_cookie: Option<String>,
    },
    /// `401`.
    Denied,
}

#[async_trait::async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, request: &AuthRequest<'_>) -> AuthOutcome;
}

/// Constant-time equality (length is not secret).
pub fn secret_eq(given: &str, expected: &str) -> bool {
    given.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// The token of `Authorization: Bearer <token>`, if any.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn cookie<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let decoded = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[i], decoded) {
            (_, Some(b)) => {
                out.push(b);
                i += 3;
                continue;
            }
            (b'+', None) => out.push(b' '),
            (b, None) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lifetime of a dashboard session cookie (12 hours).
pub const SESSION_TTL_SECS: u64 = 12 * 3600;

type HmacSha256 = hmac::Hmac<sha2::Sha256>;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `GUARD_DASHBOARD_TOKEN`: `Authorization: Bearer <token>`, cookie
/// `guard_dashboard`, or `?token=<token>` (sets the cookie and redirects to
/// the same path without that parameter, so the token stays out of the
/// browser's address bar and history after the first request).
///
/// The cookie never holds the token: it is a session `<expiry>.<hmac>` with
/// `hmac = HMAC-SHA256(token, "guard_dashboard:<expiry>")`, valid for
/// [`SESSION_TTL_SECS`] (`Max-Age`), `Secure` when the request came over TLS
/// (`X-Forwarded-Proto: https`). Rotating the token ends every session. A
/// cookie equal to the token itself is still accepted (older sessions).
pub struct TokenAuthProvider {
    token: String,
    now: fn() -> u64,
}

impl TokenAuthProvider {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            now: unix_now,
        }
    }

    /// Replaces the clock (unix seconds), for tests.
    pub fn with_clock(mut self, now: fn() -> u64) -> Self {
        self.now = now;
        self
    }

    fn principal() -> Principal {
        Principal {
            login: "token".into(),
            provider: "token".into(),
        }
    }

    fn mac(&self, expiry: u64) -> HmacSha256 {
        use hmac::Mac;
        let mut mac =
            HmacSha256::new_from_slice(self.token.as_bytes()).expect("HMAC takes any key length");
        mac.update(format!("{DASHBOARD_COOKIE}:{expiry}").as_bytes());
        mac
    }

    fn session(&self) -> String {
        use hmac::Mac;
        let expiry = (self.now)() + SESSION_TTL_SECS;
        let tag = self.mac(expiry).finalize().into_bytes();
        format!("{expiry}.{}", hex::encode(tag))
    }

    /// A session cookie value signed with this token and not expired.
    pub fn valid_session(&self, value: &str) -> bool {
        use hmac::Mac;
        let Some((expiry, tag)) = value.split_once('.') else {
            return false;
        };
        let (Ok(expiry), Ok(tag)) = (expiry.parse::<u64>(), hex::decode(tag)) else {
            return false;
        };
        expiry > (self.now)()
            && expiry <= (self.now)() + SESSION_TTL_SECS
            && self.mac(expiry).verify_slice(&tag).is_ok()
    }
}

/// The request reached the server over TLS (directly or via a proxy).
fn over_tls(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("https"))
}

#[async_trait::async_trait]
impl AuthProvider for TokenAuthProvider {
    async fn authenticate(&self, request: &AuthRequest<'_>) -> AuthOutcome {
        let cookie_ok = cookie(request.headers, DASHBOARD_COOKIE)
            .is_some_and(|c| self.valid_session(c) || secret_eq(c, &self.token));
        if bearer(request.headers).is_some_and(|t| secret_eq(t, &self.token)) || cookie_ok {
            return AuthOutcome::Allowed(Self::principal());
        }
        let Some(query) = request.query else {
            return AuthOutcome::Denied;
        };
        let mut token = None;
        let mut rest = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            match pair.split_once('=') {
                Some(("token", value)) => token = Some(percent_decode(value)),
                _ => rest.push(pair),
            }
        }
        match token {
            Some(t) if secret_eq(&t, &self.token) => {
                let location = if rest.is_empty() {
                    request.path.to_string()
                } else {
                    format!("{}?{}", request.path, rest.join("&"))
                };
                let secure = if over_tls(request.headers) {
                    "; Secure"
                } else {
                    ""
                };
                AuthOutcome::Redirect {
                    location,
                    set_cookie: Some(format!(
                        "{DASHBOARD_COOKIE}={}; Path=/; Max-Age={SESSION_TTL_SECS}; HttpOnly; SameSite=Strict{secure}",
                        self.session()
                    )),
                }
            }
            _ => AuthOutcome::Denied,
        }
    }
}

/// No dashboard token configured: everyone is allowed.
pub struct OpenAuthProvider;

#[async_trait::async_trait]
impl AuthProvider for OpenAuthProvider {
    async fn authenticate(&self, _request: &AuthRequest<'_>) -> AuthOutcome {
        AuthOutcome::Allowed(Principal {
            login: "anonymous".into(),
            provider: "open".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn req<'a>(headers: &'a HeaderMap, query: Option<&'a str>) -> AuthRequest<'a> {
        AuthRequest {
            path: "/catalog",
            query,
            headers,
        }
    }

    #[test]
    fn secret_eq_is_exact() {
        assert!(secret_eq("abc", "abc"));
        assert!(!secret_eq("abc", "abd"));
        assert!(!secret_eq("ab", "abc"));
        assert!(!secret_eq("", "abc"));
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        assert_eq!(bearer(&h), None);
        h.insert(AUTHORIZATION, HeaderValue::from_static("Basic x"));
        assert_eq!(bearer(&h), None);
        h.insert(AUTHORIZATION, HeaderValue::from_static("Bearer tok"));
        assert_eq!(bearer(&h), Some("tok"));
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[tokio::test]
    async fn token_provider_paths() {
        let p = TokenAuthProvider::new("s3cret");
        let empty = HeaderMap::new();
        assert_eq!(
            p.authenticate(&req(&empty, None)).await,
            AuthOutcome::Denied
        );
        let p = TokenAuthProvider::new("s3cret").with_clock(|| 1_000);
        let AuthOutcome::Redirect {
            location,
            set_cookie: Some(cookie),
        } = p.authenticate(&req(&empty, Some("token=s3cret"))).await
        else {
            panic!("expected a redirect with a cookie");
        };
        assert_eq!(location, "/catalog");
        assert!(!cookie.contains("s3cret"), "{cookie}");
        assert!(
            cookie.ends_with(&format!(
                "; Path=/; Max-Age={SESSION_TTL_SECS}; HttpOnly; SameSite=Strict"
            )),
            "{cookie}"
        );
        let value = cookie
            .strip_prefix("guard_dashboard=")
            .and_then(|c| c.split(';').next())
            .unwrap()
            .to_string();
        assert!(p.valid_session(&value));
        // Expired after the TTL.
        let later = TokenAuthProvider::new("s3cret").with_clock(|| 1_000 + SESSION_TTL_SECS + 1);
        assert!(!later.valid_session(&value));
        // Signed with another token: invalid.
        assert!(
            !TokenAuthProvider::new("other")
                .with_clock(|| 1_000)
                .valid_session(&value)
        );
        assert!(!p.valid_session("garbage"));
        assert!(!p.valid_session(&format!("99999999999.{}", "0".repeat(64))));
        assert_eq!(
            p.authenticate(&req(&empty, Some("token=bad"))).await,
            AuthOutcome::Denied
        );
        let mut cookies = HeaderMap::new();
        cookies.insert(
            COOKIE,
            HeaderValue::from_static("a=b; guard_dashboard=s3cret"),
        );
        assert!(matches!(
            p.authenticate(&req(&cookies, None)).await,
            AuthOutcome::Allowed(_)
        ));
    }
}
