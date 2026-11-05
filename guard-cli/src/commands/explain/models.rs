//! `guard explain --list-models [--json]`: the models the configured provider
//! offers, from the same base URL and key as `guard explain`.
//!
//! The list endpoint is picked from the base URL's host, with fallbacks
//! tried in order (all on the same host as the base URL):
//!
//! - DeepSeek (`*.deepseek.com`): `GET {origin}/models`, OpenAI format with
//!   `Authorization: Bearer` (api-docs.deepseek.com/api/list-models), then
//!   the Anthropic format under the base.
//! - Ollama (port 11434): `GET {origin}/api/tags`, then `{origin}/v1/models`.
//! - `api.anthropic.com`: `GET {base}/v1/models` only.
//! - Anything else (Anthropic-compatible): `{base}/v1/models`, then
//!   `{base}/models` (OpenAI format), then `{origin}/api/tags`.
//!
//! A rejected key (401/403) stops the fallbacks: it is the real error. The
//! key is sent only in headers, never printed, and redacted from any echoed
//! error body.

use super::{API_KEY_ENV, FALLBACK_API_KEY_ENV, api_error, redact};
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::runtime::block_on;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;
use std::time::Duration;

/// Escapes everything but RFC 3986 unreserved characters in a query value.
const QUERY_VALUE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Anthropic's `limit` maximum for `GET /v1/models`.
const PAGE_LIMIT: u32 = 1000;
/// A safety stop for a server whose `has_more` never turns false.
const MAX_PAGES: usize = 50;

/// One model a provider offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The response shape of a list endpoint, and the auth it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListFormat {
    /// `{data: [{id, display_name}], has_more, last_id}`; `x-api-key`.
    Anthropic,
    /// `{object: "list", data: [{id, name?}]}`; `Authorization: Bearer`.
    OpenAi,
    /// Ollama's `{models: [{name}]}`; no auth.
    Ollama,
}

/// One list endpoint to try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub format: ListFormat,
    pub url: String,
}

/// One page of the Anthropic list: its models and the `after_id` of the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub models: Vec<ModelInfo>,
    pub next: Option<String>,
}

/// The `host[:port]` of a base URL, lowercased, without scheme, credentials
/// or path.
pub fn host_of(base: &str) -> String {
    let url = base.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    host.to_ascii_lowercase()
}

/// `scheme://authority` of a base URL (its path dropped).
fn origin(base: &str) -> String {
    let url = base.trim().trim_end_matches('/');
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
            format!("{scheme}://{authority}")
        }
        None => url.split(['/', '?', '#']).next().unwrap_or_default().into(),
    }
}

/// The list endpoints for a base URL, best guess first (see the module docs).
pub fn endpoints(base: &str) -> Vec<Endpoint> {
    let base = base.trim().trim_end_matches('/');
    let host = host_of(base);
    let origin = origin(base);
    let at = |format, url: String| Endpoint { format, url };
    let name = host.split(':').next().unwrap_or_default();
    if name == "deepseek.com" || name.ends_with(".deepseek.com") {
        vec![
            at(ListFormat::OpenAi, format!("{origin}/models")),
            at(ListFormat::Anthropic, format!("{base}/v1/models")),
        ]
    } else if host.ends_with(":11434") {
        vec![
            at(ListFormat::Ollama, format!("{origin}/api/tags")),
            at(ListFormat::OpenAi, format!("{origin}/v1/models")),
        ]
    } else if host == "api.anthropic.com" {
        vec![at(ListFormat::Anthropic, format!("{base}/v1/models"))]
    } else {
        vec![
            at(ListFormat::Anthropic, format!("{base}/v1/models")),
            at(ListFormat::OpenAi, format!("{base}/models")),
            at(ListFormat::Ollama, format!("{origin}/api/tags")),
        ]
    }
}

fn json(body: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(body).map_err(|e| format!("invalid model list: {e}"))
}

/// A non-blank string field.
fn text(value: &serde_json::Value, field: &str) -> Option<String> {
    value[field]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The `data` array of models, each with an `id` and the display name in
/// `name_field`.
fn data_models(value: &serde_json::Value, name_field: &str) -> Result<Vec<ModelInfo>, String> {
    let data = value["data"]
        .as_array()
        .ok_or("model list has no data array")?;
    Ok(data
        .iter()
        .filter_map(|m| {
            Some(ModelInfo {
                id: text(m, "id")?,
                name: text(m, name_field),
            })
        })
        .collect())
}

/// One page of `GET /v1/models` (Anthropic format).
pub fn parse_anthropic_page(body: &str) -> Result<Page, String> {
    let value = json(body)?;
    let models = data_models(&value, "display_name")?;
    let next = if value["has_more"].as_bool() == Some(true) {
        text(&value, "last_id")
    } else {
        None
    };
    Ok(Page { models, next })
}

/// An OpenAI-format list (`GET /models` on DeepSeek).
pub fn parse_openai(body: &str) -> Result<Vec<ModelInfo>, String> {
    data_models(&json(body)?, "name")
}

/// Ollama's `GET /api/tags`.
pub fn parse_ollama(body: &str) -> Result<Vec<ModelInfo>, String> {
    let value = json(body)?;
    let models = value["models"]
        .as_array()
        .ok_or("model list has no models array")?;
    Ok(models
        .iter()
        .filter_map(|m| {
            Some(ModelInfo {
                id: text(m, "name").or_else(|| text(m, "model"))?,
                name: None,
            })
        })
        .collect())
}

/// One id per line, its display name after it when there is one.
pub fn render_text(models: &[ModelInfo]) -> String {
    let width = models.iter().map(|m| m.id.len()).max().unwrap_or(0);
    let mut out = String::new();
    for m in models {
        match m.name.as_deref().filter(|n| *n != m.id) {
            Some(name) => out.push_str(&format!("{:<width$}  {name}\n", m.id)),
            None => out.push_str(&format!("{}\n", m.id)),
        }
    }
    out
}

/// The `--json` output: one array on one line.
pub fn render_json(models: &[ModelInfo]) -> String {
    serde_json::to_string(models).expect("model list serialises")
}

/// Why one endpoint failed; `fatal` stops the fallbacks (the key was rejected).
#[derive(Debug)]
struct Failure {
    message: String,
    fatal: bool,
}

/// `GET url` with the endpoint's auth; the body of a 2xx, else the error.
async fn get(
    client: &reqwest::Client,
    format: ListFormat,
    url: &str,
    key: Option<&str>,
) -> Result<String, Failure> {
    let mut request = client.get(url);
    match (format, key) {
        (ListFormat::Anthropic, Some(key)) => {
            request = request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01");
        }
        (ListFormat::Anthropic, None) => {
            request = request.header("anthropic-version", "2023-06-01");
        }
        (ListFormat::OpenAi, Some(key)) => request = request.bearer_auth(key),
        _ => {}
    }
    let failure = |message: String, fatal| Failure {
        message: redact(&message, key),
        fatal,
    };
    let response = request
        .send()
        .await
        .map_err(|e| failure(e.to_string(), false))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|e| failure(e.to_string(), false))?;
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(failure(
            api_error(status, &body),
            matches!(status, 401 | 403),
        ))
    }
}

/// Every model behind one endpoint, following Anthropic pagination.
async fn fetch(
    client: &reqwest::Client,
    endpoint: &Endpoint,
    key: Option<&str>,
) -> Result<Vec<ModelInfo>, Failure> {
    let parse_failure = |message: String| Failure {
        message,
        fatal: false,
    };
    match endpoint.format {
        ListFormat::Anthropic => {
            let mut models: Vec<ModelInfo> = Vec::new();
            let mut after: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let mut url = format!("{}?limit={PAGE_LIMIT}", endpoint.url);
                if let Some(id) = &after {
                    url.push_str("&after_id=");
                    url.push_str(&utf8_percent_encode(id, QUERY_VALUE).to_string());
                }
                let body = get(client, endpoint.format, &url, key).await?;
                let page = parse_anthropic_page(&body).map_err(parse_failure)?;
                for m in page.models {
                    if !models.iter().any(|seen| seen.id == m.id) {
                        models.push(m);
                    }
                }
                match page.next {
                    Some(next) if after.as_ref() != Some(&next) => after = Some(next),
                    _ => return Ok(models),
                }
            }
            Ok(models)
        }
        ListFormat::OpenAi => {
            let body = get(client, endpoint.format, &endpoint.url, key).await?;
            parse_openai(&body).map_err(parse_failure)
        }
        ListFormat::Ollama => {
            let body = get(client, endpoint.format, &endpoint.url, None).await?;
            parse_ollama(&body).map_err(parse_failure)
        }
    }
}

/// Tries each endpoint in order; the first that answers wins. On failure,
/// one `GET <url>: <reason>` line per endpoint tried.
pub async fn list(
    endpoints: &[Endpoint],
    key: Option<&str>,
) -> Result<Vec<ModelInfo>, Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("guard-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| vec![e.to_string()])?;
    let mut errors = Vec::new();
    for endpoint in endpoints {
        match fetch(&client, endpoint, key).await {
            Ok(models) => return Ok(models),
            Err(failure) => {
                errors.push(format!("GET {}: {}", endpoint.url, failure.message));
                if failure.fatal {
                    break;
                }
            }
        }
    }
    Err(errors)
}

/// `guard explain --list-models [--json]`.
pub fn run(base: &str, key: Option<String>, json: bool) -> CliResult<u8> {
    let endpoints = endpoints(base);
    let needs_key = endpoints
        .first()
        .is_some_and(|e| e.format != ListFormat::Ollama);
    if needs_key && key.is_none() {
        return Err(
            CliError::env(format!("{API_KEY_ENV} is not set")).with_hint(format!(
                "set {API_KEY_ENV} to your API key to list models \
                 ({FALLBACK_API_KEY_ENV} is read as a fallback)"
            )),
        );
    }
    let models = block_on(list(&endpoints, key.as_deref())).map_err(|errors| {
        CliError::env("cannot list models")
            .with_messages(errors)
            .with_hint("check GUARD_EXPLAIN_BASE_URL and the API key")
    })?;
    if json {
        println!("{}", render_json(&models));
    } else if models.is_empty() {
        crate::error::hint("the provider reported no models");
    } else {
        print!("{}", render_text(&models));
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn formats(base: &str) -> Vec<(ListFormat, String)> {
        endpoints(base)
            .into_iter()
            .map(|e| (e.format, e.url))
            .collect()
    }

    #[test]
    fn deepseek_lists_at_the_root_in_openai_format_first() {
        assert_eq!(
            formats("https://api.deepseek.com/anthropic"),
            [
                (ListFormat::OpenAi, "https://api.deepseek.com/models".into()),
                (
                    ListFormat::Anthropic,
                    "https://api.deepseek.com/anthropic/v1/models".into()
                ),
            ]
        );
    }

    #[test]
    fn ollama_is_picked_by_its_port() {
        assert_eq!(
            formats("http://localhost:11434/"),
            [
                (ListFormat::Ollama, "http://localhost:11434/api/tags".into()),
                (
                    ListFormat::OpenAi,
                    "http://localhost:11434/v1/models".into()
                ),
            ]
        );
        assert_eq!(
            endpoints("http://127.0.0.1:11434/v1")[0].url,
            "http://127.0.0.1:11434/api/tags"
        );
    }

    #[test]
    fn anthropic_only_tries_its_own_endpoint() {
        assert_eq!(
            formats("https://api.anthropic.com"),
            [(
                ListFormat::Anthropic,
                "https://api.anthropic.com/v1/models".into()
            )]
        );
    }

    #[test]
    fn an_unknown_host_tries_anthropic_then_openai_then_ollama() {
        assert_eq!(
            formats("https://llm.example.com/proxy"),
            [
                (
                    ListFormat::Anthropic,
                    "https://llm.example.com/proxy/v1/models".into()
                ),
                (
                    ListFormat::OpenAi,
                    "https://llm.example.com/proxy/models".into()
                ),
                (
                    ListFormat::Ollama,
                    "https://llm.example.com/api/tags".into()
                ),
            ]
        );
    }

    #[test]
    fn host_of_strips_scheme_credentials_and_path() {
        assert_eq!(
            host_of(" HTTPS://u:p@API.Example.com/v1 "),
            "api.example.com"
        );
        assert_eq!(host_of("localhost:11434/x"), "localhost:11434");
    }

    #[test]
    fn anthropic_pages_carry_display_names_and_the_next_cursor() {
        let body = r#"{"data":[{"type":"model","id":"claude-opus-5-5","display_name":"Claude Opus 5.5","created_at":"2026-01-01T00:00:00Z"},{"id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5"}],"has_more":true,"first_id":"claude-opus-5-5","last_id":"claude-haiku-5-5"}"#;
        let page = parse_anthropic_page(body).unwrap();
        assert_eq!(page.models.len(), 2);
        assert_eq!(page.models[0].id, "claude-opus-5-5");
        assert_eq!(page.models[0].name.as_deref(), Some("Claude Opus 5.5"));
        assert_eq!(page.next.as_deref(), Some("claude-haiku-5-5"));
        let last = r#"{"data":[{"id":"m"}],"has_more":false,"last_id":"m"}"#;
        assert_eq!(parse_anthropic_page(last).unwrap().next, None);
        assert!(parse_anthropic_page("{}").is_err());
        assert!(parse_anthropic_page("nope").is_err());
    }

    #[test]
    fn openai_lists_take_the_optional_name() {
        let body = r#"{"object":"list","data":[{"id":"deepseek-flash","object":"model","owned_by":"deepseek","name":"DeepSeek-V4.1-Flash"},{"id":"deepseek-v4-pro","object":"model","owned_by":"deepseek"},{"object":"model"}]}"#;
        let models = parse_openai(body).unwrap();
        assert_eq!(
            models,
            [
                ModelInfo {
                    id: "deepseek-flash".into(),
                    name: Some("DeepSeek-V4.1-Flash".into())
                },
                ModelInfo {
                    id: "deepseek-v4-pro".into(),
                    name: None
                },
            ]
        );
        assert!(parse_openai(r#"{"models":[]}"#).is_err());
    }

    #[test]
    fn ollama_tags_list_each_name() {
        let body = r#"{"models":[{"name":"qwen3:8b","model":"qwen3:8b","size":1},{"model":"llama3.2:latest"}]}"#;
        let ids: Vec<_> = parse_ollama(body)
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, ["qwen3:8b", "llama3.2:latest"]);
        assert_eq!(parse_ollama(r#"{"models":[]}"#).unwrap(), []);
        assert!(parse_ollama(r#"{"data":[]}"#).is_err());
    }

    #[test]
    fn text_output_aligns_names_and_json_is_an_array() {
        let models = [
            ModelInfo {
                id: "a".into(),
                name: Some("Model A".into()),
            },
            ModelInfo {
                id: "long-id".into(),
                name: None,
            },
        ];
        assert_eq!(render_text(&models), "a        Model A\nlong-id\n");
        assert_eq!(
            render_json(&models),
            r#"[{"id":"a","name":"Model A"},{"id":"long-id"}]"#
        );
    }

    #[tokio::test]
    async fn anthropic_listing_follows_pagination_with_the_key() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(query_param("after_id", "m2"))
            .and(header("x-api-key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "m3"}], "has_more": false, "last_id": "m3"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "m1"}, {"id": "m2"}], "has_more": true, "last_id": "m2"
            })))
            .mount(&server)
            .await;
        let models = list(&endpoints(&server.uri()), Some("k")).await.unwrap();
        let ids: Vec<_> = models.into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["m1", "m2", "m3"]);
    }

    #[tokio::test]
    async fn a_missing_endpoint_falls_back_but_a_rejected_key_stops() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header("authorization", "Bearer k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list", "data": [{"id": "x"}]
            })))
            .mount(&server)
            .await;
        // /v1/models is unmounted: wiremock answers 404, so OpenAI is next.
        let models = list(&endpoints(&server.uri()), Some("k")).await.unwrap();
        assert_eq!(models[0].id, "x");

        let rejecting = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "invalid key sk-secret"}
            })))
            .mount(&rejecting)
            .await;
        let errors = list(&endpoints(&rejecting.uri()), Some("sk-secret"))
            .await
            .unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("HTTP 401"), "{errors:?}");
        assert!(!errors[0].contains("sk-secret"), "{errors:?}");
        assert!(errors[0].contains("[redacted]"), "{errors:?}");
    }
}
