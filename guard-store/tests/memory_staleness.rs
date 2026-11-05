//! Staleness job (docs/contracts/memory.md, "Staleness"): referenced files
//! touched since the fact's commit (directly or through fan-out),
//! `review_after` expiry, and fingerprints absent for N gate runs.

mod common;

use async_trait::async_trait;
use common::fixtures;
use guard_core::fingerprint::Fingerprint;
use guard_core::ports::fakes::InMemoryMemoryFacts;
use guard_core::ports::*;
use guard_store::memory_staleness::{
    DEFAULT_FINGERPRINT_WINDOW, FanOutReach, FingerprintRuns, GateHistoryRuns, RecentRuns,
    StalenessConfig, StalenessSources, TouchedFile, TouchedFiles, run_staleness,
};
use guard_store::{MemoryStore, PgMemoryFacts};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

const REPO: &str = "acme/shop";
const OLD: &str = "0123456789abcdef0123456789abcdef01234567";
const NEW: &str = "89abcdef0123456789abcdef0123456789abcdef";

/// Commit → files touched after it.
#[derive(Default)]
struct FakeGit {
    since: BTreeMap<String, Vec<TouchedFile>>,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl TouchedFiles for FakeGit {
    async fn touched_since(
        &self,
        _repo: &str,
        since: &str,
    ) -> Result<Vec<TouchedFile>, StoreError> {
        self.calls.lock().unwrap().push(since.to_string());
        Ok(self.since.get(since).cloned().unwrap_or_default())
    }
}

struct FakeImports(BTreeMap<String, Vec<String>>);

impl FanOutReach for FakeImports {
    fn dependents(&self, path: &str) -> Vec<String> {
        self.0.get(path).cloned().unwrap_or_default()
    }
}

struct FakeRuns(RecentRuns);

#[async_trait]
impl FingerprintRuns for FakeRuns {
    async fn recent(
        &self,
        _repo: &str,
        _journey: Option<&str>,
        _n: u32,
    ) -> Result<RecentRuns, StoreError> {
        Ok(self.0.clone())
    }
}

fn fact(topic: &str, paths: &[&str], review_after: Option<&str>) -> NewFact {
    NewFact {
        repo: REPO.into(),
        scope: FactScope::Journey,
        scope_key: Some("checkout.pay".into()),
        topic_key: topic.into(),
        body: format!("claim about {topic}"),
        commit_sha: OLD.into(),
        run_id: None,
        evidence_uri: None,
        source: FactSource::GateResult,
        valid_from: "2026-10-01T00:00:00Z".into(),
        review_after: review_after.map(str::to_string),
        referenced_paths: paths.iter().map(|p| p.to_string()).collect(),
    }
}

fn git() -> FakeGit {
    FakeGit {
        since: BTreeMap::from([(
            OLD.to_string(),
            vec![
                TouchedFile {
                    path: "src/pay.ts".into(),
                    commit: NEW.into(),
                },
                TouchedFile {
                    path: "src/money.ts".into(),
                    commit: NEW.into(),
                },
            ],
        )]),
        calls: Mutex::default(),
    }
}

fn config() -> StalenessConfig {
    StalenessConfig {
        now: "2026-11-01T00:00:00Z".into(),
        fingerprint_window: DEFAULT_FINGERPRINT_WINDOW,
    }
}

async fn status(w: &InMemoryMemoryFacts, id: FactId) -> (FactStatus, Option<String>) {
    let f = w.get_fact(id).await.unwrap().unwrap();
    (f.status, f.review_reason)
}

#[tokio::test]
async fn touched_referenced_files_and_expired_deadlines_need_review() {
    let w = InMemoryMemoryFacts::new();
    let direct = w
        .record_fact(&fact("a/direct", &["src/pay.ts"], None))
        .await
        .unwrap();
    let via = w
        .record_fact(&fact("a/via", &["src/cart.ts"], None))
        .await
        .unwrap();
    let untouched = w
        .record_fact(&fact("a/untouched", &["src/other.ts"], None))
        .await
        .unwrap();
    let expired = w
        .record_fact(&fact("a/expired", &[], Some("2026-10-31T00:00:00Z")))
        .await
        .unwrap();
    let not_yet = w
        .record_fact(&fact("a/not-yet", &[], Some("2026-12-01T00:00:00Z")))
        .await
        .unwrap();
    let mut newer = fact("a/newer", &["src/pay.ts"], None);
    newer.commit_sha = NEW.into(); // observed after the change
    let newer = w.record_fact(&newer).await.unwrap();

    let git = git();
    // money.ts is imported by cart.ts: a change to money.ts reaches cart.ts.
    let imports = FakeImports(BTreeMap::from([(
        "src/money.ts".to_string(),
        vec!["src/cart.ts".to_string()],
    )]));
    let sources = StalenessSources {
        touched: &git,
        fan_out: Some(&imports),
        fingerprints: None,
    };
    let report = run_staleness(&w, REPO, &sources, &config()).await.unwrap();
    assert_eq!(report.checked, 6);

    assert_eq!(
        status(&w, direct).await,
        (
            FactStatus::NeedsReview,
            Some("src/pay.ts changed in 89abcde".into())
        )
    );
    assert_eq!(
        status(&w, via).await,
        (
            FactStatus::NeedsReview,
            Some("src/money.ts changed in 89abcde".into())
        )
    );
    assert_eq!(
        status(&w, expired).await,
        (FactStatus::NeedsReview, Some("review_after passed".into()))
    );
    for id in [untouched, not_yet, newer] {
        assert_eq!(status(&w, id).await.0, FactStatus::Active, "{id:?}");
    }
    let marked: Vec<FactId> = report.marked.iter().map(|(id, _)| *id).collect();
    assert_eq!(marked, vec![direct, via, expired]);
    // One git query per distinct commit.
    let calls = git.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "{calls:?}");

    // A second run is a no-op.
    let again = run_staleness(&w, REPO, &sources, &config()).await.unwrap();
    assert!(again.marked.is_empty());
    assert_eq!(again.checked, 3);
}

#[tokio::test]
async fn fingerprints_absent_for_the_window_need_review() {
    let w = InMemoryMemoryFacts::new();
    let gone = w
        .record_fact(&fact("failure/guard-fp-1a2b3c4d", &[], None))
        .await
        .unwrap();
    let present = w
        .record_fact(&fact("fingerprint/guard-fp-55555555", &[], None))
        .await
        .unwrap();
    let other_topic = w.record_fact(&fact("flaky/x", &[], None)).await.unwrap();
    let git = FakeGit::default();
    let runs = FakeRuns(RecentRuns {
        runs: 20,
        labels: BTreeSet::from(["guard-fp-55555555".to_string()]),
    });
    let sources = StalenessSources {
        touched: &git,
        fan_out: None,
        fingerprints: Some(&runs),
    };
    run_staleness(&w, REPO, &sources, &config()).await.unwrap();
    assert_eq!(
        status(&w, gone).await,
        (
            FactStatus::NeedsReview,
            Some("fingerprint absent for 20 runs".into())
        )
    );
    assert_eq!(status(&w, present).await.0, FactStatus::Active);
    assert_eq!(status(&w, other_topic).await.0, FactStatus::Active);

    // Fewer than N runs so far: not enough evidence of absence.
    let w = InMemoryMemoryFacts::new();
    let id = w
        .record_fact(&fact("failure/guard-fp-1a2b3c4d", &[], None))
        .await
        .unwrap();
    let few = FakeRuns(RecentRuns {
        runs: 19,
        labels: BTreeSet::new(),
    });
    let sources = StalenessSources {
        touched: &git,
        fan_out: None,
        fingerprints: Some(&few),
    };
    run_staleness(&w, REPO, &sources, &config()).await.unwrap();
    assert_eq!(status(&w, id).await.0, FactStatus::Active);
}

#[tokio::test]
async fn gate_history_provides_the_recent_fingerprints_of_a_journey() {
    let store = MemoryStore::new();
    for i in 0..25u32 {
        let mut t = fixtures::test("chromium::pay.spec.ts::pays", TestStatus::Failed);
        // Only the oldest 3 runs saw guard-fp-1a2b3c4d; the newer ones saw another.
        let fp = if i < 3 { "1a2b3c4d" } else { "55555555" };
        t.fingerprint = Some(Fingerprint(fp.into()));
        let mut other = fixtures::test("chromium::cart.spec.ts::adds", TestStatus::Failed);
        other.journey = Some("cart.add".into());
        other.fingerprint = Some(Fingerprint("77777777".into()));
        let mut report = fixtures::report(Some(REPO), &format!("run-{i}"), None, vec![t, other]);
        report.generated_at = format!("2026-10-01T00:{i:02}:00Z");
        store
            .record_gate_result(&report, &fixtures::outcome(""))
            .await
            .unwrap();
    }
    let runs = GateHistoryRuns::new(&store);
    let recent = runs.recent(REPO, Some("checkout.pay"), 20).await.unwrap();
    assert_eq!(recent.runs, 20);
    assert!(recent.labels.contains("guard-fp-55555555"));
    assert!(!recent.labels.contains("guard-fp-1a2b3c4d"));
    assert!(
        !recent.labels.contains("guard-fp-77777777"),
        "another journey"
    );
    let none = runs
        .recent("other/repo", Some("checkout.pay"), 20)
        .await
        .unwrap();
    assert_eq!(none.runs, 0);
}

#[tokio::test]
#[ignore = "needs DATABASE_URL"]
async fn staleness_on_postgres() {
    let Some(h) = common::pg().await else { return };
    let w = PgMemoryFacts::new(h.store.pool().clone());
    let direct = w
        .record_fact(&fact("a/direct", &["src/pay.ts"], None))
        .await
        .unwrap();
    let expired = w
        .record_fact(&fact("a/expired", &[], Some("2026-10-31T00:00:00Z")))
        .await
        .unwrap();
    let keep = w
        .record_fact(&fact("a/keep", &["src/other.ts"], None))
        .await
        .unwrap();
    let git = git();
    let sources = StalenessSources {
        touched: &git,
        fan_out: None,
        fingerprints: None,
    };
    let report = run_staleness(&w, REPO, &sources, &config()).await.unwrap();
    let marked: Vec<FactId> = report.marked.iter().map(|(id, _)| *id).collect();
    assert_eq!(marked, vec![direct, expired]);
    let f = w.get_fact(keep).await.unwrap().unwrap();
    assert_eq!(f.status, FactStatus::Active);
    h.cleanup().await;
}
