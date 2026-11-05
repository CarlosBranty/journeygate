//! Gateway spend ingestion (docs/contracts/tokens.md, "Spend ingestion"):
//! pages of the [`ModelGateway`] spend logs (LiteLLM `/spend/logs/v2`) are
//! upserted into `spend_records` by `request_id`, keeping the prompt-cache
//! counters (`cached_tokens`, `cache_creation_input_tokens`) that the A/B
//! report needs. Idempotent: re-ingesting a window replaces rows in place.
//!
//! Runs after every attempt and every 15 minutes (orchestrator). Verified only
//! against `FakeModelGateway`; no provider or real gateway is called in tests.

use crate::pg::map_sqlx_error;
use crate::util::{parse_ts, to_micros};
use guard_core::ports::model_gateway::MAX_SPEND_PAGE_SIZE;
use guard_core::ports::{GatewayError, ModelGateway, SpendQuery, SpendRecord, StoreError};
use sqlx::PgPool;
use sqlx::types::Json;
use std::collections::BTreeMap;
use std::sync::Mutex;
use time::OffsetDateTime;

/// Safety stop for a gateway that never reaches its last page.
pub const MAX_SPEND_PAGES: u32 = 10_000;

/// Where ingested spend goes.
#[async_trait::async_trait]
pub trait SpendSink: Send + Sync {
    /// Upserts `records` (already validated) by `request_id`; returns how many.
    async fn upsert_spend(&self, records: &[SpendRecord]) -> Result<u64, StoreError>;
}

/// One ingestion window (`GET /spend/logs/v2` parameters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpendWindow {
    pub key_alias: Option<String>,
    /// `YYYY-MM-DD HH:MM:SS` UTC (the gateway's format).
    pub start: String,
    pub end: String,
    /// `1..=1000`.
    pub page_size: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpendIngest {
    pub pages: u32,
    pub records: u64,
    /// `(request_id, reason)` of rows that failed validation and were skipped.
    pub rejected: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpendError {
    #[error(transparent)]
    Gateway(#[from] GatewayError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Validated, normalized timestamps of one record.
#[derive(Debug)]
struct Times {
    start: OffsetDateTime,
    end: OffsetDateTime,
}

fn check(record: &SpendRecord) -> Result<Times, String> {
    if record.request_id.trim().is_empty() {
        return Err("request_id is empty".into());
    }
    if record.model.trim().is_empty() {
        return Err("model is empty".into());
    }
    if !(record.spend_usd.is_finite() && record.spend_usd >= 0.0) {
        return Err(format!(
            "spend_usd {} is not a non-negative number",
            record.spend_usd
        ));
    }
    for (name, v) in [
        ("prompt_tokens", record.prompt_tokens),
        ("completion_tokens", record.completion_tokens),
        ("cached_tokens", record.cached_tokens),
        (
            "cache_creation_input_tokens",
            record.cache_creation_input_tokens,
        ),
    ] {
        if i64::try_from(v).is_err() {
            return Err(format!("{name} {v} is out of range"));
        }
    }
    let start = parse_ts("start_time", &record.start_time).map_err(|e| e.to_string())?;
    let end = parse_ts("end_time", &record.end_time).map_err(|e| e.to_string())?;
    Ok(Times {
        start: to_micros(start),
        end: to_micros(end),
    })
}

/// Pages `window` until the last page, validating every row and handing the
/// valid ones to `sink`, one page at a time.
pub async fn ingest_spend(
    gateway: &dyn ModelGateway,
    sink: &dyn SpendSink,
    window: &SpendWindow,
) -> Result<SpendIngest, SpendError> {
    let mut out = SpendIngest::default();
    let mut page = 1;
    loop {
        let query = SpendQuery {
            key_alias: window.key_alias.clone(),
            start: window.start.clone(),
            end: window.end.clone(),
            page,
            page_size: window.page_size,
        };
        let result = gateway.spend_logs(&query).await?;
        out.pages += 1;
        let raw_empty = result.records.is_empty();
        let mut valid = Vec::with_capacity(result.records.len());
        for r in result.records {
            match check(&r) {
                Ok(_) => valid.push(r),
                Err(why) => out.rejected.push((r.request_id.clone(), why)),
            }
        }
        if !valid.is_empty() {
            out.records += sink.upsert_spend(&valid).await?;
        }
        let last = result.total_pages.max(1);
        if page >= last || page >= MAX_SPEND_PAGES || (raw_empty && page > 1) {
            return Ok(out);
        }
        page += 1;
    }
}

/// [`SpendWindow::page_size`] default: the gateway maximum.
pub const DEFAULT_SPEND_PAGE_SIZE: u32 = MAX_SPEND_PAGE_SIZE;

// ---------------------------------------------------------------- sinks

/// In-memory sink (tests, CI-native mode), keyed by `request_id`.
#[derive(Default)]
pub struct MemorySpendSink {
    rows: Mutex<BTreeMap<String, SpendRecord>>,
}

impl MemorySpendSink {
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, request_id: &str) -> Option<SpendRecord> {
        self.lock().get(request_id).cloned()
    }

    pub fn records(&self) -> Vec<SpendRecord> {
        self.lock().values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, SpendRecord>> {
        self.rows.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait::async_trait]
impl SpendSink for MemorySpendSink {
    async fn upsert_spend(&self, records: &[SpendRecord]) -> Result<u64, StoreError> {
        for r in records {
            check(r).map_err(StoreError::Invalid)?;
        }
        let mut rows = self.lock();
        for r in records {
            rows.insert(r.request_id.clone(), r.clone());
        }
        Ok(records.len() as u64)
    }
}

/// `spend_records` on Postgres (orchestrator role).
#[derive(Clone, Debug)]
pub struct PgSpendStore {
    pool: PgPool,
}

impl PgSpendStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Total `spend_usd` of requests that started in `[start, end)` (RFC 3339);
    /// `[scheduler].daily_usd_cap` uses the current UTC day.
    pub async fn spend_usd_between(&self, start: &str, end: &str) -> Result<f64, StoreError> {
        let start = parse_ts("start", start)?;
        let end = parse_ts("end", end)?;
        sqlx::query_scalar(
            "SELECT coalesce(sum(spend_usd), 0)::float8 FROM spend_records
             WHERE start_time >= $1 AND start_time < $2",
        )
        .bind(start)
        .bind(end)
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx_error)
    }
}

fn int(v: u64) -> i64 {
    // `check` guarantees the range.
    i64::try_from(v).unwrap_or(i64::MAX)
}

#[async_trait::async_trait]
impl SpendSink for PgSpendStore {
    async fn upsert_spend(&self, records: &[SpendRecord]) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_error)?;
        for r in records {
            let times = check(r).map_err(StoreError::Invalid)?;
            sqlx::query(
                "INSERT INTO spend_records (request_id, key_alias, run_id, arm, model, spend_usd,
                     prompt_tokens, completion_tokens, cached_tokens, cache_creation_input_tokens,
                     cache_hit, start_time, end_time, metadata)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
                 ON CONFLICT (request_id) DO UPDATE SET
                     key_alias = EXCLUDED.key_alias, run_id = EXCLUDED.run_id,
                     arm = EXCLUDED.arm, model = EXCLUDED.model, spend_usd = EXCLUDED.spend_usd,
                     prompt_tokens = EXCLUDED.prompt_tokens,
                     completion_tokens = EXCLUDED.completion_tokens,
                     cached_tokens = EXCLUDED.cached_tokens,
                     cache_creation_input_tokens = EXCLUDED.cache_creation_input_tokens,
                     cache_hit = EXCLUDED.cache_hit, start_time = EXCLUDED.start_time,
                     end_time = EXCLUDED.end_time, metadata = EXCLUDED.metadata,
                     ingested_at = now()",
            )
            .bind(&r.request_id)
            .bind(r.key_alias.as_deref())
            .bind(r.metadata.get("run_id"))
            .bind(r.metadata.get("arm"))
            .bind(&r.model)
            .bind(r.spend_usd)
            .bind(int(r.prompt_tokens))
            .bind(int(r.completion_tokens))
            .bind(int(r.cached_tokens))
            .bind(int(r.cache_creation_input_tokens))
            .bind(r.cache_hit)
            .bind(times.start)
            .bind(times.end)
            .bind(Json(&r.metadata))
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_error)?;
        }
        tx.commit().await.map_err(map_sqlx_error)?;
        Ok(records.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_rejects_out_of_range_tokens() {
        let r = SpendRecord {
            request_id: "r".into(),
            key_alias: None,
            model: "m".into(),
            spend_usd: 0.0,
            prompt_tokens: u64::MAX,
            completion_tokens: 0,
            cached_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_hit: None,
            start_time: "2026-11-01T00:00:00Z".into(),
            end_time: "2026-11-01T00:00:00Z".into(),
            metadata: BTreeMap::new(),
        };
        assert!(check(&r).unwrap_err().contains("prompt_tokens"));
    }
}
