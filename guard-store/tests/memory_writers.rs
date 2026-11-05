//! Orchestrator fact writers and the agent-proposal candidate queue
//! (docs/contracts/memory.md, "Writers"). Run against the in-memory fake;
//! one live test repeats the gate-result path on Postgres.

mod common;

use common::fixtures;
use guard_core::fingerprint::Fingerprint;
use guard_core::ports::fakes::InMemoryMemoryFacts;
use guard_core::ports::memory_facts::{MAX_FACT_BODY_BYTES, is_topic_key};
use guard_core::ports::*;
use guard_store::memory_writers::{
    FixCommit, MAX_PROPOSALS_PER_RUN, ProposalContext, enqueue_proposals, fact_from_fix_commit,
    fact_from_label_override, facts_from_fingerprints, facts_from_flaky, facts_from_gate_result,
    observe, topic_segment,
};
use guard_store::{FlakyStat, PgMemoryFacts};

const REPO: &str = "acme/shop";
const BASE: &str = "s3://guard-evidence";

fn failing(test_id: &str, fp: &str) -> GateTest {
    let mut t = fixtures::test(test_id, TestStatus::Failed);
    t.error = Some("Timeout 5000ms exceeded waiting for #pay".into());
    t.fingerprint = Some(Fingerprint(fp.into()));
    t.evidence = Some(EvidenceBundle {
        test_id: test_id.into(),
        fingerprint: Fingerprint(fp.into()),
        title: "pays".into(),
        journey: Some("checkout.pay".into()),
        location: Some("tests/journeys/checkout/pay.spec.ts:14".into()),
        error: "Timeout 5000ms exceeded waiting for #pay".into(),
        step: None,
        seed: None,
        commit: "a".repeat(40),
        attempts: vec![],
        baseline: Some(BaselineOutcome::Regression),
        files: vec![
            EvidenceFile {
                kind: EvidenceKind::Screenshot,
                name: "s.png".into(),
                content_type: "image/png".into(),
                key: Some("runs/run-1/s.png".into()),
                url: None,
                viewer_url: None,
            },
            EvidenceFile {
                kind: EvidenceKind::Trace,
                name: "trace.zip".into(),
                content_type: "application/zip".into(),
                key: Some("runs/run-1/trace.zip".into()),
                url: None,
                viewer_url: None,
            },
        ],
    });
    t
}

async fn live(w: &InMemoryMemoryFacts) -> Vec<MemoryFact> {
    let q = FactsQuery {
        limit: 50,
        ..FactsQuery::for_repo(REPO)
    };
    w.list_facts(&q).await.unwrap().facts
}

#[test]
fn topic_segments_are_valid_stable_and_distinct() {
    let a = topic_segment("flaky/", "chromium::tests/pay.spec.ts::Checkout > Pays");
    let b = topic_segment("flaky/", "chromium::tests/pay.spec.ts::Checkout > pays");
    assert!(is_topic_key(&a), "{a}");
    assert!(is_topic_key(&b), "{b}");
    assert_ne!(a, b, "case differences must not collide");
    assert_eq!(
        a,
        topic_segment("flaky/", "chromium::tests/pay.spec.ts::Checkout > Pays")
    );
    assert!(a.starts_with("flaky/chromium-tests/pay.spec.ts-checkout-pays-"));
    // Already valid segments stay as they are.
    assert_eq!(topic_segment("decision/", "shop-1"), "decision/shop-1");
    let long = topic_segment("flaky/", &"x".repeat(500));
    assert!(is_topic_key(&long) && long.len() <= 120, "{long}");
    assert!(is_topic_key(&topic_segment("flaky/", "!!!")));
}

#[tokio::test]
async fn blocking_failures_of_a_gate_become_journey_facts() {
    let w = InMemoryMemoryFacts::new();
    let mut passed = fixtures::test("chromium::a.spec.ts::ok", TestStatus::Passed);
    passed.fingerprint = Some(Fingerprint("99999999".into()));
    let mut not_blocking = failing("chromium::b.spec.ts::nb", "22222222");
    not_blocking.blocking = false;
    let mut no_journey = failing("chromium::c.spec.ts::nj", "33333333");
    no_journey.journey = None;
    let report = fixtures::report(
        Some(REPO),
        "run-1",
        Some(7),
        vec![
            failing(
                "chromium::tests/journeys/checkout/pay.spec.ts::pays",
                "1a2b3c4d",
            ),
            passed,
            not_blocking,
            no_journey,
        ],
    );
    let out = facts_from_gate_result(&w, &report, Some(BASE))
        .await
        .unwrap();
    assert_eq!(out.recorded.len(), 2, "{out:?}");

    let facts = live(&w).await;
    let f = facts
        .iter()
        .find(|f| f.topic_key == "failure/guard-fp-1a2b3c4d")
        .expect("journey failure fact");
    assert_eq!(f.scope, FactScope::Journey);
    assert_eq!(f.scope_key.as_deref(), Some("checkout.pay"));
    assert_eq!(f.source, FactSource::GateResult);
    assert_eq!(f.commit_sha, "a".repeat(40));
    assert_eq!(f.run_id.as_deref(), Some("run-1"));
    assert_eq!(
        f.evidence_uri.as_deref(),
        Some("s3://guard-evidence/runs/run-1/trace.zip")
    );
    assert_eq!(f.valid_from, "2026-11-01T10:00:00Z");
    assert!(
        f.body.contains("tests/journeys/checkout/pay.spec.ts:14"),
        "{}",
        f.body
    );
    assert!(f.body.contains("Timeout 5000ms exceeded"), "{}", f.body);
    assert_eq!(f.referenced_paths, ["tests/journeys/checkout/pay.spec.ts"]);
    let repo_scoped = facts
        .iter()
        .find(|f| f.topic_key == "failure/guard-fp-33333333")
        .unwrap();
    assert_eq!(repo_scoped.scope, FactScope::Repo);

    // Replaying the same gate result changes nothing.
    let again = facts_from_gate_result(&w, &report, Some(BASE))
        .await
        .unwrap();
    assert_eq!(again.recorded, out.recorded);
    assert_eq!(
        w.topic_history(REPO, "failure/guard-fp-1a2b3c4d")
            .await
            .unwrap()
            .len(),
        1
    );

    // No repo: nothing to attach facts to.
    let anonymous = fixtures::report(None, "run-2", None, vec![failing("x::y::z", "44444444")]);
    let out = facts_from_gate_result(&w, &anonymous, Some(BASE))
        .await
        .unwrap();
    assert!(out.recorded.is_empty());
    assert_eq!(out.skipped.len(), 1);
}

#[tokio::test]
async fn long_errors_are_clipped_to_the_body_cap() {
    let w = InMemoryMemoryFacts::new();
    let mut t = failing("chromium::a::b", "1a2b3c4d");
    t.evidence = None;
    t.error = Some("é".repeat(2000));
    let report = fixtures::report(Some(REPO), "run-1", None, vec![t]);
    let out = facts_from_gate_result(&w, &report, None).await.unwrap();
    let f = w.get_fact(out.recorded[0]).await.unwrap().unwrap();
    assert!(f.body.len() <= MAX_FACT_BODY_BYTES);
    assert_eq!(f.evidence_uri, None);
}

#[tokio::test]
async fn fingerprints_seen_in_two_runs_become_facts() {
    let w = InMemoryMemoryFacts::new();
    let stored = |id: i64, run: &str, at: &str, tests: Vec<GateTest>| {
        let mut report = fixtures::report(Some(REPO), run, None, tests);
        report.generated_at = at.into();
        StoredGateResult {
            id: GateResultId(id),
            received_at: at.into(),
            report,
            outcome: ReportOutcome::default(),
        }
    };
    // Newest first, like GateHistory::recent_gate_results.
    let results = vec![
        stored(
            3,
            "run-3",
            "2026-11-01T12:00:00Z",
            vec![failing("p::f::t", "1a2b3c4d")],
        ),
        stored(
            2,
            "run-2",
            "2026-11-01T11:00:00Z",
            vec![failing("p::f::u", "55555555")],
        ),
        stored(
            1,
            "run-1",
            "2026-11-01T10:00:00Z",
            vec![failing("p::f::t", "1a2b3c4d")],
        ),
    ];
    let out = facts_from_fingerprints(&w, REPO, &results, 2)
        .await
        .unwrap();
    assert_eq!(out.recorded.len(), 1, "{out:?}");
    let f = w.get_fact(out.recorded[0]).await.unwrap().unwrap();
    assert_eq!(f.topic_key, "fingerprint/guard-fp-1a2b3c4d");
    assert_eq!(f.source, FactSource::FailureFingerprint);
    assert_eq!(f.scope_key.as_deref(), Some("checkout.pay"));
    assert_eq!(f.run_id.as_deref(), Some("run-3"));
    assert_eq!(f.valid_from, "2026-11-01T12:00:00Z");
    // The normalized error (numbers replaced) is the claim.
    assert!(f.body.contains("Timeout <n>ms exceeded"), "{}", f.body);
}

#[tokio::test]
async fn flaky_tests_get_a_review_deadline() {
    let w = InMemoryMemoryFacts::new();
    let stats = vec![
        FlakyStat {
            test_id: "chromium::tests/pay.spec.ts::Checkout > pays".into(),
            journey: Some("checkout.pay".into()),
            runs: 50,
            flaky: 5,
            failed: 0,
            flaky_rate: 0.1,
        },
        FlakyStat {
            test_id: "chromium::tests/rare.spec.ts::rare".into(),
            journey: None,
            runs: 100,
            flaky: 1,
            failed: 0,
            flaky_rate: 0.01,
        },
    ];
    let sha = "c".repeat(40);
    let out = facts_from_flaky(&w, REPO, &sha, &stats, "2026-11-01T00:00:00Z", 0.02)
        .await
        .unwrap();
    assert_eq!(out.recorded.len(), 1);
    let f = w.get_fact(out.recorded[0]).await.unwrap().unwrap();
    assert!(
        f.topic_key.starts_with("flaky/chromium-tests/pay.spec.ts"),
        "{}",
        f.topic_key
    );
    assert_eq!(f.source, FactSource::FlakyHistory);
    assert_eq!(f.review_after.as_deref(), Some("2026-11-15T00:00:00Z"));
    assert_eq!(f.referenced_paths, ["tests/pay.spec.ts"]);
}

#[tokio::test]
async fn fix_commits_reference_their_touched_files() {
    let w = InMemoryMemoryFacts::new();
    let fix = FixCommit {
        repo: REPO.into(),
        area: "checkout".into(),
        fingerprint: Fingerprint("1a2b3c4d".into()),
        commit_sha: "d".repeat(40),
        merged_at: "2026-11-01T09:00:00Z".into(),
        touched_files: vec![
            "src/pay.ts".into(),
            "src/cart.ts".into(),
            "src/pay.ts".into(),
        ],
        pr: Some(42),
        run_id: Some("run-9".into()),
        evidence_uri: None,
    };
    let new = fact_from_fix_commit(&fix);
    assert_eq!(new.topic_key, "fix/guard-fp-1a2b3c4d");
    assert_eq!(new.scope, FactScope::Area);
    assert_eq!(new.scope_key.as_deref(), Some("checkout"));
    assert_eq!(new.referenced_paths, ["src/cart.ts", "src/pay.ts"]);
    assert!(
        new.body.contains("ddddddd") && new.body.contains("#42"),
        "{}",
        new.body
    );
    assert_eq!(new.source, FactSource::FixCommit);
    observe(&w, &new).await.unwrap();
}

#[tokio::test]
async fn only_label_overrides_become_decision_facts() {
    let decision = fixtures::decision("SHOP-1", "2026-10-30T10:00:00Z", Mode::Active);
    let mut agrees = fixtures::label("SHOP-1", "2026-10-31T10:00:00Z", &["lane:agent"]);
    agrees.lane = Some(LaneKind::Agent);
    agrees.area = Some(Area::new("checkout"));
    agrees.kind = Some(Kind::Bugfix);
    let row = |human: Option<HumanLabel>| CalibrationRow {
        decision_id: DecisionId(1),
        decision: decision.clone(),
        human,
    };
    let sha = "e".repeat(40);
    assert!(fact_from_label_override(&row(Some(agrees.clone())), REPO, &sha).is_none());
    assert!(fact_from_label_override(&row(None), REPO, &sha).is_none());

    let mut overrides = agrees;
    overrides.lane = Some(LaneKind::Human);
    let f = fact_from_label_override(&row(Some(overrides)), REPO, &sha).expect("override");
    assert_eq!(f.scope, FactScope::Repo);
    assert_eq!(f.topic_key, "decision/shop-1");
    assert_eq!(f.source, FactSource::ApprovedDecision);
    assert_eq!(f.valid_from, "2026-10-31T10:00:00Z");
    assert!(
        f.body.contains("lane human") && f.body.contains("agent"),
        "{}",
        f.body
    );
}

#[tokio::test]
async fn a_flagged_claim_re_observed_at_a_newer_commit_is_a_new_active_version() {
    let w = InMemoryMemoryFacts::new();
    let report = fixtures::report(
        Some(REPO),
        "run-1",
        None,
        vec![failing("p::f::t", "1a2b3c4d")],
    );
    let first = facts_from_gate_result(&w, &report, None)
        .await
        .unwrap()
        .recorded[0];
    // Replays at the same commit: the same fact, still active.
    let replay = facts_from_gate_result(&w, &report, None)
        .await
        .unwrap()
        .recorded[0];
    assert_eq!(replay, first);
    w.mark_needs_review(first, "src changed").await.unwrap();
    // Re-observed at a newer commit with the same claim: a new active
    // version at that commit (so staleness measures from it), the flagged
    // one superseded.
    let mut newer = report.clone();
    newer.head_sha = "f".repeat(40);
    newer.run_id = "run-2".into();
    let again = facts_from_gate_result(&w, &newer, None)
        .await
        .unwrap()
        .recorded[0];
    assert_ne!(again, first);
    let fact = w.get_fact(again).await.unwrap().unwrap();
    assert_eq!(fact.status, FactStatus::Active);
    assert_eq!(fact.commit_sha, "f".repeat(40));
    assert_eq!(
        w.get_fact(first).await.unwrap().unwrap().status,
        FactStatus::Invalidated
    );
}

#[tokio::test]
async fn re_scanning_old_evidence_never_clears_needs_review() {
    let w = InMemoryMemoryFacts::new();
    let report = fixtures::report(
        Some(REPO),
        "run-1",
        None,
        vec![failing("p::f::t", "1a2b3c4d")],
    );
    let first = facts_from_gate_result(&w, &report, None)
        .await
        .unwrap()
        .recorded[0];
    w.mark_needs_review(first, "fingerprint absent for 20 runs")
        .await
        .unwrap();
    // The same observation at the fact's own commit (a writer re-scanning
    // the recent gate results that still hold the old runs) is not a
    // re-observation: the flag stays until a human, or a newer commit,
    // clears it.
    let again = facts_from_gate_result(&w, &report, None)
        .await
        .unwrap()
        .recorded[0];
    assert_eq!(again, first);
    let fact = w.get_fact(first).await.unwrap().unwrap();
    assert_eq!(fact.status, FactStatus::NeedsReview);
    assert_eq!(
        fact.review_reason.as_deref(),
        Some("fingerprint absent for 20 runs")
    );
    assert_eq!(
        w.topic_history(REPO, "failure/guard-fp-1a2b3c4d")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn agent_proposals_only_reach_the_candidate_queue() {
    let w = InMemoryMemoryFacts::new();
    let ctx = ProposalContext {
        repo: REPO.into(),
        scope: FactScope::Area,
        scope_key: Some("checkout".into()),
        commit_sha: "a".repeat(40),
        run_id: Some("run-1".into()),
        valid_from: "2026-11-01T10:00:00Z".into(),
        proposed_by: "claude-code:coder".into(),
    };
    let mut proposals = vec![
        MemoryProposal {
            topic_key: "selector/coupon".into(),
            body: "Coupon input is #coupon-code".into(),
            evidence: Some("s3://guard-evidence/runs/run-1/trace.zip".into()),
        },
        MemoryProposal {
            topic_key: "selector/total".into(),
            body: "Total is #order-total".into(),
            evidence: Some("src/cart.ts:12".into()),
        },
        MemoryProposal {
            topic_key: "Bad Topic".into(),
            body: "x".into(),
            evidence: None,
        },
    ];
    for i in 0..MAX_PROPOSALS_PER_RUN {
        proposals.push(MemoryProposal {
            topic_key: format!("extra/{i}"),
            body: "b".into(),
            evidence: None,
        });
    }
    let out = enqueue_proposals(&w, &ctx, &proposals).await.unwrap();
    assert_eq!(out.enqueued.len(), MAX_PROPOSALS_PER_RUN);
    assert_eq!(out.rejected.len(), 3, "{out:?}");
    // Nothing became a fact.
    assert!(live(&w).await.is_empty());
    let pending = w.list_candidates(REPO).await.unwrap();
    assert_eq!(pending.len(), MAX_PROPOSALS_PER_RUN);
    let first = &pending[0].candidate;
    assert_eq!(first.proposed_by, "claude-code:coder");
    assert_eq!(
        first.fact.evidence_uri.as_deref(),
        Some("s3://guard-evidence/runs/run-1/trace.zip")
    );
    assert_eq!(first.fact.source, FactSource::ApprovedCandidate);
    // Free-text evidence stays in the body, never as a pointer.
    assert_eq!(pending[1].candidate.fact.evidence_uri, None);
    assert!(pending[1].candidate.fact.body.contains("src/cart.ts:12"));
    // Only a human approval turns it into a fact.
    let id = w.approve_candidate(pending[0].id, "alice").await.unwrap();
    assert_eq!(
        w.get_fact(id).await.unwrap().unwrap().source,
        FactSource::ApprovedCandidate
    );
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn gate_result_writer_on_postgres() {
    let Some(h) = common::pg().await else { return };
    let w = PgMemoryFacts::new(h.store.pool().clone());
    let report = fixtures::report(
        Some(REPO),
        "run-1",
        None,
        vec![failing("p::f::t", "1a2b3c4d")],
    );
    let a = facts_from_gate_result(&w, &report, Some(BASE))
        .await
        .unwrap();
    let b = facts_from_gate_result(&w, &report, Some(BASE))
        .await
        .unwrap();
    assert_eq!(a.recorded, b.recorded);
    assert_eq!(a.recorded.len(), 1);
    h.cleanup().await;
}
