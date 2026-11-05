//! `LiteLlmGateway`: `ModelGateway` over the LiteLLM proxy management API.
//!
//! Sources (checked 2026-11-01): docs.litellm.ai `proxy/virtual_keys`,
//! `proxy/users` (rate-limit fields), `proxy/cost_tracking` (`/spend/logs/v2`:
//! `start_date`/`end_date` as `YYYY-MM-DD HH:MM:SS`, `page` from 1,
//! `page_size` up to 1000, response `{data, total, page, page_size,
//! total_pages}`), and the proxy source (BerriAI/litellm main @ 60fc121):
//! - `GenerateKeyResponse` (`proxy/_types.py`): `key` (the bearer value),
//!   `token` / `token_id` (the hashed id used for revocation), `key_alias`,
//!   `expires`.
//! - `/key/delete` (`key_management_endpoints.py`): body `{"keys": [...]}`,
//!   answers `{"deleted_keys": [...]}`, 404 when a key does not exist.
//! - `/spend/logs/v2` (`spend_management_endpoints.py::ui_view_spend_logs`):
//!   the `key_alias` filter is a **substring** match
//!   (`metadata->>'user_api_key_alias' LIKE %alias%`), so rows are filtered
//!   again here to the exact alias (`run:coder:1` would otherwise also match
//!   `run:coder:10`). Rows are `SpendLogsPayload`: `request_id`, `spend`,
//!   `prompt_tokens`, `completion_tokens`, `startTime`, `endTime`, `model`,
//!   `model_group`, `cache_hit` (a string), `metadata` (an object, or a JSON
//!   string) with `user_api_key_alias`, `spend_logs_metadata` and
//!   `usage_object`.
//!
//! Key metadata: `SpendLogsMetadata` has no field echoing the key's
//! `metadata`, so [`SpendRecord::metadata`] is filled from
//! `metadata.spend_logs_metadata` (per-request tags) and, if a proxy version
//! adds it, `metadata.user_api_key_metadata`. Attribution relies on the key
//! alias, which every row carries.
//!
//! The master key goes only into the `Authorization` header; it never appears
//! in errors (reqwest errors carry the URL, which has no credentials).

use guard_core::ports::model_gateway::{
    GatewayError, KeyInfo, KeyRequest, ModelGateway, SpendPage, SpendQuery, SpendRecord, VirtualKey,
};
use guard_core::ports::secret::Secret;
use reqwest::{StatusCode, Url};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

const ERROR_BODY_MAX: usize = 2000;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// `size` of `GET /key/list` (the proxy caps it at 100).
const KEY_LIST_PAGE_SIZE: u32 = 100;
/// Pages of `GET /key/list` read at most (100 000 keys).
const KEY_LIST_MAX_PAGES: u32 = 1000;

pub struct LiteLlmGateway {
    http: reqwest::Client,
    base: Url,
    master_key: Secret,
}

impl std::fmt::Debug for LiteLlmGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiteLlmGateway")
            .field("base", &self.base.as_str())
            .field("master_key", &self.master_key)
            .finish()
    }
}

impl LiteLlmGateway {
    pub fn new(base_url: &str, master_key: Secret) -> Result<Self, GatewayError> {
        let mut base = Url::parse(base_url)
            .map_err(|e| GatewayError::Unconfigured(format!("gateway url {base_url:?}: {e}")))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(GatewayError::Unconfigured(format!(
                "gateway url {base_url:?} must be http(s)://host"
            )));
        }
        if !base.path().ends_with('/') {
            let p = format!("{}/", base.path());
            base.set_path(&p);
        }
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .map_err(|e| GatewayError::Unconfigured(format!("http client: {e}")))?;
        Ok(Self {
            http,
            base,
            master_key,
        })
    }

    /// Reads the master key from the env var named `master_key_env`
    /// (`[gateway].master_key_env`, usually `LITELLM_MASTER_KEY`).
    pub fn from_env(base_url: &str, master_key_env: &str) -> Result<Self, GatewayError> {
        let key = std::env::var(master_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| {
                GatewayError::Unconfigured(format!("env var {master_key_env} is not set"))
            })?;
        Self::new(base_url, Secret::new(key))
    }

    fn url(&self, path: &str) -> Result<Url, GatewayError> {
        self.base
            .join(path)
            .map_err(|e| GatewayError::Unconfigured(format!("{path}: {e}")))
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, GatewayError> {
        let resp = req
            .bearer_auth(self.master_key.expose())
            .send()
            .await
            .map_err(|e| GatewayError::Transport(e.without_url().to_string()))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| GatewayError::Transport(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(GatewayError::Rejected {
                status: status.as_u16(),
                body: truncate(&body),
            });
        }
        serde_json::from_str(&body)
            .map_err(|e| GatewayError::Transport(format!("gateway answered non-JSON: {e}")))
    }
}

fn truncate(s: &str) -> String {
    let mut end = s.len().min(ERROR_BODY_MAX);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

pub(crate) fn key_body(r: &KeyRequest) -> Value {
    let mut body = Map::new();
    body.insert("key_alias".into(), json!(r.alias));
    body.insert("models".into(), json!(r.models));
    body.insert("max_budget".into(), json!(r.max_budget_usd));
    if let Some(v) = r.rpm_limit {
        body.insert("rpm_limit".into(), json!(v));
    }
    if let Some(v) = r.tpm_limit {
        body.insert("tpm_limit".into(), json!(v));
    }
    if let Some(v) = r.max_parallel_requests {
        body.insert("max_parallel_requests".into(), json!(v));
    }
    body.insert(
        "duration".into(),
        json!(format!("{}s", r.duration.as_secs().max(1))),
    );
    body.insert("metadata".into(), json!(r.metadata));
    Value::Object(body)
}

fn as_u64(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_f64().filter(|f| *f >= 0.0).map(|f| f as u64))
        .unwrap_or(0)
}

/// LiteLLM timestamps (`2026-11-01T10:00:00.123000Z`, `...+00:00`, or naive
/// `2026-11-01 11:00:00`, which the proxy stores in UTC) as RFC 3339 UTC.
fn rfc3339(raw: &str) -> String {
    let s = raw.trim().replacen(' ', "T", 1);
    if let Some(body) = s.strip_suffix("+00:00") {
        return format!("{body}Z");
    }
    let has_offset = s.ends_with('Z')
        || s.rsplit_once('T')
            .is_some_and(|(_, t)| t.contains('+') || t.contains('-'));
    if has_offset { s } else { format!("{s}Z") }
}

fn metadata_object(row: &Value) -> Map<String, Value> {
    match &row["metadata"] {
        Value::Object(m) => m.clone(),
        Value::String(s) => serde_json::from_str::<Value>(s)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        _ => Map::new(),
    }
}

fn string_map(v: Option<&Value>, into: &mut BTreeMap<String, String>) {
    if let Some(Value::Object(m)) = v {
        for (k, v) in m {
            let s = match v {
                Value::String(s) => s.clone(),
                Value::Null => continue,
                other => other.to_string(),
            };
            into.insert(k.clone(), s);
        }
    }
}

/// One `UserAPIKeyAuth` of `GET /key/list?return_full_object=true`; `None`
/// for a bare token string (a proxy that ignored `return_full_object`).
fn key_info(v: &Value) -> Option<KeyInfo> {
    let key_id = v["token"].as_str().filter(|s| !s.is_empty())?;
    let mut metadata = BTreeMap::new();
    string_map(v.get("metadata"), &mut metadata);
    Some(KeyInfo {
        key_id: key_id.to_string(),
        alias: v["key_alias"].as_str().map(str::to_string),
        created_at: v["created_at"].as_str().map(rfc3339),
        expires_at: v["expires"].as_str().map(rfc3339),
        metadata,
    })
}

pub(crate) fn spend_record(row: &Value) -> SpendRecord {
    let meta = metadata_object(row);
    let usage = meta.get("usage_object").cloned().unwrap_or(Value::Null);
    let extra = meta
        .get("additional_usage_values")
        .cloned()
        .unwrap_or(Value::Null);
    let cached = [
        &usage["prompt_tokens_details"]["cached_tokens"],
        &usage["cache_read_input_tokens"],
        &extra["cache_read_input_tokens"],
    ]
    .into_iter()
    .map(as_u64)
    .max()
    .unwrap_or(0);
    let created = as_u64(&usage["cache_creation_input_tokens"])
        .max(as_u64(&extra["cache_creation_input_tokens"]));
    let cache_hit = match &row["cache_hit"] {
        Value::Bool(b) => Some(*b),
        Value::String(s) if s.eq_ignore_ascii_case("true") => Some(true),
        Value::String(s) if s.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    };
    let model = row["model_group"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| row["model"].as_str())
        .unwrap_or_default()
        .to_string();
    let mut metadata = BTreeMap::new();
    string_map(meta.get("user_api_key_metadata"), &mut metadata);
    string_map(meta.get("spend_logs_metadata"), &mut metadata);
    SpendRecord {
        request_id: row["request_id"].as_str().unwrap_or_default().to_string(),
        key_alias: meta
            .get("user_api_key_alias")
            .and_then(Value::as_str)
            .map(str::to_string),
        model,
        spend_usd: row["spend"].as_f64().unwrap_or(0.0),
        prompt_tokens: as_u64(&row["prompt_tokens"]),
        completion_tokens: as_u64(&row["completion_tokens"]),
        cached_tokens: cached,
        cache_creation_input_tokens: created,
        cache_hit,
        start_time: rfc3339(row["startTime"].as_str().unwrap_or_default()),
        end_time: rfc3339(row["endTime"].as_str().unwrap_or_default()),
        metadata,
    }
}

#[async_trait::async_trait]
impl ModelGateway for LiteLlmGateway {
    async fn mint_key(&self, request: &KeyRequest) -> Result<VirtualKey, GatewayError> {
        request.validate().map_err(GatewayError::InvalidRequest)?;
        let v = self
            .send(
                self.http
                    .post(self.url("key/generate")?)
                    .json(&key_body(request)),
            )
            .await?;
        let token = v["key"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
            GatewayError::Transport("/key/generate answered without `key`".into())
        })?;
        let key_id = v["token"]
            .as_str()
            .or_else(|| v["token_id"].as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                GatewayError::Transport(
                    "/key/generate answered without `token`/`token_id` (needed to revoke)".into(),
                )
            })?;
        Ok(VirtualKey {
            key_id: key_id.to_string(),
            alias: v["key_alias"]
                .as_str()
                .unwrap_or(&request.alias)
                .to_string(),
            token: Secret::new(token),
            models: request.models.clone(),
            max_budget_usd: request.max_budget_usd,
            expires_at: v["expires"].as_str().map(str::to_string),
        })
    }

    async fn revoke(&self, key_id: &str) -> Result<(), GatewayError> {
        let req = self
            .http
            .post(self.url("key/delete")?)
            .json(&json!({ "keys": [key_id] }));
        let v = match self.send(req).await {
            Ok(v) => v,
            Err(GatewayError::Rejected { status, .. })
                if status == StatusCode::NOT_FOUND.as_u16() =>
            {
                return Err(GatewayError::NotFound(key_id.to_string()));
            }
            Err(e) => return Err(e),
        };
        let deleted = v["deleted_keys"].as_array().is_some_and(|a| !a.is_empty());
        if deleted {
            Ok(())
        } else {
            Err(GatewayError::NotFound(key_id.to_string()))
        }
    }

    /// Pages `GET /key/list?return_full_object=true&size=100` and filters
    /// by prefix here: the server's `key_alias` filter is exact unless
    /// `substring_matching=true`, which older proxies do not know.
    async fn list_keys(&self, alias_prefix: &str) -> Result<Vec<KeyInfo>, GatewayError> {
        let mut keys = vec![];
        let mut page = 1;
        loop {
            let params = [
                ("return_full_object", "true".to_string()),
                ("page", page.to_string()),
                ("size", KEY_LIST_PAGE_SIZE.to_string()),
            ];
            let v = self
                .send(self.http.get(self.url("key/list")?).query(&params))
                .await?;
            keys.extend(
                v["keys"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(key_info)
                    .filter(|k| {
                        k.alias
                            .as_deref()
                            .map_or(alias_prefix.is_empty(), |a| a.starts_with(alias_prefix))
                    }),
            );
            let total_pages = v["total_pages"].as_u64().unwrap_or(1);
            if u64::from(page) >= total_pages {
                break;
            }
            if page >= KEY_LIST_MAX_PAGES {
                return Err(GatewayError::Transport(format!(
                    "/key/list has more than {KEY_LIST_MAX_PAGES} pages of {KEY_LIST_PAGE_SIZE} keys"
                )));
            }
            page += 1;
        }
        keys.sort_by(|a, b| (&a.alias, &a.key_id).cmp(&(&b.alias, &b.key_id)));
        keys.dedup_by(|a, b| a.key_id == b.key_id);
        Ok(keys)
    }

    async fn spend_logs(&self, query: &SpendQuery) -> Result<SpendPage, GatewayError> {
        query.validate().map_err(GatewayError::InvalidRequest)?;
        let mut params: Vec<(&str, String)> = vec![
            ("start_date", query.start.clone()),
            ("end_date", query.end.clone()),
            ("page", query.page.to_string()),
            ("page_size", query.page_size.to_string()),
        ];
        if let Some(alias) = &query.key_alias {
            params.push(("key_alias", alias.clone()));
        }
        let v = self
            .send(self.http.get(self.url("spend/logs/v2")?).query(&params))
            .await?;
        let rows = v["data"].as_array().cloned().unwrap_or_default();
        let records = rows
            .iter()
            .map(spend_record)
            .filter(|r| query.key_alias.is_none() || r.key_alias == query.key_alias)
            .collect();
        let total_pages = match v["total_pages"].as_u64() {
            Some(n) => n as u32,
            None => {
                let total = as_u64(&v["total"]);
                let size = as_u64(&v["page_size"]).max(1);
                total.div_ceil(size) as u32
            }
        };
        Ok(SpendPage {
            records,
            page: v["page"].as_u64().map_or(query.page, |p| p as u32),
            total_pages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_normalize_to_utc() {
        assert_eq!(rfc3339("2026-11-01 11:00:00"), "2026-11-01T11:00:00Z");
        assert_eq!(rfc3339("2026-11-01T10:00:04+00:00"), "2026-11-01T10:00:04Z");
        assert_eq!(
            rfc3339("2026-11-01T10:00:00.123000Z"),
            "2026-11-01T10:00:00.123000Z"
        );
        assert_eq!(
            rfc3339("2026-11-01T10:00:00-05:00"),
            "2026-11-01T10:00:00-05:00"
        );
    }

    #[test]
    fn debug_never_prints_the_master_key() {
        let g = LiteLlmGateway::new("http://gw:4000", Secret::new("sk-master")).unwrap();
        assert!(!format!("{g:?}").contains("sk-master"));
    }
}
