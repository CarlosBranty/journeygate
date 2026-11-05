//! Live Postgres: `PgRunLog` writes `agent_runs` rows the A/B report reads.

mod common;

use guard_agents::pipeline::{AttemptOutcome, AttemptRecord};
use guard_agents::runs::{PgRunLog, RunContext, RunLog};
use guard_core::ports::{Role, TaskStatus, Usage};
use guard_store::sqlx::{self, Row};

fn record(attempt: u32, outcome: AttemptOutcome) -> AttemptRecord {
    AttemptRecord {
        role: Role::Coder,
        attempt,
        agent: "coder".into(),
        runtime: "fake".into(),
        model: "anthropic/claude-sonnet-4-5".into(),
        key_alias: "exp1:memory:SHOP-1:r1".into(),
        sandbox_id: Some("sbx-1".into()),
        status: Some(TaskStatus::Completed),
        outcome: Some(outcome),
        cost_usd: 0.125,
        diff_paths: vec!["src/a.ts".into()],
        changed_lines: 3,
        sealed_spec_sha256: Some("a".repeat(64)),
        detail: Some("ok".into()),
        arm: Some("memory".into()),
        steps: Some(7),
        memory_reads: 2,
        facts_brief_ids: vec![11, 12],
        usage: Some(Usage {
            input_tokens: 1000,
            output_tokens: 100,
            cached_input_tokens: 800,
            cache_creation_input_tokens: 0,
        }),
    }
}

#[tokio::test]
#[ignore = "live Postgres: DATABASE_URL"]
async fn runs_are_upserted_by_run_role_attempt_and_read_by_the_ab_loader() {
    let Some(pg) = common::pg().await else { return };
    let log = PgRunLog::new(pg.pool.clone());
    let ctx = RunContext {
        run_id: "exp1-memory-shop-1-r1".into(),
        task_id: None,
        repo: "acme/shop".into(),
        ticket: "SHOP-1".into(),
        base_commit: "abc1234".into(),
    };
    log.record(&ctx, &record(1, AttemptOutcome::DiffRejected))
        .await
        .unwrap();
    log.record(&ctx, &record(2, AttemptOutcome::GateFailed))
        .await
        .unwrap();
    // Replay of attempt 2 with its final outcome: same row.
    log.record(&ctx, &record(2, AttemptOutcome::Passed))
        .await
        .unwrap();
    let rows = sqlx::query(
        "SELECT attempt, outcome, status, arm, steps, memory_reads, facts_brief_ids,
                cost_usd::float8 AS cost, usage->>'cached_input_tokens' AS cached
         FROM agent_runs ORDER BY attempt",
    )
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[1].get::<Option<String>, _>("outcome").as_deref(),
        Some("passed")
    );
    assert_eq!(rows[1].get::<String, _>("status"), "completed");
    assert_eq!(
        rows[1].get::<Option<String>, _>("arm").as_deref(),
        Some("memory")
    );
    assert_eq!(rows[1].get::<Option<i32>, _>("steps"), Some(7));
    assert_eq!(rows[1].get::<i32, _>("memory_reads"), 2);
    assert_eq!(rows[1].get::<Vec<i64>, _>("facts_brief_ids"), vec![11, 12]);
    assert_eq!(rows[1].get::<f64, _>("cost"), 0.125);
    assert_eq!(
        rows[1].get::<Option<String>, _>("cached").as_deref(),
        Some("800")
    );

    let inputs = guard_store::spend_report::load_ab_inputs(&pg.pool, "exp1")
        .await
        .unwrap();
    assert_eq!(inputs.runs.len(), 2);
    assert_eq!(inputs.runs[1].outcome.as_deref(), Some("passed"));
    pg.cleanup().await;
}

#[tokio::test]
#[ignore = "live Postgres: DATABASE_URL"]
async fn best_of_n_candidates_of_one_attempt_keep_their_own_rows() {
    let Some(pg) = common::pg().await else { return };
    let log = PgRunLog::new(pg.pool.clone());
    let ctx = RunContext {
        run_id: "a1793528430-shop-7".into(),
        task_id: None,
        repo: "acme/shop".into(),
        ticket: "SHOP-7".into(),
        base_commit: "abc1234".into(),
    };
    let mut a = record(1, AttemptOutcome::GateFailed);
    a.agent = "coder-a".into();
    a.key_alias = "a1793528430-shop-7:coder:1:coder-a".into();
    let mut b = record(1, AttemptOutcome::Passed);
    b.agent = "coder-b".into();
    b.key_alias = "a1793528430-shop-7:coder:1:coder-b".into();
    log.record(&ctx, &a).await.unwrap();
    log.record(&ctx, &b).await.unwrap();
    // Replaying candidate A updates its row only.
    log.record(&ctx, &a).await.unwrap();
    let rows = sqlx::query(
        "SELECT agent, outcome, key_alias FROM agent_runs WHERE run_id = $1 ORDER BY agent",
    )
    .bind(&ctx.run_id)
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    let got: Vec<(String, Option<String>)> = rows
        .iter()
        .map(|r| (r.get("agent"), r.get("outcome")))
        .collect();
    assert_eq!(
        got,
        vec![
            ("coder-a".to_string(), Some("gate_failed".to_string())),
            ("coder-b".to_string(), Some("passed".to_string())),
        ]
    );
    pg.cleanup().await;
}

#[tokio::test]
async fn the_memory_run_log_keys_rows_by_agent_too() {
    use guard_agents::runs::MemoryRunLog;
    let log = MemoryRunLog::new();
    let ctx = RunContext {
        run_id: "r".into(),
        task_id: None,
        repo: "acme/shop".into(),
        ticket: "SHOP-7".into(),
        base_commit: "abc1234".into(),
    };
    let mut a = record(1, AttemptOutcome::GateFailed);
    a.agent = "coder-a".into();
    let mut b = record(1, AttemptOutcome::Passed);
    b.agent = "coder-b".into();
    log.record(&ctx, &a).await.unwrap();
    log.record(&ctx, &b).await.unwrap();
    log.record(&ctx, &a).await.unwrap();
    assert_eq!(log.rows().len(), 2);
}
