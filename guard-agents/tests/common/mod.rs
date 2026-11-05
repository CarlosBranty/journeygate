//! Live Postgres harness and the shared `TaskQueue` behavioral suite.
#![allow(dead_code)]

pub mod queue_suite;

use guard_store::sqlx::PgPool;
use guard_store::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// `<prefix>_<pid>_<nanos>_<n>`, unique across processes and calls.
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

/// A pool whose `search_path` is a fresh schema with every guard-store
/// migration applied. Dropped by `cleanup` (or on unwind by `Drop`).
pub struct PgHarness {
    pub pool: PgPool,
    admin: PgPool,
    schema: String,
    url: String,
    cleaned: bool,
}

impl PgHarness {
    pub async fn cleanup(mut self) {
        self.pool.close().await;
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

/// Panics when `DATABASE_URL` is unset (the Postgres suites are `#[ignore]`d,
/// so they only run when asked); `None` with `GUARD_ALLOW_SKIPPED_LIVE_TESTS=1`.
pub async fn pg() -> Option<PgHarness> {
    let Ok(url) = std::env::var("DATABASE_URL") else {
        missing_live_env("DATABASE_URL");
        return None;
    };
    let schema = unique("guard_agents_test");
    let admin = PgPool::connect(&url).await.expect("connect DATABASE_URL");
    guard_store::sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create test schema");
    let options: PgConnectOptions = url.parse().expect("DATABASE_URL");
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .connect_with(options.options([("search_path", schema.as_str())]))
        .await
        .expect("connect test pool");
    guard_store::MIGRATOR
        .run(&pool)
        .await
        .expect("migrations apply");
    Some(PgHarness {
        pool,
        admin,
        schema,
        url,
        cleaned: false,
    })
}
pub mod pipeline_fakes;

/// A live test was asked to run (`--include-ignored`) without `var`: fail,
/// so a CI run that forgot it never reports a vacuous `ok`.
/// `GUARD_ALLOW_SKIPPED_LIVE_TESTS=1` turns that into a skip.
pub fn missing_live_env(var: &str) {
    if std::env::var("GUARD_ALLOW_SKIPPED_LIVE_TESTS").as_deref() == Ok("1") {
        eprintln!("skipped: {var} not set (GUARD_ALLOW_SKIPPED_LIVE_TESTS=1)");
        return;
    }
    panic!("{var} is not set; set it, or GUARD_ALLOW_SKIPPED_LIVE_TESTS=1 to skip live tests");
}
