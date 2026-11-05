//! Facts brief builder (4096-byte hard cap, deterministic order) and the
//! Engram-compatible export (docs/contracts/memory.md).

mod common;

use guard_core::ports::fakes::InMemoryMemoryFacts;
use guard_core::ports::memory_facts::{FACTS_BRIEF_HEADER, FACTS_BRIEF_MAX_BYTES};
use guard_core::ports::*;
use guard_store::memory_brief::{BriefScope, build_facts_brief};
use guard_store::memory_export::{ENGRAM_EXPORT_VERSION, engram_export, engram_export_from_reader};
use guard_store::{PgMemoryFacts, StoredFact};
use serde_json::{Value, json};

const REPO: &str = "acme/shop";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn new(scope: FactScope, key: Option<&str>, topic: &str, body: &str) -> NewFact {
    NewFact {
        repo: REPO.into(),
        scope,
        scope_key: key.map(str::to_string),
        topic_key: topic.into(),
        body: body.into(),
        commit_sha: SHA.into(),
        run_id: None,
        evidence_uri: None,
        source: FactSource::GateResult,
        valid_from: "2026-10-01T00:00:00Z".into(),
        review_after: None,
        referenced_paths: vec![],
    }
}

fn scope(areas: &[&str], journeys: &[&str]) -> BriefScope {
    BriefScope {
        repo: REPO.into(),
        areas: areas.iter().map(|s| s.to_string()).collect(),
        journeys: journeys.iter().map(|s| s.to_string()).collect(),
    }
}

// ---------------------------------------------------------------- brief

#[tokio::test]
async fn the_brief_covers_the_repo_its_areas_and_journeys() {
    let w = InMemoryMemoryFacts::new();
    let repo_fact = w
        .record_fact(&new(
            FactScope::Repo,
            None,
            "decision/shop-1",
            "SHOP-1 is human lane",
        ))
        .await
        .unwrap();
    let area = w
        .record_fact(&new(
            FactScope::Area,
            Some("checkout"),
            "fix/guard-fp-1",
            "fixed by abc",
        ))
        .await
        .unwrap();
    let journey = w
        .record_fact(&new(
            FactScope::Journey,
            Some("checkout.pay"),
            "flaky/pay",
            "flaky\n  under   3DS",
        ))
        .await
        .unwrap();
    w.record_fact(&new(
        FactScope::Area,
        Some("catalog"),
        "selector/card",
        "other area",
    ))
    .await
    .unwrap();
    w.record_fact(&new(
        FactScope::Journey,
        Some("cart.add"),
        "flaky/add",
        "other journey",
    ))
    .await
    .unwrap();
    let gone = w
        .record_fact(&new(
            FactScope::Area,
            Some("checkout"),
            "fix/guard-fp-2",
            "gone",
        ))
        .await
        .unwrap();
    w.invalidate(gone, "2026-10-02T00:00:00Z", "wrong")
        .await
        .unwrap();
    w.mark_needs_review(journey, "src/pay.ts changed in 89abcde")
        .await
        .unwrap();

    let brief = build_facts_brief(&w, &scope(&["checkout"], &["checkout.pay"]))
        .await
        .unwrap();
    // Order: (scope, scope_key, topic_key, id) = repo, area, journey.
    assert_eq!(brief.fact_ids, vec![repo_fact, area, journey]);
    assert!(!brief.truncated);
    let expected = format!(
        "{FACTS_BRIEF_HEADER}\n\
         - [{}] repo decision/shop-1: SHOP-1 is human lane\n\
         - [{}] area:checkout fix/guard-fp-1: fixed by abc\n\
         - [{}] journey:checkout.pay flaky/pay: flaky under 3DS (needs_review)\n",
        repo_fact.0, area.0, journey.0
    );
    assert_eq!(brief.text, expected);
    brief.validate().unwrap();
}

#[tokio::test]
async fn the_brief_is_hard_capped_and_deterministic() {
    let w = InMemoryMemoryFacts::new();
    // Inserted in reverse topic order, across many areas and pages.
    for i in (0..300).rev() {
        let area = format!("area-{}", i % 7);
        w.record_fact(&new(
            FactScope::Area,
            Some(&area),
            &format!("topic/{i:03}"),
            &format!("{} {}", "claim é".repeat(10), i),
        ))
        .await
        .unwrap();
    }
    let areas: Vec<String> = (0..7).map(|i| format!("area-{i}")).collect();
    let mut reversed = areas.clone();
    reversed.reverse();
    reversed.push("area-3".into()); // duplicates are harmless
    let a = build_facts_brief(
        &w,
        &BriefScope {
            repo: REPO.into(),
            areas,
            journeys: vec![],
        },
    )
    .await
    .unwrap();
    let b = build_facts_brief(
        &w,
        &BriefScope {
            repo: REPO.into(),
            areas: reversed,
            journeys: vec![],
        },
    )
    .await
    .unwrap();
    assert_eq!(a, b);
    assert!(a.truncated);
    assert!(a.text.len() <= FACTS_BRIEF_MAX_BYTES, "{}", a.text.len());
    assert!(
        a.text.len() > FACTS_BRIEF_MAX_BYTES - 200,
        "the cap is used"
    );
    assert_eq!(a.text.lines().count(), a.fact_ids.len() + 1);
    // area-0 sorts first, its topics in order.
    assert!(
        a.text
            .lines()
            .nth(1)
            .unwrap()
            .contains("area:area-0 topic/000:")
    );
    let unique: std::collections::BTreeSet<_> = a.fact_ids.iter().collect();
    assert_eq!(unique.len(), a.fact_ids.len());
}

#[tokio::test]
async fn an_empty_memory_gives_just_the_header() {
    let w = InMemoryMemoryFacts::new();
    let brief = build_facts_brief(&w, &scope(&[], &[])).await.unwrap();
    assert_eq!(brief.text, format!("{FACTS_BRIEF_HEADER}\n"));
    assert!(brief.fact_ids.is_empty() && !brief.truncated);
}

// ---------------------------------------------------------------- export

const EXAMPLE: &str = include_str!("../../schemas/examples/engram-export.example.json");
const SCHEMA: &str = include_str!("../../schemas/engram-export.schema.json");

fn stored(fact: MemoryFact, updated_at: &str) -> StoredFact {
    StoredFact {
        fact,
        updated_at: updated_at.into(),
    }
}

fn mf(
    id: i64,
    scope: FactScope,
    key: Option<&str>,
    topic: &str,
    body: &str,
    source: FactSource,
) -> MemoryFact {
    MemoryFact {
        id: FactId(id),
        repo: REPO.into(),
        scope,
        scope_key: key.map(str::to_string),
        topic_key: topic.into(),
        body: body.into(),
        commit_sha: SHA.into(),
        run_id: None,
        evidence_uri: None,
        source,
        valid_from: "2026-10-01T00:00:00Z".into(),
        invalid_at: None,
        recorded_at: "2026-10-01T00:00:00Z".into(),
        review_after: None,
        status: FactStatus::Active,
        review_reason: None,
        supersedes: None,
        referenced_paths: vec![],
    }
}

fn example_facts() -> Vec<StoredFact> {
    let mut f12 = mf(
        12,
        FactScope::Journey,
        Some("catalog"),
        "catalog/stock-badge",
        "Stock badge reads product.stock, not inventory.available",
        FactSource::GateResult,
    );
    f12.run_id = Some("run-20261031-0007".into());
    f12.evidence_uri = Some("s3://guard-evidence/runs/run-20261031-0007/trace.zip".into());
    f12.recorded_at = "2026-10-31T09:00:00Z".into();
    let mut f7 = mf(
        7,
        FactScope::Journey,
        Some("checkout"),
        "checkout/pay-timeout",
        "Payment step times out when the 3DS iframe loads late",
        FactSource::FailureFingerprint,
    );
    f7.commit_sha = "89abcdef0123456789abcdef0123456789abcdef".into();
    f7.recorded_at = "2026-10-20T09:00:00Z".into();
    f7.status = FactStatus::Invalidated;
    f7.invalid_at = Some("2026-10-28T09:00:00Z".into());
    let mut f15 = mf(
        15,
        FactScope::Journey,
        Some("checkout"),
        "checkout/pay-timeout",
        "Payment step times out unless 3DS is awaited",
        FactSource::FailureFingerprint,
    );
    f15.supersedes = Some(FactId(7));
    f15.recorded_at = "2026-10-28T09:00:00Z".into();
    f15.review_after = Some("2026-11-28T00:00:00Z".into());
    let f3 = mf(
        3,
        FactScope::Repo,
        None,
        "decision/shop-1",
        "SHOP-1 is human lane",
        FactSource::ApprovedDecision,
    );
    vec![
        stored(f12, "2026-10-31T09:00:00Z"),
        stored(f7, "2026-10-28T09:00:00Z"),
        stored(f15, "2026-10-28T09:00:00Z"),
        stored(f3, "2026-10-01T00:00:00Z"),
    ]
}

/// Keys of `value` are declared by `schema` and its required keys present.
fn check_shape(value: &Value, schema: &Value, what: &str) {
    let props = schema["properties"].as_object().unwrap();
    for key in value.as_object().unwrap().keys() {
        assert!(
            props.contains_key(key),
            "{what}: {key} is not in the schema"
        );
    }
    for req in schema["required"].as_array().unwrap() {
        assert!(
            value.get(req.as_str().unwrap()).is_some(),
            "{what}: missing {req}"
        );
    }
}

#[test]
fn the_export_matches_the_contract_example() {
    let export = serde_json::to_value(engram_export(
        REPO,
        &example_facts(),
        "2026-11-01T12:00:00Z",
    ))
    .unwrap();
    let example: Value = serde_json::from_str(EXAMPLE).unwrap();
    assert_eq!(export["version"], json!(ENGRAM_EXPORT_VERSION));
    assert_eq!(export["version"], example["version"]);
    assert_eq!(export["exported_at"], example["exported_at"]);
    assert_eq!(export["sessions"], example["sessions"]);
    assert_eq!(export["prompts"], json!([]));

    let observations = export["observations"].as_array().unwrap();
    let ids: Vec<i64> = observations
        .iter()
        .map(|o| o["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![3, 7, 12, 15]);
    let by_id = |id: i64| observations.iter().find(|o| o["id"] == json!(id)).unwrap();
    for expected in example["observations"].as_array().unwrap() {
        assert_eq!(by_id(expected["id"].as_i64().unwrap()), expected);
    }
    assert_eq!(by_id(3)["type"], json!("decision"));
    assert_eq!(by_id(3)["topic_key"], json!("guard/repo/decision/shop-1"));
    assert_eq!(by_id(15)["review_after"], json!("2026-11-28T00:00:00Z"));
    assert!(by_id(15).get("superseded_by_sync_id").is_none());

    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    check_shape(&export, &schema, "export");
    for s in export["sessions"].as_array().unwrap() {
        check_shape(s, &schema["$defs"]["session"], "session");
    }
    for o in observations {
        check_shape(o, &schema["$defs"]["observation"], "observation");
        let allowed = schema["$defs"]["observation"]["properties"]["type"]["enum"]
            .as_array()
            .unwrap();
        assert!(allowed.contains(&o["type"]), "{}", o["type"]);
    }
}

#[test]
fn an_empty_export_still_has_its_session() {
    let export = serde_json::to_value(engram_export(REPO, &[], "2026-11-01T12:00:00Z")).unwrap();
    assert_eq!(
        export["sessions"][0]["started_at"],
        json!("2026-11-01T12:00:00Z")
    );
    assert_eq!(export["observations"], json!([]));
}

#[tokio::test]
async fn the_export_from_a_reader_includes_every_status() {
    let w = InMemoryMemoryFacts::new();
    let a = w
        .record_fact(&new(FactScope::Repo, None, "a/one", "v1"))
        .await
        .unwrap();
    let mut v2 = new(FactScope::Repo, None, "a/one", "v2");
    v2.valid_from = "2026-10-05T00:00:00Z".into();
    let b = w.record_fact(&v2).await.unwrap();
    let export = engram_export_from_reader(&w, REPO, "2026-11-01T12:00:00Z")
        .await
        .unwrap();
    let obs = &export.observations;
    assert_eq!(obs.len(), 2);
    assert_eq!(obs[0].id, a.0);
    assert_eq!(
        obs[0].superseded_by_sync_id.as_deref(),
        Some(format!("guard-fact-{}", b.0).as_str())
    );
    assert_eq!(obs[0].deleted_at.as_deref(), Some("2026-10-05T00:00:00Z"));
    assert!(obs[0].content.ends_with("status: invalidated"));
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn the_export_on_postgres_uses_updated_at() {
    let Some(h) = common::pg().await else { return };
    let w = PgMemoryFacts::new(h.store.pool().clone());
    let id = w
        .record_fact(&new(FactScope::Repo, None, "a/one", "v1"))
        .await
        .unwrap();
    w.mark_needs_review(id, "stale").await.unwrap();
    let stored = w.stored_facts(REPO).await.unwrap();
    assert_eq!(stored.len(), 1);
    let export = engram_export(REPO, &stored, "2026-11-01T12:00:00Z");
    let o = &export.observations[0];
    assert_eq!(o.updated_at, stored[0].updated_at);
    assert!(o.updated_at >= o.created_at);
    assert!(o.content.ends_with("status: needs_review"));
    let brief = build_facts_brief(&w, &scope(&[], &[])).await.unwrap();
    assert_eq!(brief.fact_ids, vec![id]);
    h.cleanup().await;
}
