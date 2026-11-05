//! Persistence adapters.
//!
//! - [`PgStore`]: `DecisionLog`, `GateHistory`, [`FlakyHistory`] and the durable
//!   [`JobQueue`] on Postgres via sqlx, with the schema in
//!   `guard-store/migrations/` (`docs/contracts/decision-log.md`).
//! - [`MemoryStore`]: the same four traits in memory, with the same behavior.
//! - [`S3EvidenceStore`]: `EvidenceStore` on S3-compatible storage (MinIO in
//!   `deploy/`), SigV4-signed `PUT`/`GET` and presigned `GET` URLs
//!   (`docs/contracts/finalize-phase2.md`).
//! - [`FsEvidenceStore`]: `EvidenceStore` on the local filesystem (CI-native mode).
//! - Orchestrator memory (`docs/contracts/memory.md`): [`PgMemoryFacts`]
//!   (write side, orchestrator only) and [`PgMemoryFactsReader`] (read-only
//!   role and sessions); fact writers ([`memory_writers`]), the staleness job
//!   ([`memory_staleness`]), the facts brief ([`memory_brief`]) and the Engram
//!   export ([`memory_export`]).
//! - Token savings (`docs/contracts/tokens.md`): gateway spend ingestion into
//!   `spend_records` ([`spend`]) and the 4-arm A/B report ([`spend_report`]).
//!
//! The shared behavioral suites in `tests/` run against every implementation.
//! Tests that need a live Postgres or MinIO are gated on `DATABASE_URL` and
//! `GUARD_S3_TEST_ENDPOINT` and skip cleanly when those are unset.

mod flaky;
mod fs;
mod jobs;
mod memory;
pub mod memory_brief;
pub mod memory_export;
mod memory_facts;
pub mod memory_staleness;
pub mod memory_writers;
mod pg;
mod s3;
mod sigv4;
pub mod spend;
pub mod spend_report;
mod util;

pub use flaky::{DEFAULT_FLAKY_WINDOW, FlakyHistory, FlakyQuery, FlakyStat};
pub use fs::FsEvidenceStore;
pub use jobs::{Enqueued, Job, JobId, JobQueue, JobStatus, MAX_DELAY, NewJob};
pub use memory::MemoryStore;
pub use memory_facts::{
    MEMORY_READ_DATABASE_URL_ENV, PgMemoryFacts, PgMemoryFactsReader, StoredFact,
};
pub use pg::{DATABASE_URL_ENV, MIGRATOR, PgStore, map_sqlx_error};
pub use s3::{DEFAULT_REGION, S3Config, S3EvidenceStore};
/// Re-exported so callers (and tests) use the exact pinned sqlx.
pub use sqlx;

use guard_core::ports::{EvidenceError, EvidenceStore};
use std::sync::Arc;

/// Builds the `EvidenceStore` a storage URL names:
/// `s3+http(s)://...` → [`S3EvidenceStore`], `file:///<dir>` → [`FsEvidenceStore`].
pub fn evidence_store_from_url(url: &str) -> Result<Arc<dyn EvidenceStore>, EvidenceError> {
    if url.starts_with("s3+http://") || url.starts_with("s3+https://") {
        return Ok(Arc::new(S3EvidenceStore::from_url(url)?));
    }
    if url.starts_with("file://") {
        // Only host-less (or localhost) URLs: `to_file_path` would build a UNC
        // path from a host on Windows (and url 2.5.7 asserts on some of them).
        let path = url::Url::parse(url)
            .ok()
            .filter(|u| matches!(u.host_str(), None | Some("") | Some("localhost")))
            .and_then(|u| u.to_file_path().ok())
            .ok_or_else(|| {
                EvidenceError::Unconfigured(format!("{url:?} is not an absolute file:// URL"))
            })?;
        return Ok(Arc::new(FsEvidenceStore::new(path)));
    }
    Err(EvidenceError::Unconfigured(
        "storage URL must start with s3+http://, s3+https:// or file://".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_picks_the_adapter_by_scheme() {
        assert!(evidence_store_from_url("s3+http://a:b@localhost:9000/bucket").is_ok());
        let dir = std::env::temp_dir();
        let file_url = url::Url::from_directory_path(&dir).unwrap();
        assert!(evidence_store_from_url(file_url.as_str()).is_ok());
        for bad in ["ftp://x/y", "", "file://relative"] {
            assert!(
                matches!(
                    evidence_store_from_url(bad),
                    Err(EvidenceError::Unconfigured(_))
                ),
                "{bad}"
            );
        }
    }
}
