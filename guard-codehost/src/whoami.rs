//! Token checks of `guard connect github|gitlab` (cli.md): `GET /user` on
//! the API base URL, returning the user name the token belongs to.
//!
//! The token only travels in the auth header; errors never contain it.

use crate::http;
use guard_core::ports::CodeHostError;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

/// GitHub `GET /user` with `Authorization: Bearer <token>`: the `login`.
pub async fn github_user(api_url: &str, token: &str) -> Result<String, CodeHostError> {
    let token = SecretString::from(token.to_string());
    let request = http::client()?
        .get(format!("{}/user", http::trim_base(api_url)))
        .bearer_auth(token.expose_secret())
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28");
    name(request, "login").await
}

/// GitLab `GET /user` with `PRIVATE-TOKEN: <token>`: the `username`.
/// `api_url` includes `/api/v4`.
pub async fn gitlab_user(api_url: &str, token: &str) -> Result<String, CodeHostError> {
    let token = SecretString::from(token.to_string());
    let request = http::client()?
        .get(format!("{}/user", http::trim_base(api_url)))
        .header("private-token", token.expose_secret());
    name(request, "username").await
}

async fn name(request: reqwest::RequestBuilder, field: &str) -> Result<String, CodeHostError> {
    let response = request.send().await.map_err(http::transport)?;
    let response = http::ok(response, None).await?;
    let status = response.status().as_u16();
    let body: Value = http::json(response).await?;
    body[field]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| CodeHostError::Rejected {
            status,
            body: format!("GET /user response without {field}"),
        })
}
