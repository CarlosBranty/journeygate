//! Schema contract of the Phase 4 migration (`20261101000003_agent_lane.sql`,
//! `docs/contracts/memory.md`): tables, constraints, the append-only
//! `memory_facts` trigger and the role grants that make memory read-only for
//! agents. Live Postgres only: `DATABASE_URL=... cargo test -p guard-store
//! --test agent_lane_schema -- --ignored`.

mod common;

use guard_store::sqlx::{self, Row};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

async fn insert_fact(
    pool: &sqlx::PgPool,
    scope: &str,
    key: Option<&str>,
    topic: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query(
        "INSERT INTO memory_facts (repo, scope, scope_key, topic_key, body, commit_sha, source, valid_from)
         VALUES ('acme/shop', $1, $2, $3, 'body', $4, 'gate_result', now()) RETURNING id",
    )
    .bind(scope)
    .bind(key)
    .bind(topic)
    .bind(SHA)
    .fetch_one(pool)
    .await
    .map(|r| r.get::<i64, _>("id"))
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn agent_lane_tables_and_constraints() {
    let Some(h) = common::pg().await else { return };
    let pool = h.store.pool().clone();

    for table in [
        "memory_facts",
        "memory_fact_candidates",
        "task_queue",
        "agent_runs",
        "spend_records",
    ] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("{table}: {e}"));
        assert_eq!(n, 0);
    }

    let id = insert_fact(&pool, "journey", Some("checkout"), "flaky/pay")
        .await
        .unwrap();
    // scope repo must not carry a scope_key; area/journey must.
    assert!(insert_fact(&pool, "repo", Some("x"), "a").await.is_err());
    assert!(insert_fact(&pool, "area", None, "a").await.is_err());
    assert!(
        insert_fact(&pool, "journey", Some("checkout"), "Bad Topic")
            .await
            .is_err()
    );
    // One live fact per (repo, scope, scope_key, topic_key).
    assert!(
        insert_fact(&pool, "journey", Some("checkout"), "flaky/pay")
            .await
            .is_err()
    );

    // Append-only: the claim of a fact never changes, rows are never deleted.
    assert!(
        sqlx::query("UPDATE memory_facts SET body = 'changed' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM memory_facts WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .is_err()
    );
    // Lifecycle columns may change; invalidated needs invalid_at.
    assert!(
        sqlx::query("UPDATE memory_facts SET status = 'invalidated' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("UPDATE memory_facts SET status = 'invalidated', invalid_at = now() + interval '1 second', review_reason = 'gone' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect("lifecycle update");
    insert_fact(&pool, "journey", Some("checkout"), "flaky/pay")
        .await
        .expect("a new live version after invalidation");

    // One active task per (repo, ticket, role).
    let enqueue = "INSERT INTO task_queue (repo, ticket, role, predicted_files, payload) VALUES ('acme/shop', 'SHOP-1', 'coder', ARRAY['src/a.ts'], '{}')";
    sqlx::query(enqueue).execute(&pool).await.unwrap();
    assert!(sqlx::query(enqueue).execute(&pool).await.is_err());

    // Coder attempts are capped at 3.
    assert!(
        sqlx::query("INSERT INTO agent_runs (run_id, repo, ticket, role, attempt, agent, runtime, model, base_commit) VALUES ('r', 'acme/shop', 'SHOP-1', 'coder', 4, 'coder', 'fake', 'm', $1)")
            .bind(SHA)
            .execute(&pool)
            .await
            .is_err()
    );
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn memory_reader_role_is_select_only() {
    let Some(h) = common::pg().await else { return };
    let pool = h.store.pool().clone();
    let schema: String = sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let privilege = |role: &'static str, table: &'static str, privilege: &'static str| {
        let pool = pool.clone();
        let schema = schema.clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT has_table_privilege($1, $2, $3)")
                .bind(role)
                .bind(format!("{schema}.{table}"))
                .bind(privilege)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert!(privilege("guard_memory_reader", "memory_facts", "SELECT").await);
    for p in ["INSERT", "UPDATE", "DELETE", "TRUNCATE"] {
        assert!(
            !privilege("guard_memory_reader", "memory_facts", p).await,
            "reader has {p}"
        );
    }
    for t in [
        "memory_fact_candidates",
        "task_queue",
        "agent_runs",
        "spend_records",
    ] {
        assert!(
            !privilege("guard_memory_reader", t, "SELECT").await,
            "reader reads {t}"
        );
    }
    for t in [
        "memory_facts",
        "memory_fact_candidates",
        "task_queue",
        "agent_runs",
        "spend_records",
    ] {
        assert!(
            privilege("guard_orchestrator", t, "INSERT").await,
            "orchestrator cannot insert {t}"
        );
    }
    assert!(!privilege("guard_orchestrator", "memory_facts", "DELETE").await);
    h.cleanup().await;
}
