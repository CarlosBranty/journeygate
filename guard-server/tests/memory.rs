//! Orchestrator memory in `guard serve` (memory.md, "Writers" and
//! "Staleness"): gate results, fingerprints, flaky history and human label
//! overrides become facts; the staleness job runs on a timer.

mod common;

use common::*;
use guard_core::fingerprint::Fingerprint;
use guard_core::ports::fakes::{FakeDecisionEngine, InMemoryMemoryFacts};
use guard_core::ports::*;
use guard_server::jobs::PeriodicJob;
use guard_server::memory::{BranchHead, MemoryWriters};
use guard_server::staleness::{FanOutSource, GitRepo, ImpactFanOut, StalenessJob, parse_interval};
use guard_store::memory_staleness::{FanOutReach, TouchedFile, TouchedFiles};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const HEAD: &str = "c0ffee0c0ffee0c0ffee0c0ffee0c0ffee0c0ffe";

struct FixedHead;

#[async_trait::async_trait]
impl BranchHead for FixedHead {
    async fn head(&self) -> Result<String, String> {
        Ok(HEAD.into())
    }
}

fn test(fp: &str, status: TestStatus, blocking: bool) -> GateTest {
    GateTest {
        test_id: format!("chromium::tests/journeys/checkout/card.spec.ts::pays {fp}"),
        title: format!("pays {fp}"),
        file: "tests/journeys/checkout/card.spec.ts".into(),
        line: 14,
        project: "chromium".into(),
        tags: vec![],
        journey: Some("checkout.customer-card".into()),
        status,
        blocking,
        attempts: 3,
        duration_ms: 1200,
        error: Some("Timeout 5000ms exceeded".into()),
        baseline: Some(BaselineOutcome::Regression),
        fingerprint: Some(Fingerprint(fp.into())),
        evidence: None,
    }
}

fn report(run_id: &str, tests: Vec<GateTest>) -> GateReport {
    GateReport {
        version: 2,
        run_id: run_id.into(),
        generated_at: NOW.into(),
        provider: Some(CodeHostProvider::Github),
        repo: Some("acme/shop".into()),
        pr: None,
        ticket_key: None,
        ticket_is_flow_change: false,
        head_sha: "b".repeat(40),
        base_sha: Some("a".repeat(40)),
        verdict: GateVerdict::Red,
        counts: GateCounts::default(),
        tests,
        journey_changes: vec![],
        change_flags: vec![],
        report_url: None,
        ci_url: None,
    }
}

async fn topics(facts: &dyn MemoryFactsReader) -> Vec<(String, FactSource)> {
    let mut q = FactsQuery::for_repo("acme/shop");
    q.limit = 50;
    let mut got: Vec<_> = facts
        .list_facts(&q)
        .await
        .unwrap()
        .facts
        .into_iter()
        .map(|f| (f.topic_key, f.source))
        .collect();
    got.sort_by(|a, b| a.0.cmp(&b.0));
    got
}

fn memory_state(fakes: &Fakes) -> (guard_server::AppState, Arc<InMemoryMemoryFacts>) {
    let facts = Arc::new(InMemoryMemoryFacts::new());
    let store = Arc::new(guard_store::MemoryStore::new());
    let writers = MemoryWriters::new(facts.clone(), "acme/shop")
        .with_flaky(store.clone())
        .with_head(Arc::new(FixedHead))
        .with_evidence_base("s3://guard-evidence");
    let state = fakes
        .state()
        .with_gates(store.clone())
        .with_decisions(store)
        .with_memory(Arc::new(writers));
    (state, facts)
}

#[tokio::test]
async fn gate_results_write_failure_fingerprint_and_flaky_facts() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let (state, facts) = memory_state(&fakes);
    let tasks = state.tasks.clone();
    let app = guard_server::router(state);

    let first = report(
        "run-1",
        vec![
            test("1a2b3c4d", TestStatus::Failed, true),
            test("99999999", TestStatus::Flaky, false),
        ],
    );
    let res = send(
        app.clone(),
        api_post("/api/gate-result", serde_json::to_string(&first).unwrap()),
    )
    .await;
    assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.text());
    tasks.wait_idle().await;
    let after_one = topics(facts.as_ref()).await;
    assert!(
        after_one.contains(&(
            "failure/guard-fp-1a2b3c4d".to_string(),
            FactSource::GateResult
        )),
        "{after_one:?}"
    );
    assert!(
        after_one
            .iter()
            .any(|(t, s)| t.starts_with("flaky/") && *s == FactSource::FlakyHistory),
        "{after_one:?}"
    );
    assert!(
        !after_one.iter().any(|(t, _)| t.starts_with("fingerprint/")),
        "one run is not a recurring fingerprint"
    );

    // The same fingerprint in a second run: a fingerprint/ fact.
    let second = report("run-2", vec![test("1a2b3c4d", TestStatus::Failed, true)]);
    send(
        app.clone(),
        api_post("/api/gate-result", serde_json::to_string(&second).unwrap()),
    )
    .await;
    tasks.wait_idle().await;
    let after_two = topics(facts.as_ref()).await;
    assert!(
        after_two.contains(&(
            "fingerprint/guard-fp-1a2b3c4d".to_string(),
            FactSource::FailureFingerprint
        )),
        "{after_two:?}"
    );

    // A redelivery replays the writers without new versions.
    let before = after_two.len();
    send(
        app,
        api_post("/api/gate-result", serde_json::to_string(&second).unwrap()),
    )
    .await;
    tasks.wait_idle().await;
    assert_eq!(topics(facts.as_ref()).await.len(), before);
    let mut q = FactsQuery::for_repo("acme/shop");
    q.topic_prefix = Some("failure/".into());
    let failure = &facts.list_facts(&q).await.unwrap().facts[0];
    assert_eq!(failure.run_id.as_deref(), Some("run-1"));
    let mut q = FactsQuery::for_repo("acme/shop");
    q.topic_prefix = Some("flaky/".into());
    assert_eq!(
        facts.list_facts(&q).await.unwrap().facts[0].commit_sha,
        HEAD
    );
}

#[tokio::test]
async fn a_human_label_overriding_triage_becomes_a_decision_fact() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    fakes.tracker.insert_issue(issue("SHOP-7", "x", "", &[]));
    let (state, facts) = memory_state(&fakes);
    let tasks = state.tasks.clone();
    // The engine is unconfigured: triage decides the human lane.
    guard_server::triage::triage_ticket(&state, "SHOP-7")
        .await
        .unwrap();
    let body = json!({
        "webhookEvent": "jira:issue_updated",
        "issue": {"key": "SHOP-7", "fields": {"labels": ["lane:agent", "area:checkout"]}},
        "changelog": {"items": [{"field": "labels"}]}
    });
    let res = send(guard_server::router(state), jira_hook(body)).await;
    assert_eq!(res.status, axum::http::StatusCode::ACCEPTED);
    tasks.wait_idle().await;
    let got = topics(facts.as_ref()).await;
    assert_eq!(
        got,
        vec![("decision/shop-7".to_string(), FactSource::ApprovedDecision)]
    );
    let fact = &facts
        .list_facts(&FactsQuery::for_repo("acme/shop"))
        .await
        .unwrap()
        .facts[0];
    assert!(
        fact.body.contains("lane agent (triage chose human)"),
        "{}",
        fact.body
    );
    assert_eq!(fact.commit_sha, HEAD);
}

#[tokio::test]
async fn without_memory_writers_nothing_changes() {
    let fakes = Fakes::new(FakeDecisionEngine::new());
    let state = fakes.state();
    let tasks = state.tasks.clone();
    let r = report("run-1", vec![test("1a2b3c4d", TestStatus::Failed, true)]);
    let res = send(
        guard_server::router(state),
        api_post("/api/gate-result", serde_json::to_string(&r).unwrap()),
    )
    .await;
    assert_eq!(res.status, axum::http::StatusCode::OK);
    tasks.wait_idle().await;
}

// ---------------------------------------------------------------- staleness

struct Touched(Vec<TouchedFile>);

#[async_trait::async_trait]
impl TouchedFiles for Touched {
    async fn touched_since(&self, _: &str, _: &str) -> Result<Vec<TouchedFile>, StoreError> {
        Ok(self.0.clone())
    }
}

struct Graph;

impl FanOutReach for Graph {
    fn dependents(&self, path: &str) -> Vec<String> {
        if path == "web/src/checkout/api.ts" {
            vec!["tests/journeys/checkout/card.spec.ts".into()]
        } else {
            vec![]
        }
    }
}

fn fact(topic: &str, paths: &[&str], review_after: Option<&str>) -> NewFact {
    NewFact {
        repo: "acme/shop".into(),
        scope: FactScope::Repo,
        scope_key: None,
        topic_key: topic.into(),
        body: format!("about {topic}"),
        commit_sha: "a".repeat(40),
        run_id: None,
        evidence_uri: None,
        source: FactSource::GateResult,
        valid_from: "2026-10-01T00:00:00Z".into(),
        review_after: review_after.map(str::to_string),
        referenced_paths: paths.iter().map(|p| p.to_string()).collect(),
    }
}

#[tokio::test]
async fn the_staleness_job_marks_facts_through_touched_files_and_fan_out() {
    let facts = Arc::new(InMemoryMemoryFacts::new());
    let via_fan_out = facts
        .record_fact(&fact(
            "note/card",
            &["tests/journeys/checkout/card.spec.ts"],
            None,
        ))
        .await
        .unwrap();
    let untouched = facts
        .record_fact(&fact("note/other", &["services/billing/x.ts"], None))
        .await
        .unwrap();
    let overdue = facts
        .record_fact(&fact("note/due", &[], Some("2026-10-15T00:00:00Z")))
        .await
        .unwrap();
    let touched = Arc::new(Touched(vec![TouchedFile {
        path: "web/src/checkout/api.ts".into(),
        commit: "d".repeat(40),
    }]));
    let job = StalenessJob::new(facts.clone(), "acme/shop", touched)
        .with_fan_out(FanOutSource::fixed(Arc::new(Graph)))
        .with_clock(Arc::new(guard_server::FixedClock(NOW.into())));
    let report = job.run_once().await.unwrap();
    assert_eq!(report.checked, 3);
    let marked: Vec<_> = report.marked.iter().map(|(id, _)| *id).collect();
    assert_eq!(marked, vec![via_fan_out, overdue]);
    assert_eq!(
        report.marked[0].1,
        format!("web/src/checkout/api.ts changed in {}", &"d".repeat(7))
    );
    let still: Vec<_> = facts
        .list_facts(&FactsQuery {
            statuses: vec![FactStatus::Active],
            ..FactsQuery::for_repo("acme/shop")
        })
        .await
        .unwrap()
        .facts
        .into_iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(still, vec![untouched]);

    // As a periodic job: runs at once, then on every interval.
    let periodic = job.periodic(Duration::from_millis(30));
    assert_eq!(periodic.name(), "memory staleness");
    let summary = periodic.run_once().await.unwrap();
    assert!(summary.contains("checked 1"), "{summary}");
}

#[tokio::test]
async fn periodic_jobs_run_on_their_interval_until_stopped() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = runs.clone();
    let job = PeriodicJob::new("count", Duration::from_millis(20), move || {
        let counter = counter.clone();
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    });
    let failing = PeriodicJob::new("fail", Duration::from_millis(20), || async {
        Err("boom".to_string())
    });
    let running = guard_server::jobs::start(&[job, failing]);
    tokio::time::sleep(Duration::from_millis(150)).await;
    running.stop().await;
    let n = runs.load(Ordering::SeqCst);
    assert!(n >= 3, "ran {n} times");
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(runs.load(Ordering::SeqCst), n, "stopped");
}

#[test]
fn intervals_parse_with_units() {
    assert_eq!(parse_interval("15m").unwrap(), Duration::from_secs(900));
    assert_eq!(parse_interval("900").unwrap(), Duration::from_secs(900));
    assert_eq!(parse_interval(" 30s ").unwrap(), Duration::from_secs(30));
    assert_eq!(parse_interval("2h").unwrap(), Duration::from_secs(7200));
    for bad in ["", "0", "0m", "m", "-5m", "5x"] {
        assert!(parse_interval(bad).is_err(), "{bad}");
    }
    assert_eq!(
        guard_server::staleness::DEFAULT_STALENESS_INTERVAL,
        Duration::from_secs(15 * 60)
    );
}

// ---------------------------------------------------------------- git + impact

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &std::path::Path, path: &str, body: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

#[tokio::test]
async fn git_reports_files_of_new_default_branch_commits_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "src/a.ts", "export const a = 1;\n");
    write(root, "src/b.ts", "import { a } from './a';\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "one"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    write(root, "src/a.ts", "export const a = 2;\n");
    git(root, &["commit", "-q", "-am", "two"]);
    let two = git(root, &["rev-parse", "HEAD"]);
    write(root, "src/a.ts", "export const a = 3;\n");
    write(root, "src/c.ts", "export const c = 1;\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "three"]);
    let three = git(root, &["rev-parse", "HEAD"]);

    let repo = GitRepo::new(root, "main");
    assert_eq!(repo.head().await.unwrap(), three);
    let touched = repo.touched_since("acme/shop", &base).await.unwrap();
    assert_eq!(
        touched,
        vec![
            TouchedFile {
                path: "src/a.ts".into(),
                commit: three.clone()
            },
            TouchedFile {
                path: "src/c.ts".into(),
                commit: three.clone()
            },
        ]
    );
    let since_two = repo.touched_since("acme/shop", &two).await.unwrap();
    assert_eq!(since_two.len(), 2);
    assert!(
        repo.touched_since("acme/shop", &three)
            .await
            .unwrap()
            .is_empty()
    );
    // A commit the checkout does not have: nothing is known to have changed.
    assert!(
        repo.touched_since("acme/shop", &"e".repeat(40))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(repo.touched_since("acme/shop", "--all").await.is_err());

    // The import graph of the checkout, rebuilt by guard-impact.
    let fan_out = ImpactFanOut::build(root, &config()).unwrap();
    assert_eq!(fan_out.dependents("src/a.ts"), vec!["src/b.ts".to_string()]);
    let source = FanOutSource::rebuild(root, config());
    let graph = source.current().await.unwrap();
    assert_eq!(graph.dependents("src/a.ts"), vec!["src/b.ts".to_string()]);
}

// ---------------------------------------------------------------- live Postgres

struct Pg {
    store: guard_store::PgStore,
    admin: guard_store::sqlx::PgPool,
    schema: String,
}

impl Pg {
    async fn start() -> Option<Pg> {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            if std::env::var("GUARD_ALLOW_SKIPPED_LIVE_TESTS").as_deref() == Ok("1") {
                return None;
            }
            panic!("DATABASE_URL is not set (GUARD_ALLOW_SKIPPED_LIVE_TESTS=1 to skip)");
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let schema = format!("guard_server_mem_{}_{nanos}", std::process::id());
        let admin = guard_store::sqlx::PgPool::connect(&url).await.unwrap();
        guard_store::sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options: guard_store::sqlx::postgres::PgConnectOptions = url.parse().unwrap();
        let store =
            guard_store::PgStore::connect_with(options.options([("search_path", schema.as_str())]))
                .await
                .unwrap();
        store.migrate().await.unwrap();
        Some(Pg {
            store,
            admin,
            schema,
        })
    }

    async fn facts(&self) -> Vec<(String, String, String)> {
        guard_store::sqlx::query_as(
            "SELECT topic_key, source, status FROM memory_facts ORDER BY topic_key, id",
        )
        .fetch_all(self.store.pool())
        .await
        .unwrap()
    }

    async fn cleanup(self) {
        self.store.close().await;
        guard_store::sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "live Postgres: DATABASE_URL"]
async fn live_postgres_gate_results_and_label_overrides_become_facts() {
    let Some(pg) = Pg::start().await else { return };
    let store = Arc::new(guard_store::PgStore::from_pool(pg.store.pool().clone()));
    let facts = Arc::new(guard_store::PgMemoryFacts::new(pg.store.pool().clone()));
    let fakes = Fakes::new(FakeDecisionEngine::new());
    fakes.tracker.insert_issue(issue("SHOP-9", "x", "", &[]));
    let writers = MemoryWriters::new(facts.clone(), "acme/shop")
        .with_flaky(store.clone())
        .with_head(Arc::new(FixedHead));
    let state = fakes
        .state()
        .with_gates(store.clone())
        .with_decisions(store.clone())
        .with_memory(Arc::new(writers));
    let tasks = state.tasks.clone();
    let app = guard_server::router(state.clone());

    for run in ["run-1", "run-2", "run-2"] {
        let r = report(
            run,
            vec![
                test("1a2b3c4d", TestStatus::Failed, true),
                test("99999999", TestStatus::Flaky, false),
            ],
        );
        let res = send(
            app.clone(),
            api_post("/api/gate-result", serde_json::to_string(&r).unwrap()),
        )
        .await;
        assert_eq!(res.status, axum::http::StatusCode::OK, "{}", res.text());
        tasks.wait_idle().await;
    }
    guard_server::triage::triage_ticket(&state, "SHOP-9")
        .await
        .unwrap();
    let body = json!({
        "webhookEvent": "jira:issue_updated",
        "issue": {"key": "SHOP-9", "fields": {"labels": ["lane:agent"]}},
        "changelog": {"items": [{"field": "labels"}]}
    });
    send(app, jira_hook(body)).await;
    tasks.wait_idle().await;

    let rows = pg.facts().await;
    let got: Vec<(&str, &str)> = rows
        .iter()
        .map(|(t, s, _)| (t.split('/').next().unwrap_or_default(), s.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            ("decision", "approved_decision"),
            ("failure", "gate_result"),
            ("fingerprint", "failure_fingerprint"),
            ("fingerprint", "failure_fingerprint"),
            ("flaky", "flaky_history"),
        ],
        "{rows:?}"
    );
    // Replays confirmed: one live version per topic.
    assert!(rows.iter().all(|(_, _, status)| status == "active"));
    pg.cleanup().await;
}

#[tokio::test]
#[ignore = "live Postgres: DATABASE_URL"]
async fn live_postgres_staleness_job_marks_facts_after_new_commits() {
    let Some(pg) = Pg::start().await else { return };
    let facts = Arc::new(guard_store::PgMemoryFacts::new(pg.store.pool().clone()));
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "src/a.ts", "export const a = 1;\n");
    write(
        root,
        "src/b.ts",
        "import { a } from './a';\nexport const b = a;\n",
    );
    write(root, "src/z.ts", "export const z = 1;\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "one"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    let mut on_b = fact("note/b", &["src/b.ts"], None);
    on_b.commit_sha = base.clone();
    let mut on_z = fact("note/z", &["src/z.ts"], None);
    on_z.commit_sha = base.clone();
    let b_id = facts.record_fact(&on_b).await.unwrap();
    facts.record_fact(&on_z).await.unwrap();

    write(root, "src/a.ts", "export const a = 2;\n");
    git(root, &["commit", "-q", "-am", "two"]);
    let two = git(root, &["rev-parse", "HEAD"]);

    let job = StalenessJob::new(
        facts.clone(),
        "acme/shop",
        Arc::new(GitRepo::new(root, "main")),
    )
    .with_fan_out(FanOutSource::rebuild(root, config()))
    .with_gates(Arc::new(guard_store::PgStore::from_pool(
        pg.store.pool().clone(),
    )))
    .with_clock(Arc::new(guard_server::FixedClock(NOW.into())));
    let running = guard_server::jobs::start(&[job.periodic(Duration::from_millis(50))]);
    let mut marked = vec![];
    for _ in 0..100 {
        marked = pg
            .facts()
            .await
            .into_iter()
            .filter(|(_, _, s)| s == "needs_review")
            .collect();
        if !marked.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    running.stop().await;
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert_eq!(marked[0].0, "note/b");
    let reason: Option<String> =
        guard_store::sqlx::query_scalar("SELECT review_reason FROM memory_facts WHERE id = $1")
            .bind(b_id.0)
            .fetch_one(pg.store.pool())
            .await
            .unwrap();
    assert_eq!(
        reason.as_deref(),
        Some(format!("src/a.ts changed in {}", &two[..7]).as_str())
    );
    pg.cleanup().await;
}
