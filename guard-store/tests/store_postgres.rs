//! The shared suites against `PgStore`, each test in its own schema.
//! Every test is `#[ignore]`d (it needs a live Postgres): `cargo test` lists
//! them as ignored. Run them with `DATABASE_URL=... cargo test -p guard-store
//! --test store_postgres -- --ignored`; without `DATABASE_URL` they still
//! return early.

mod common;

use common::{jobs_suite, store_suite};
use std::sync::Arc;

macro_rules! suite {
    ($module:ident: $($name:ident),* $(,)?) => { $(
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        #[ignore = "needs DATABASE_URL"]
        async fn $name() {
            let Some(h) = common::pg().await else { return };
            $module::$name(&h.store).await;
            h.cleanup().await;
        }
    )* };
}

suite!(store_suite:
    decision_round_trips_through_calibration,
    decision_ids_increase,
    failed_engine_decision_is_recorded,
    decision_without_answers_nor_error_is_invalid,
    decision_with_bad_timestamp_is_invalid,
    calibration_keeps_latest_decision_per_ticket,
    calibration_joins_latest_human_label,
    human_label_with_empty_optionals_round_trips,
    calibration_filters,
    calibration_mode_filter_applies_to_latest_decision,
    calibration_since_must_be_rfc3339,
    gate_result_round_trips,
    gate_result_upsert_keeps_id_and_moves_to_front,
    gate_result_same_run_id_other_repo_is_distinct,
    recent_gate_results_limit,
    latest_for_pr_picks_newest_of_that_pr,
    flaky_stats_over_window,
    flaky_stats_follow_upserted_runs,
);

suite!(jobs_suite:
    enqueue_then_claim,
    claim_order_is_priority_then_fifo,
    dedupe_key_returns_active_job,
    complete_requires_the_lease_holder,
    fail_retries_until_max_attempts_then_dead,
    fail_with_backoff_delays_the_retry,
    expired_lease_is_reclaimed,
    expired_lease_at_max_attempts_goes_dead,
    heartbeat_extends_only_own_lease,
    unknown_job,
    invalid_new_job_is_rejected,
);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs DATABASE_URL"]
async fn concurrent_claims_are_exclusive() {
    let Some(h) = common::pg().await else { return };
    let store = Arc::new(h.store.clone());
    jobs_suite::concurrent_claims_are_exclusive(store).await;
    h.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs DATABASE_URL"]
async fn migrations_are_idempotent() {
    let Some(h) = common::pg().await else { return };
    h.store.migrate().await.expect("second run is a no-op");
    h.cleanup().await;
}
