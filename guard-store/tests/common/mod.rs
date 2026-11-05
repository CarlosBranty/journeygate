//! Shared behavioral suites and live-service harnesses.
#![allow(dead_code)]

pub mod evidence_suite;
pub mod fixtures;
pub mod jobs_suite;
pub mod store_suite;

use guard_store::sqlx::PgPool;
use guard_store::sqlx::postgres::PgConnectOptions;
use guard_store::{PgStore, S3EvidenceStore};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A name unique across processes and calls: `<prefix>_<pid>_<nanos>_<n>`.
pub fn unique(prefix: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "{prefix}_{}_{nanos}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// A `PgStore` migrated into its own fresh schema, dropped by `cleanup`, or
/// by `Drop` when a failing assertion unwinds before `cleanup` runs.
pub struct PgHarness {
    pub store: PgStore,
    admin: PgPool,
    schema: String,
    url: String,
    cleaned: bool,
}

impl PgHarness {
    pub async fn cleanup(mut self) {
        self.store.close().await;
        guard_store::sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop test schema");
        self.admin.close().await;
        self.cleaned = true;
    }
}

impl Drop for PgHarness {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        // Unwinding from a failed assertion: drop the schema on a thread with
        // its own runtime (the test's runtime may be shutting down).
        let (url, schema) = (self.url.clone(), self.schema.clone());
        let _ = std::thread::spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            rt.block_on(async {
                if let Ok(pool) = PgPool::connect(&url).await {
                    let _ = guard_store::sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
                        .execute(&pool)
                        .await;
                    pool.close().await;
                }
            });
        })
        .join();
    }
}

/// `None` when `DATABASE_URL` is unset. The Postgres suites are
/// `#[ignore]`d, so `cargo test` reports them as ignored instead of a silent
/// `ok`; run them with `cargo test -p guard-store -- --ignored`.
pub async fn pg() -> Option<PgHarness> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipped: DATABASE_URL not set");
        return None;
    };
    let schema = unique("guard_test");
    let admin = PgPool::connect(&url).await.expect("connect DATABASE_URL");
    guard_store::sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create test schema");
    let options: PgConnectOptions = url.parse().expect("DATABASE_URL");
    let store = PgStore::connect_with(options.options([("search_path", schema.as_str())]))
        .await
        .expect("connect store");
    let harness = PgHarness {
        store,
        admin,
        schema,
        url,
        cleaned: false,
    };
    harness.store.migrate().await.expect("migrations apply");
    Some(harness)
}

/// A live S3 store and a unique key prefix; `None` when
/// `GUARD_S3_TEST_ENDPOINT` is unset.
pub async fn s3() -> Option<(S3EvidenceStore, String)> {
    let Ok(url) = std::env::var("GUARD_S3_TEST_ENDPOINT") else {
        println!("skipped: GUARD_S3_TEST_ENDPOINT not set");
        return None;
    };
    let store = S3EvidenceStore::from_url(&url).expect("GUARD_S3_TEST_ENDPOINT");
    store
        .ensure_bucket()
        .await
        .expect("bucket exists or is created");
    Some((store, unique("guard-test")))
}
