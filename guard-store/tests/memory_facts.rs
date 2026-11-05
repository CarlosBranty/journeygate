//! `MemoryFactsWriter` / `MemoryFactsReader` behavior (docs/contracts/memory.md).
//!
//! The same suite runs against the in-memory fake (always) and `PgMemoryFacts`
//! (live Postgres, `#[ignore]`d: `DATABASE_URL=... cargo test -p guard-store
//! --test memory_facts -- --include-ignored`). The Postgres-only tests check
//! the read-only role (the reader cannot INSERT/UPDATE), bi-temporal queries
//! and concurrent writers.

mod common;

use guard_core::ports::fakes::InMemoryMemoryFacts;
use guard_core::ports::memory_facts::{MAX_FACT_BODY_BYTES, MAX_FACTS_PAGE};
use guard_core::ports::*;
use guard_store::sqlx::postgres::PgConnectOptions;
use guard_store::sqlx::{self, Row};
use guard_store::{PgMemoryFacts, PgMemoryFactsReader};
use std::sync::Arc;

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SHA2: &str = "89abcdef0123456789abcdef0123456789abcdef";
const REPO: &str = "acme/shop";

fn fact(scope: FactScope, key: Option<&str>, topic: &str, body: &str, valid_from: &str) -> NewFact {
    NewFact {
        repo: REPO.into(),
        scope,
        scope_key: key.map(str::to_string),
        topic_key: topic.into(),
        body: body.into(),
        commit_sha: SHA.into(),
        run_id: Some("run-1".into()),
        evidence_uri: Some("s3://guard-evidence/runs/run-1/trace.zip".into()),
        source: FactSource::GateResult,
        valid_from: valid_from.into(),
        review_after: None,
        referenced_paths: vec!["src/checkout/pay.ts".into()],
    }
}

fn journey(topic: &str, body: &str) -> NewFact {
    fact(
        FactScope::Journey,
        Some("checkout"),
        topic,
        body,
        "2026-10-01T00:00:00Z",
    )
}

fn all_statuses(repo: &str) -> FactsQuery {
    FactsQuery {
        statuses: vec![
            FactStatus::Active,
            FactStatus::NeedsReview,
            FactStatus::Invalidated,
        ],
        limit: 50,
        ..FactsQuery::for_repo(repo)
    }
}

// ---------------------------------------------------------------- suite

mod suite {
    use super::*;

    pub async fn record_and_read(w: &dyn MemoryFactsWriter) {
        let new = journey("flaky/pay", "Pay button is flaky under slow 3DS");
        let id = w.record_fact(&new).await.unwrap();
        let got = w.get_fact(id).await.unwrap().expect("fact");
        assert_eq!(got.id, id);
        assert_eq!(got.repo, REPO);
        assert_eq!(got.scope, FactScope::Journey);
        assert_eq!(got.scope_key.as_deref(), Some("checkout"));
        assert_eq!(got.topic_key, "flaky/pay");
        assert_eq!(got.body, new.body);
        assert_eq!(got.commit_sha, SHA);
        assert_eq!(got.run_id.as_deref(), Some("run-1"));
        assert_eq!(got.evidence_uri, new.evidence_uri);
        assert_eq!(got.source, FactSource::GateResult);
        assert_eq!(got.valid_from, "2026-10-01T00:00:00Z");
        assert_eq!(got.status, FactStatus::Active);
        assert_eq!(got.invalid_at, None);
        assert_eq!(got.supersedes, None);
        assert_eq!(got.referenced_paths, ["src/checkout/pay.ts"]);
        assert!(w.get_fact(FactId(i64::MAX)).await.unwrap().is_none());

        w.record_fact(&fact(
            FactScope::Area,
            Some("catalog"),
            "selector/card",
            "Use data-testid=product-card",
            "2026-10-01T00:00:00Z",
        ))
        .await
        .unwrap();
        w.record_fact(&fact(
            FactScope::Repo,
            None,
            "decision/shop-1",
            "SHOP-1 is human lane",
            "2026-10-01T00:00:00Z",
        ))
        .await
        .unwrap();

        let page = w.list_facts(&FactsQuery::for_repo(REPO)).await.unwrap();
        assert_eq!(page.facts.len(), 3);
        assert!(page.facts.windows(2).all(|p| p[0].id < p[1].id));
        assert_eq!(page.next_cursor, None);

        let q = FactsQuery {
            scope: Some(FactScope::Area),
            scope_key: Some("catalog".into()),
            ..FactsQuery::for_repo(REPO)
        };
        let page = w.list_facts(&q).await.unwrap();
        assert_eq!(page.facts.len(), 1);
        assert_eq!(page.facts[0].topic_key, "selector/card");

        let q = FactsQuery {
            topic_prefix: Some("flaky/".into()),
            ..FactsQuery::for_repo(REPO)
        };
        assert_eq!(w.list_facts(&q).await.unwrap().facts.len(), 1);
        let other = w
            .list_facts(&FactsQuery::for_repo("other/repo"))
            .await
            .unwrap();
        assert!(other.facts.is_empty());
    }

    pub async fn a_new_version_supersedes(w: &dyn MemoryFactsWriter) {
        let v1 = w
            .record_fact(&journey("failure/guard-fp-1a2b3c4d", "times out"))
            .await
            .unwrap();
        let mut second = journey("failure/guard-fp-1a2b3c4d", "times out at step 3");
        second.valid_from = "2026-10-15T00:00:00Z".into();
        second.commit_sha = SHA2.into();
        let v2 = w.record_fact(&second).await.unwrap();
        assert_ne!(v1, v2);

        let old = w.get_fact(v1).await.unwrap().unwrap();
        assert_eq!(old.status, FactStatus::Invalidated);
        assert_eq!(old.invalid_at.as_deref(), Some("2026-10-15T00:00:00Z"));
        let new = w.get_fact(v2).await.unwrap().unwrap();
        assert_eq!(new.supersedes, Some(v1));
        assert_eq!(new.status, FactStatus::Active);

        let live = w.list_facts(&FactsQuery::for_repo(REPO)).await.unwrap();
        assert_eq!(
            live.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![v2]
        );
        let history = w
            .topic_history(REPO, "failure/guard-fp-1a2b3c4d")
            .await
            .unwrap();
        assert_eq!(
            history.iter().map(|f| f.id).collect::<Vec<_>>(),
            vec![v1, v2]
        );
    }

    pub async fn recording_the_same_claim_is_idempotent(w: &dyn MemoryFactsWriter) {
        let new = journey("fingerprint/guard-fp-00000001", "net::ERR_TIMED_OUT");
        let a = w.record_fact(&new).await.unwrap();
        let b = w.record_fact(&new).await.unwrap();
        assert_eq!(a, b);
        let history = w
            .topic_history(REPO, "fingerprint/guard-fp-00000001")
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
    }

    pub async fn lifecycle(w: &dyn MemoryFactsWriter) {
        let id = w.record_fact(&journey("flaky/pay", "flaky")).await.unwrap();
        w.mark_needs_review(id, "src/checkout/pay.ts changed in 89abcde")
            .await
            .unwrap();
        // No-op on needs_review: the first reason stays.
        w.mark_needs_review(id, "other").await.unwrap();
        let f = w.get_fact(id).await.unwrap().unwrap();
        assert_eq!(f.status, FactStatus::NeedsReview);
        assert_eq!(
            f.review_reason.as_deref(),
            Some("src/checkout/pay.ts changed in 89abcde")
        );
        // needs_review facts are still listed by default.
        assert_eq!(
            w.list_facts(&FactsQuery::for_repo(REPO))
                .await
                .unwrap()
                .facts
                .len(),
            1
        );

        w.confirm(id, Some("2026-12-01T00:00:00Z")).await.unwrap();
        let f = w.get_fact(id).await.unwrap().unwrap();
        assert_eq!(f.status, FactStatus::Active);
        assert_eq!(f.review_reason, None);
        assert_eq!(f.review_after.as_deref(), Some("2026-12-01T00:00:00Z"));

        w.invalidate(id, "2026-10-20T00:00:00Z", "fixed upstream")
            .await
            .unwrap();
        let f = w.get_fact(id).await.unwrap().unwrap();
        assert_eq!(f.status, FactStatus::Invalidated);
        assert_eq!(f.invalid_at.as_deref(), Some("2026-10-20T00:00:00Z"));
        assert_eq!(f.review_reason.as_deref(), Some("fixed upstream"));
        assert!(w.mark_needs_review(id, "x").await.is_err());
        assert!(w.confirm(id, None).await.is_err());
        assert!(
            w.list_facts(&FactsQuery::for_repo(REPO))
                .await
                .unwrap()
                .facts
                .is_empty()
        );

        let missing = FactId(i64::MAX);
        assert!(matches!(
            w.mark_needs_review(missing, "x").await,
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            w.confirm(missing, None).await,
            Err(StoreError::Invalid(_))
        ));
        assert!(matches!(
            w.invalidate(missing, "2026-10-20T00:00:00Z", "x").await,
            Err(StoreError::Invalid(_))
        ));
    }

    pub async fn pages_by_cursor(w: &dyn MemoryFactsWriter) {
        let mut ids = vec![];
        for i in 0..5 {
            ids.push(
                w.record_fact(&journey(&format!("topic/{i}"), "b"))
                    .await
                    .unwrap(),
            );
        }
        let mut q = FactsQuery {
            limit: 2,
            ..FactsQuery::for_repo(REPO)
        };
        let mut seen = vec![];
        loop {
            let page = w.list_facts(&q).await.unwrap();
            assert!(page.facts.len() <= 2);
            seen.extend(page.facts.iter().map(|f| f.id));
            match page.next_cursor {
                Some(c) => q.cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seen, ids);

        for limit in [0, MAX_FACTS_PAGE + 1] {
            let q = FactsQuery {
                limit,
                ..FactsQuery::for_repo(REPO)
            };
            assert!(matches!(
                w.list_facts(&q).await,
                Err(StoreError::Invalid(_))
            ));
        }
        let q = FactsQuery {
            cursor: Some("not-a-cursor".into()),
            ..FactsQuery::for_repo(REPO)
        };
        assert!(matches!(
            w.list_facts(&q).await,
            Err(StoreError::Invalid(_))
        ));
    }

    pub async fn valid_time_as_of(w: &dyn MemoryFactsWriter) {
        let v1 = w
            .record_fact(&journey("selector/pay", "button#pay"))
            .await
            .unwrap();
        let mut second = journey("selector/pay", "[data-testid=pay]");
        second.valid_from = "2026-10-15T00:00:00Z".into();
        let v2 = w.record_fact(&second).await.unwrap();

        let at = |t: &str| {
            let mut q = all_statuses(REPO);
            q.as_of = Some(t.into());
            q
        };
        let ids = |p: FactsPage| p.facts.iter().map(|f| f.id).collect::<Vec<_>>();
        assert_eq!(
            ids(w.list_facts(&at("2026-10-10T00:00:00Z")).await.unwrap()),
            vec![v1]
        );
        assert_eq!(
            ids(w.list_facts(&at("2026-10-15T00:00:00Z")).await.unwrap()),
            vec![v2]
        );
        assert!(
            w.list_facts(&at("2026-09-01T00:00:00Z"))
                .await
                .unwrap()
                .facts
                .is_empty()
        );
        // Regression: with the default statuses, a valid-time point still
        // returns the version that was valid then, although it is superseded.
        let mut default_q = FactsQuery::for_repo(REPO);
        default_q.as_of = Some("2026-10-10T00:00:00Z".into());
        assert_eq!(ids(w.list_facts(&default_q).await.unwrap()), vec![v1]);
    }

    pub async fn candidates_need_a_human(w: &dyn MemoryFactsWriter) {
        let mut proposed = journey("selector/coupon", "Coupon input is #coupon-code");
        proposed.source = FactSource::GateResult; // overwritten on approval
        let candidate = FactCandidate {
            fact: proposed.clone(),
            proposed_by: "claude-code:coder".into(),
        };
        let c1 = w.enqueue_candidate(&candidate).await.unwrap();
        let c2 = w
            .enqueue_candidate(&FactCandidate {
                fact: journey("selector/total", "Total is #order-total"),
                proposed_by: "codex:qa".into(),
            })
            .await
            .unwrap();
        // A candidate is not a fact.
        assert!(
            w.list_facts(&FactsQuery::for_repo(REPO))
                .await
                .unwrap()
                .facts
                .is_empty()
        );
        let pending = w.list_candidates(REPO).await.unwrap();
        assert_eq!(
            pending.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![c1, c2]
        );
        assert_eq!(pending[0].candidate, candidate);

        let fact_id = w.approve_candidate(c1, "alice").await.unwrap();
        let f = w.get_fact(fact_id).await.unwrap().unwrap();
        assert_eq!(f.source, FactSource::ApprovedCandidate);
        assert_eq!(f.body, proposed.body);
        assert!(w.approve_candidate(c1, "alice").await.is_err());

        w.reject_candidate(c2, "alice", "not a stable selector")
            .await
            .unwrap();
        assert!(w.reject_candidate(c2, "alice", "again").await.is_err());
        assert!(w.list_candidates(REPO).await.unwrap().is_empty());

        let mut bad = journey("Bad Topic", "x");
        bad.topic_key = "Bad Topic".into();
        assert!(matches!(
            w.enqueue_candidate(&FactCandidate {
                fact: bad,
                proposed_by: "x:coder".into()
            })
            .await,
            Err(StoreError::Invalid(_))
        ));
    }

    pub async fn invalid_facts_are_rejected(w: &dyn MemoryFactsWriter) {
        let mut bad = journey("ok/topic", "body");
        bad.topic_key = "Not Valid".into();
        assert!(matches!(
            w.record_fact(&bad).await,
            Err(StoreError::Invalid(_))
        ));
        let mut bad = journey("ok/topic", "body");
        bad.body = "x".repeat(MAX_FACT_BODY_BYTES + 1);
        assert!(matches!(
            w.record_fact(&bad).await,
            Err(StoreError::Invalid(_))
        ));
        let mut bad = journey("ok/topic", "body");
        bad.scope_key = None;
        assert!(matches!(
            w.record_fact(&bad).await,
            Err(StoreError::Invalid(_))
        ));
    }
}

macro_rules! suite_tests {
    ($($name:ident),* $(,)?) => {
        mod in_memory {
            use super::*;
            $(
                #[tokio::test]
                async fn $name() {
                    suite::$name(&InMemoryMemoryFacts::new()).await;
                }
            )*
        }

        mod postgres {
            use super::*;
            $(
                #[tokio::test]
                #[ignore = "needs DATABASE_URL"]
                async fn $name() {
                    let Some(h) = common::pg().await else { return };
                    let facts = PgMemoryFacts::new(h.store.pool().clone());
                    suite::$name(&facts).await;
                    h.cleanup().await;
                }
            )*
        }
    };
}

suite_tests!(
    record_and_read,
    a_new_version_supersedes,
    recording_the_same_claim_is_idempotent,
    lifecycle,
    pages_by_cursor,
    valid_time_as_of,
    candidates_need_a_human,
    invalid_facts_are_rejected,
);

// ---------------------------------------------------------------- Postgres only

/// A LOGIN role that is a member of `group`, created for one test.
async fn login_role(admin: &sqlx::PgPool, group: &str) -> (String, String) {
    let role = common::unique("guard_login");
    // Generated per run; not a secret anyone holds.
    let password = common::unique("pw");
    sqlx::query(&format!(
        "CREATE ROLE {role} LOGIN PASSWORD '{password}' IN ROLE {group}"
    ))
    .execute(admin)
    .await
    .expect("create login role (DATABASE_URL user needs CREATEROLE)");
    (role, password)
}

async fn drop_role(admin: &sqlx::PgPool, role: &str) {
    sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
        .execute(admin)
        .await
        .expect("drop login role");
}

async fn schema_of(pool: &sqlx::PgPool) -> String {
    sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn options_as(role: &str, password: &str, schema: &str) -> PgConnectOptions {
    let url = std::env::var("DATABASE_URL").unwrap();
    let options: PgConnectOptions = url.parse().unwrap();
    options
        .username(role)
        .password(password)
        .options([("search_path", schema)])
}

fn sqlstate(e: &sqlx::Error) -> Option<String> {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.to_string())
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn the_reader_role_cannot_insert_or_update() {
    let Some(h) = common::pg().await else { return };
    let admin = h.store.pool().clone();
    let schema = schema_of(&admin).await;
    let writer = PgMemoryFacts::new(admin.clone());
    let id = writer
        .record_fact(&journey("flaky/pay", "flaky"))
        .await
        .unwrap();

    let (role, password) = login_role(&admin, "guard_memory_reader").await;
    let options = options_as(&role, &password, &schema);

    // The adapter reads through the read-only role.
    let reader = PgMemoryFactsReader::connect_with(options.clone())
        .await
        .unwrap();
    let page = reader
        .list_facts(&FactsQuery::for_repo(REPO))
        .await
        .unwrap();
    assert_eq!(page.facts.len(), 1);
    assert_eq!(
        reader.get_fact(id).await.unwrap().unwrap().topic_key,
        "flaky/pay"
    );
    assert_eq!(
        reader.topic_history(REPO, "flaky/pay").await.unwrap().len(),
        1
    );

    // A raw connection of the same login, without the adapter's read-only
    // session setting: the database itself refuses every write.
    let raw = sqlx::PgPool::connect_with(options).await.unwrap();
    let insert = sqlx::query(
        "INSERT INTO memory_facts (repo, scope, scope_key, topic_key, body, commit_sha, source, valid_from)
         VALUES ('acme/shop', 'repo', NULL, 'evil', 'x', $1, 'gate_result', now())",
    )
    .bind(SHA)
    .execute(&raw)
    .await
    .expect_err("reader inserted a fact");
    assert_eq!(sqlstate(&insert).as_deref(), Some("42501"), "{insert}");
    let update = sqlx::query("UPDATE memory_facts SET status = 'needs_review' WHERE id = $1")
        .bind(id.0)
        .execute(&raw)
        .await
        .expect_err("reader updated a fact");
    assert_eq!(sqlstate(&update).as_deref(), Some("42501"), "{update}");
    let delete = sqlx::query("DELETE FROM memory_facts")
        .execute(&raw)
        .await
        .expect_err("reader deleted");
    assert_eq!(sqlstate(&delete).as_deref(), Some("42501"));
    let candidates = sqlx::query("SELECT * FROM memory_fact_candidates")
        .execute(&raw)
        .await
        .expect_err("reader read candidates");
    assert_eq!(sqlstate(&candidates).as_deref(), Some("42501"));
    let candidate_insert = sqlx::query(
        "INSERT INTO memory_fact_candidates (repo, fact, proposed_by) VALUES ('acme/shop', '{}', 'x')",
    )
    .execute(&raw)
    .await
    .expect_err("reader enqueued a candidate");
    assert_eq!(sqlstate(&candidate_insert).as_deref(), Some("42501"));

    let f = writer.get_fact(id).await.unwrap().unwrap();
    assert_eq!(f.status, FactStatus::Active);

    raw.close().await;
    reader.close().await;
    drop_role(&admin, &role).await;
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn the_reader_adapter_is_read_only_even_with_a_writer_login() {
    // Defense in depth: a misconfigured GUARD_MEMORY_READ_DATABASE_URL that
    // names a privileged login still cannot write through the reader adapter.
    let Some(h) = common::pg().await else { return };
    let admin = h.store.pool().clone();
    let schema = schema_of(&admin).await;
    let url = std::env::var("DATABASE_URL").unwrap();
    let options: PgConnectOptions = url.parse().unwrap();
    let reader =
        PgMemoryFactsReader::connect_with(options.options([("search_path", schema.as_str())]))
            .await
            .unwrap();
    let e = sqlx::query(
        "INSERT INTO memory_facts (repo, scope, topic_key, body, commit_sha, source, valid_from)
         VALUES ('acme/shop', 'repo', 'evil', 'x', $1, 'gate_result', now())",
    )
    .bind(SHA)
    .execute(reader.pool())
    .await
    .expect_err("read-only session wrote");
    assert_eq!(sqlstate(&e).as_deref(), Some("25006"), "{e}");
    reader.close().await;
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn the_orchestrator_role_writes_but_never_deletes() {
    let Some(h) = common::pg().await else { return };
    let admin = h.store.pool().clone();
    let schema = schema_of(&admin).await;
    let (role, password) = login_role(&admin, "guard_orchestrator").await;
    let pool = sqlx::PgPool::connect_with(options_as(&role, &password, &schema))
        .await
        .unwrap();
    let writer = PgMemoryFacts::new(pool.clone());
    let id = writer
        .record_fact(&journey("flaky/pay", "flaky"))
        .await
        .unwrap();
    writer.mark_needs_review(id, "stale").await.unwrap();
    let c = writer
        .enqueue_candidate(&FactCandidate {
            fact: journey("selector/a", "b"),
            proposed_by: "fake:coder".into(),
        })
        .await
        .unwrap();
    writer.approve_candidate(c, "alice").await.unwrap();
    let e = sqlx::query("DELETE FROM memory_fact_candidates")
        .execute(&pool)
        .await
        .expect_err("orchestrator deleted");
    assert_eq!(sqlstate(&e).as_deref(), Some("42501"));
    pool.close().await;
    drop_role(&admin, &role).await;
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn bitemporal_believed_at() {
    let Some(h) = common::pg().await else { return };
    let pool = h.store.pool().clone();
    let facts = PgMemoryFacts::new(pool.clone());
    let v1 = facts
        .record_fact(&journey("selector/pay", "button#pay"))
        .await
        .unwrap();
    let t1: String = sqlx::query("SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS t")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get("t");
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let mut second = journey("selector/pay", "[data-testid=pay]");
    second.valid_from = "2026-10-15T00:00:00Z".into();
    let v2 = facts.record_fact(&second).await.unwrap();
    let now = "2999-01-01T00:00:00Z";

    let ids = |v: Vec<MemoryFact>| v.iter().map(|f| f.id).collect::<Vec<_>>();
    // At t1 the store only knew v1, and did not yet know it would end.
    assert_eq!(
        ids(facts
            .believed_at(REPO, &t1, "2026-10-20T00:00:00Z")
            .await
            .unwrap()),
        vec![v1]
    );
    assert_eq!(
        ids(facts
            .believed_at(REPO, now, "2026-10-20T00:00:00Z")
            .await
            .unwrap()),
        vec![v2]
    );
    assert_eq!(
        ids(facts
            .believed_at(REPO, now, "2026-10-10T00:00:00Z")
            .await
            .unwrap()),
        vec![v1]
    );
    assert!(
        facts
            .believed_at(REPO, "2000-01-01T00:00:00Z", "2026-10-20T00:00:00Z")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        facts.believed_at(REPO, "yesterday", now).await,
        Err(StoreError::Invalid(_))
    ));
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn concurrent_writers_leave_one_live_version() {
    let Some(h) = common::pg().await else { return };
    let facts = Arc::new(PgMemoryFacts::new(h.store.pool().clone()));
    let mut tasks = vec![];
    for i in 0..8 {
        let facts = facts.clone();
        tasks.push(tokio::spawn(async move {
            facts
                .record_fact(&journey("race/topic", &format!("body {i}")))
                .await
        }));
    }
    for t in tasks {
        t.await.unwrap().expect("every concurrent writer succeeds");
    }
    let history = facts.topic_history(REPO, "race/topic").await.unwrap();
    assert_eq!(history.len(), 8);
    assert_eq!(
        history
            .iter()
            .filter(|f| f.status != FactStatus::Invalidated)
            .count(),
        1
    );
    h.cleanup().await;
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn timestamps_normalize_to_utc() {
    let Some(h) = common::pg().await else { return };
    let facts = PgMemoryFacts::new(h.store.pool().clone());
    let mut new = journey("tz/topic", "b");
    new.valid_from = "2026-10-01T02:00:00+02:00".into();
    new.review_after = Some("2026-11-15T00:00:00.5Z".into());
    let id = facts.record_fact(&new).await.unwrap();
    let f = facts.get_fact(id).await.unwrap().unwrap();
    assert_eq!(f.valid_from, "2026-10-01T00:00:00Z");
    assert_eq!(f.review_after.as_deref(), Some("2026-11-15T00:00:00.5Z"));
    new.valid_from = "not a time".into();
    new.topic_key = "tz/other".into();
    assert!(matches!(
        facts.record_fact(&new).await,
        Err(StoreError::Invalid(_))
    ));
    h.cleanup().await;
}
