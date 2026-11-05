//! `ModelGateway`: the self-hosted LiteLLM proxy every sandbox talks to
//! (architecture section 16; `docs/contracts/tokens.md`).
//!
//! One virtual key per (run, role, attempt) or per A/B arm, carrying its
//! models, budget, rate limits and expiry. Real provider keys live only in the
//! gateway. Spend is paged from `/spend/logs/v2` into Postgres (`spend_records`).

use super::secret::Secret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

/// Largest `page_size` of `/spend/logs/v2`.
pub const MAX_SPEND_PAGE_SIZE: u32 = 1000;

/// `POST /key/generate` input.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyRequest {
    /// `key_alias`: `<run_id>:<role>:<attempt>` or `<experiment>:<arm>:<task>`. Unique.
    pub alias: String,
    /// Gateway model names the key may call. Non-empty.
    pub models: Vec<String>,
    /// `max_budget` in USD. Positive and finite.
    pub max_budget_usd: f64,
    pub rpm_limit: Option<u32>,
    pub tpm_limit: Option<u64>,
    pub max_parallel_requests: Option<u32>,
    /// `duration` (sent as `<seconds>s`). Positive.
    pub duration: Duration,
    /// Copied into key metadata and every spend log row (`run_id`, `ticket`,
    /// `role`, `arm`, `repo`, `commit`).
    pub metadata: BTreeMap<String, String>,
}

impl KeyRequest {
    pub fn validate(&self) -> Result<(), String> {
        if self.alias.trim().is_empty() {
            return Err("alias is empty".into());
        }
        if self.models.is_empty() || self.models.iter().any(|m| m.trim().is_empty()) {
            return Err("models must be a non-empty list of model names".into());
        }
        if !(self.max_budget_usd.is_finite() && self.max_budget_usd > 0.0) {
            return Err("max_budget_usd must be positive and finite".into());
        }
        if self.duration.is_zero() {
            return Err("duration must be positive".into());
        }
        if [self.rpm_limit, self.max_parallel_requests].contains(&Some(0))
            || self.tpm_limit == Some(0)
        {
            return Err("rate limits must be positive when set".into());
        }
        Ok(())
    }
}

/// A minted key. `token` is the bearer value injected into the sandbox env;
/// it never serializes and prints as `***`.
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualKey {
    /// Gateway-side identifier used for revocation (the hashed token / key name).
    pub key_id: String,
    pub alias: String,
    pub token: Secret,
    pub models: Vec<String>,
    pub max_budget_usd: f64,
    /// RFC 3339, when the gateway reports it.
    pub expires_at: Option<String>,
}

/// A live key as `GET /key/list` reports it (no bearer value).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeyInfo {
    /// Same identifier as [`VirtualKey::key_id`]: what [`ModelGateway::revoke`] takes.
    pub key_id: String,
    pub alias: Option<String>,
    /// RFC 3339, when the gateway reports it.
    pub created_at: Option<String>,
    /// RFC 3339; `None` = never expires (or not reported).
    pub expires_at: Option<String>,
    /// Key metadata as minted ([`KeyRequest::metadata`]: `run_id`, `role`, ...).
    pub metadata: BTreeMap<String, String>,
}

/// One `/spend/logs/v2` row, normalized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendRecord {
    pub request_id: String,
    pub key_alias: Option<String>,
    pub model: String,
    pub spend_usd: f64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Cache reads (OpenAI `prompt_tokens_details.cached_tokens`, Anthropic `cache_read_input_tokens`).
    pub cached_tokens: u64,
    /// Anthropic cache writes.
    pub cache_creation_input_tokens: u64,
    /// The gateway's exact-match response cache (not provider prompt caching).
    pub cache_hit: Option<bool>,
    /// RFC 3339 UTC.
    pub start_time: String,
    pub end_time: String,
    /// Key metadata echoed by the gateway (`run_id`, `arm`, ...).
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpendQuery {
    pub key_alias: Option<String>,
    /// `YYYY-MM-DD HH:MM:SS` UTC (the gateway's format).
    pub start: String,
    pub end: String,
    /// 1-based.
    pub page: u32,
    /// `1..=MAX_SPEND_PAGE_SIZE`.
    pub page_size: u32,
}

impl SpendQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.page == 0 {
            return Err("page starts at 1".into());
        }
        if !(1..=MAX_SPEND_PAGE_SIZE).contains(&self.page_size) {
            return Err(format!("page_size must be in 1..={MAX_SPEND_PAGE_SIZE}"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendPage {
    pub records: Vec<SpendRecord>,
    pub page: u32,
    /// `0` when there are no rows.
    pub total_pages: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewayError {
    /// Gateway URL or master-key env var missing.
    #[error("gateway not configured: {0}")]
    Unconfigured(String),
    /// The request failed local validation and was never sent.
    #[error("invalid gateway request: {0}")]
    InvalidRequest(String),
    #[error("key {0} not found")]
    NotFound(String),
    #[error("gateway rejected the request (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("gateway transport error: {0}")]
    Transport(String),
}

#[async_trait::async_trait]
pub trait ModelGateway: Send + Sync {
    /// `POST /key/generate`. Validates the request first.
    async fn mint_key(&self, request: &KeyRequest) -> Result<VirtualKey, GatewayError>;

    /// `POST /key/delete`. Unknown key = `NotFound`.
    async fn revoke(&self, key_id: &str) -> Result<(), GatewayError>;

    /// `GET /spend/logs/v2`, one page.
    async fn spend_logs(&self, query: &SpendQuery) -> Result<SpendPage, GatewayError>;

    /// `GET /key/list`: every live key whose alias starts with `alias_prefix`
    /// (`""` = every key), all pages, sorted by alias then key id. Used by the
    /// key reaper to find keys a crashed attempt never revoked. Gateways that
    /// cannot list keys keep the default (`Unconfigured`).
    async fn list_keys(&self, alias_prefix: &str) -> Result<Vec<KeyInfo>, GatewayError> {
        let _ = alias_prefix;
        Err(GatewayError::Unconfigured(
            "this gateway cannot list keys".into(),
        ))
    }
}
