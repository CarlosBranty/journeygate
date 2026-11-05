//! Token savings: gateway spend ingestion into `spend_records` and the 4-arm
//! A/B report over `agent_runs` + `spend_records` (docs/contracts/tokens.md).
//! Everything here runs on the fake gateway and synthetic runs; no provider
//! is ever called.

mod common;

use guard_core::ports::fakes::FakeModelGateway;
use guard_core::ports::*;
use guard_store::spend::{
    MemorySpendSink, PgSpendStore, SpendError, SpendSink, SpendWindow, ingest_spend,
};
use guard_store::spend_report::{
    AbInputs, AbRunRow, AbSpendRow, Arm, Preregistration, Verdict, compute_ab_report,
    load_ab_inputs, render_markdown,
};
use guard_store::sqlx;
use std::collections::BTreeMap;

fn record(i: usize, alias: &str) -> SpendRecord {
    SpendRecord {
        request_id: format!("req-{i:05}"),
        key_alias: Some(alias.into()),
        model: "anthropic/claude-sonnet-4-5".into(),
        spend_usd: 0.0125,
        prompt_tokens: 1200,
        completion_tokens: 300,
        cached_tokens: 900,
        cache_creation_input_tokens: 250,
        cache_hit: Some(false),
        start_time: "2026-11-01T10:00:00Z".into(),
        end_time: "2026-11-01T10:00:02.5Z".into(),
        metadata: BTreeMap::from([
            ("run_id".to_string(), "run-1".to_string()),
            ("arm".to_string(), "memory".to_string()),
        ]),
    }
}

fn window(alias: Option<&str>) -> SpendWindow {
    SpendWindow {
        key_alias: alias.map(str::to_string),
        start: "2026-11-01 00:00:00".into(),
        end: "2026-11-02 00:00:00".into(),
        page_size: 1000,
    }
}

// ---------------------------------------------------------------- ingestion

#[tokio::test]
async fn every_page_is_ingested_once() {
    let gateway = FakeModelGateway::new();
    for i in 0..2500 {
        gateway.push_spend(record(i, "exp1:memory:SHOP-1:0"));
    }
    gateway.push_spend(record(9999, "other"));
    let sink = MemorySpendSink::default();
    let out = ingest_spend(&gateway, &sink, &window(Some("exp1:memory:SHOP-1:0")))
        .await
        .unwrap();
    assert_eq!(out.pages, 3);
    assert_eq!(out.records, 2500);
    assert!(out.rejected.is_empty());
    assert_eq!(sink.len(), 2500);
    // Idempotent by request_id.
    ingest_spend(&gateway, &sink, &window(Some("exp1:memory:SHOP-1:0")))
        .await
        .unwrap();
    assert_eq!(sink.len(), 2500);
    let row = sink.get("req-00001").unwrap();
    assert_eq!(row.cached_tokens, 900);
    assert_eq!(row.cache_creation_input_tokens, 250);
}

#[tokio::test]
async fn bad_rows_are_rejected_one_by_one() {
    let gateway = FakeModelGateway::new();
    gateway.push_spend(record(1, "a"));
    let mut negative = record(2, "a");
    negative.spend_usd = -1.0;
    gateway.push_spend(negative);
    let mut no_id = record(3, "a");
    no_id.request_id = String::new();
    gateway.push_spend(no_id);
    let mut bad_time = record(4, "a");
    bad_time.start_time = "2026-11-01 10:00:00".into();
    gateway.push_spend(bad_time);
    let sink = MemorySpendSink::default();
    let out = ingest_spend(&gateway, &sink, &window(None)).await.unwrap();
    assert_eq!(out.records, 1);
    assert_eq!(out.rejected.len(), 3, "{:?}", out.rejected);
    assert_eq!(sink.len(), 1);
}

#[tokio::test]
async fn an_empty_window_and_bad_queries() {
    let gateway = FakeModelGateway::new();
    let sink = MemorySpendSink::default();
    let out = ingest_spend(&gateway, &sink, &window(None)).await.unwrap();
    assert_eq!((out.pages, out.records), (1, 0));
    let mut w = window(None);
    w.page_size = 0;
    assert!(matches!(
        ingest_spend(&gateway, &sink, &w).await,
        Err(SpendError::Gateway(GatewayError::InvalidRequest(_)))
    ));
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn spend_lands_in_postgres_with_cache_tokens() {
    let Some(h) = common::pg().await else { return };
    let pool = h.store.pool().clone();
    let gateway = FakeModelGateway::new();
    for i in 0..1500 {
        gateway.push_spend(record(i, "exp1:memory:SHOP-1:0"));
    }
    let store = PgSpendStore::new(pool.clone());
    let out = ingest_spend(&gateway, &store, &window(None)).await.unwrap();
    assert_eq!((out.pages, out.records), (2, 1500));
    ingest_spend(&gateway, &store, &window(None)).await.unwrap();
    let (n, cached, created, run, arm): (i64, i64, i64, String, String) = sqlx::query_as(
        "SELECT count(*), sum(cached_tokens)::bigint, sum(cache_creation_input_tokens)::bigint,
                min(run_id), min(arm) FROM spend_records",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 1500);
    assert_eq!(cached, 1500 * 900);
    assert_eq!(created, 1500 * 250);
    assert_eq!((run.as_str(), arm.as_str()), ("run-1", "memory"));

    // A late correction of the same request replaces the row.
    let mut fixed = record(0, "exp1:memory:SHOP-1:0");
    fixed.spend_usd = 0.5;
    store.upsert_spend(&[fixed]).await.unwrap();
    let total = store
        .spend_usd_between("2026-11-01T00:00:00Z", "2026-11-02T00:00:00Z")
        .await
        .unwrap();
    let expected = 0.5 + 1499.0 * 0.0125;
    assert!((total - expected).abs() < 1e-6, "{total} vs {expected}");
    assert_eq!(
        store
            .spend_usd_between("2026-11-02T00:00:00Z", "2026-11-03T00:00:00Z")
            .await
            .unwrap(),
        0.0
    );
    h.cleanup().await;
}

// ---------------------------------------------------------------- A/B report

struct ArmSpec {
    arm: &'static str,
    usd_factor: f64,
    /// Tasks `t` with `t % 100 < unresolved_pct` are unresolved.
    unresolved_pct: usize,
    cached_share: f64,
}

/// `tasks` tasks x 3 reps per arm: a QA run and a coder run per trial.
fn synthetic(experiment: &str, tasks: usize, arms: &[ArmSpec]) -> AbInputs {
    let mut runs = vec![];
    let mut spend = vec![];
    let mut fact_paths = BTreeMap::new();
    fact_paths.insert(1, vec!["src/pay.ts".to_string()]);
    fact_paths.insert(2, vec!["src/other.ts".to_string()]);
    for spec in arms {
        for t in 0..tasks {
            for rep in 0..3 {
                let alias = format!("{experiment}:{}:SHOP-{t}:{rep}", spec.arm);
                let resolved = (t * 7) % 100 >= spec.unresolved_pct;
                let memory = matches!(spec.arm, "memory" | "both");
                runs.push(AbRunRow {
                    key_alias: alias.clone(),
                    role: "qa".into(),
                    attempt: 1,
                    outcome: Some("red_first_passed".into()),
                    steps: Some(4),
                    memory_reads: 0,
                    facts_brief_ids: vec![],
                    diff_paths: vec!["tests/pay.spec.ts".into()],
                });
                runs.push(AbRunRow {
                    key_alias: alias.clone(),
                    role: "coder".into(),
                    attempt: 1,
                    outcome: Some(if resolved { "passed" } else { "gate_failed" }.into()),
                    steps: Some(10),
                    memory_reads: if memory { 2 } else { 0 },
                    facts_brief_ids: if memory { vec![1, 2] } else { vec![] },
                    diff_paths: vec!["src/pay.ts".into()],
                });
                let usd = spec.usd_factor * (1.0 + (t % 5) as f64 * 0.02 + rep as f64 * 0.01);
                spend.push(AbSpendRow {
                    key_alias: alias,
                    prompt_tokens: 10_000,
                    completion_tokens: 2_000,
                    cached_tokens: (10_000.0 * spec.cached_share) as u64,
                    cache_creation_input_tokens: 500,
                    spend_usd: usd,
                });
            }
        }
    }
    AbInputs {
        experiment: experiment.into(),
        runs,
        spend,
        fact_paths,
    }
}

fn standard_arms() -> Vec<ArmSpec> {
    vec![
        ArmSpec {
            arm: "baseline",
            usd_factor: 1.0,
            unresolved_pct: 10,
            cached_share: 0.2,
        },
        ArmSpec {
            arm: "memory",
            usd_factor: 0.7,
            unresolved_pct: 10,
            cached_share: 0.6,
        },
        ArmSpec {
            arm: "map",
            usd_factor: 0.95,
            unresolved_pct: 10,
            cached_share: 0.2,
        },
        ArmSpec {
            arm: "both",
            usd_factor: 0.6,
            unresolved_pct: 10,
            cached_share: 0.6,
        },
    ]
}

fn quick() -> Preregistration {
    Preregistration {
        bootstrap_resamples: 2_000,
        ..Preregistration::default()
    }
}

#[test]
fn the_preregistered_thresholds_are_the_contract_numbers() {
    let p = Preregistration::default();
    assert_eq!(p.max_resolution_drop_pp, 2.0);
    assert_eq!(p.min_cost_reduction, 0.15);
    assert_eq!(p.min_interaction_reduction, 0.05);
    assert_eq!(p.min_tasks, 30);
    assert_eq!(p.bootstrap_resamples, 10_000);
    assert_eq!(p.confidence, 0.95);
    assert_eq!(Arm::parse("+memory"), Some(Arm::Memory));
    assert_eq!(Arm::parse("+map"), Some(Arm::Map));
    assert_eq!(Arm::parse("both"), Some(Arm::Both));
    assert_eq!(Arm::parse("nope"), None);
}

#[test]
fn memory_wins_map_does_not_and_both_is_justified() {
    let inputs = synthetic("exp1", 40, &standard_arms());
    let report = compute_ab_report(&inputs, &Preregistration::default());
    assert_eq!(report.unattributed_runs, 0);
    let arm = |a: Arm| report.arms.iter().find(|m| m.arm == a).unwrap();
    let base = arm(Arm::Baseline);
    assert_eq!(base.trials, 120);
    assert_eq!(base.tasks, 40);
    let rate = base.resolution_rate.value.unwrap();
    assert!((rate - base.resolved as f64 / 120.0).abs() < 1e-12);
    let ci = base.resolution_rate.ci.unwrap();
    assert!(ci.low <= rate && rate <= ci.high);
    assert!((base.cache_hit_rate.value.unwrap() - 0.2).abs() < 1e-9);
    assert!((arm(Arm::Memory).cache_hit_rate.value.unwrap() - 0.6).abs() < 1e-9);
    assert_eq!(base.mean_steps.value, Some(14.0));
    assert_eq!(arm(Arm::Memory).mean_memory_reads.value, Some(2.0));
    // Fact 1 references src/pay.ts (in the diff), fact 2 does not.
    assert_eq!(arm(Arm::Memory).cited_in_diff_share.value, Some(0.5));
    assert_eq!(base.cited_in_diff_share.value, None);
    let tokens = base.tokens_per_resolved.value.unwrap();
    assert!((tokens - 12_000.0 * 120.0 / base.resolved as f64).abs() < 1e-6);

    let cmp = |a: Arm| report.comparisons.iter().find(|c| c.arm == a).unwrap();
    assert_eq!(
        cmp(Arm::Memory).verdict,
        Verdict::Wins,
        "{:?}",
        cmp(Arm::Memory)
    );
    assert!((cmp(Arm::Memory).cost_reduction.unwrap() - 0.3).abs() < 1e-9);
    assert!(cmp(Arm::Memory).cost_diff_ci.unwrap().high < 0.0);
    assert_eq!(cmp(Arm::Map).verdict, Verdict::DoesNotWin);
    assert_eq!(cmp(Arm::Both).verdict, Verdict::Wins);
    assert_eq!(report.interaction.best_single, Some(Arm::Memory));
    let extra = report.interaction.extra_reduction.unwrap();
    assert!((extra - (1.0 - 0.6 / 0.7)).abs() < 1e-9, "{extra}");
    assert!(report.interaction.enable_both);

    // Deterministic: same seed, same report.
    let again = compute_ab_report(&inputs, &Preregistration::default());
    assert_eq!(report, again);
    let md = render_markdown(&report);
    assert!(md.contains("| memory |") && md.contains("wins"), "{md}");
}

#[test]
fn a_resolution_drop_beyond_two_points_loses_even_when_cheaper() {
    let arms = vec![
        ArmSpec {
            arm: "baseline",
            usd_factor: 1.0,
            unresolved_pct: 10,
            cached_share: 0.2,
        },
        ArmSpec {
            arm: "memory",
            usd_factor: 0.5,
            unresolved_pct: 20,
            cached_share: 0.6,
        },
    ];
    let report = compute_ab_report(&synthetic("exp2", 40, &arms), &quick());
    let c = report
        .comparisons
        .iter()
        .find(|c| c.arm == Arm::Memory)
        .unwrap();
    assert!(c.resolution_delta_pp.unwrap() < -2.0, "{c:?}");
    assert_eq!(c.verdict, Verdict::DoesNotWin);
    assert!(
        c.reasons.iter().any(|r| r.contains("resolution")),
        "{:?}",
        c.reasons
    );
    // Arms without runs are reported as inconclusive, not dropped.
    let both = report
        .comparisons
        .iter()
        .find(|c| c.arm == Arm::Both)
        .unwrap();
    assert_eq!(both.verdict, Verdict::Inconclusive);
    assert!(!report.interaction.enable_both);
}

#[test]
fn fewer_than_thirty_tasks_is_inconclusive() {
    let report = compute_ab_report(&synthetic("exp3", 10, &standard_arms()), &quick());
    for c in &report.comparisons {
        assert_eq!(c.verdict, Verdict::Inconclusive, "{c:?}");
        assert!(
            c.reasons.iter().any(|r| r.contains("30")),
            "{:?}",
            c.reasons
        );
    }
    assert!(!report.interaction.enable_both);
}

#[test]
fn runs_outside_the_experiment_are_counted_not_used() {
    let mut inputs = synthetic("exp4", 2, &standard_arms());
    inputs.runs.push(AbRunRow {
        key_alias: "run-7:coder:1".into(),
        role: "coder".into(),
        attempt: 1,
        outcome: Some("passed".into()),
        steps: Some(1),
        memory_reads: 0,
        facts_brief_ids: vec![],
        diff_paths: vec![],
    });
    let report = compute_ab_report(&inputs, &quick());
    assert_eq!(report.unattributed_runs, 1);
    assert_eq!(report.trials.len(), 4 * 2 * 3);
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn the_report_reads_agent_runs_and_spend_records() {
    let Some(h) = common::pg().await else { return };
    let pool = h.store.pool().clone();
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let fact: i64 = sqlx::query_scalar(
        "INSERT INTO memory_facts (repo, scope, scope_key, topic_key, body, commit_sha, source,
                                   valid_from, referenced_paths)
         VALUES ('acme/shop', 'area', 'checkout', 'fix/a', 'b', $1, 'fix_commit', now(),
                 ARRAY['src/pay.ts']) RETURNING id",
    )
    .bind(sha)
    .fetch_one(&pool)
    .await
    .unwrap();
    // exp_live:baseline and exp_live:memory, one task each, plus another
    // experiment whose rows must not leak in.
    for (alias, arm, outcome, reads, brief, cached) in [
        (
            "exp_live:baseline:SHOP-1:0",
            "baseline",
            "passed",
            0,
            vec![],
            100i64,
        ),
        (
            "exp_live:memory:SHOP-1:0",
            "memory",
            "passed",
            3,
            vec![fact],
            700,
        ),
        (
            "exp_live_2:memory:SHOP-1:0",
            "memory",
            "gate_failed",
            0,
            vec![],
            0,
        ),
    ] {
        sqlx::query(
            "INSERT INTO agent_runs (run_id, repo, ticket, role, attempt, agent, runtime, model, arm,
                                     key_alias, base_commit, status, outcome, steps, memory_reads,
                                     facts_brief_ids, diff_paths)
             VALUES ($1, 'acme/shop', 'SHOP-1', 'coder', 1, 'coder', 'fake', 'm', $2, $1, $3,
                     'completed', $4, 12, $5, $6, ARRAY['src/pay.ts'])",
        )
        .bind(alias)
        .bind(arm)
        .bind(sha)
        .bind(outcome)
        .bind(reads)
        .bind(&brief)
        .execute(&pool)
        .await
        .unwrap();
        let mut r = record(0, alias);
        r.request_id = format!("req-{alias}");
        r.prompt_tokens = 1000;
        r.cached_tokens = cached as u64;
        PgSpendStore::new(pool.clone())
            .upsert_spend(&[r])
            .await
            .unwrap();
    }
    let inputs = load_ab_inputs(&pool, "exp_live").await.unwrap();
    assert_eq!(inputs.runs.len(), 2);
    assert_eq!(inputs.spend.len(), 2);
    assert_eq!(
        inputs.fact_paths.get(&fact),
        Some(&vec!["src/pay.ts".to_string()])
    );
    let report = compute_ab_report(&inputs, &quick());
    let memory = report.arms.iter().find(|m| m.arm == Arm::Memory).unwrap();
    assert_eq!(memory.cache_hit_rate.value, Some(0.7));
    assert_eq!(memory.cited_in_diff_share.value, Some(1.0));
    assert_eq!(memory.mean_memory_reads.value, Some(3.0));
    assert_eq!(memory.usd_per_resolved.value, Some(0.0125));
    assert!(
        report
            .comparisons
            .iter()
            .all(|c| c.verdict == Verdict::Inconclusive)
    );
    h.cleanup().await;
}
