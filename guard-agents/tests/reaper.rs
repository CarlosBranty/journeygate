//! Key and network reapers: fakes, live Postgres (`PgRunActivity`) and live
//! Docker (`CliDockerNetworks`, `GUARD_DOCKER_TESTS=1`).

mod common;

use guard_agents::reaper::{
    CliDockerNetworks, DockerNetworks, KeyReaper, NetworkInfo, NetworkReaper, PgRunActivity,
    RunActivity, RunState, parse_rfc3339,
};
use guard_core::ports::StoreError;
use guard_core::ports::fakes::FakeModelGateway;
use guard_core::ports::model_gateway::{KeyRequest, ModelGateway};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const NOW: &str = "2026-11-01T12:00:00Z";

fn now() -> i64 {
    parse_rfc3339(NOW).unwrap()
}

fn request(alias: &str, run_id: Option<&str>) -> KeyRequest {
    KeyRequest {
        alias: alias.into(),
        models: vec!["anthropic/claude-sonnet-4-5".into()],
        max_budget_usd: 1.0,
        rpm_limit: None,
        tpm_limit: None,
        max_parallel_requests: None,
        duration: Duration::from_secs(3600),
        metadata: run_id
            .map(|r| [("run_id".to_string(), r.to_string())].into())
            .unwrap_or_default(),
    }
}

struct Runs(BTreeMap<String, RunState>);

#[async_trait::async_trait]
impl RunActivity for Runs {
    async fn state(&self, run_id: &str) -> Result<RunState, StoreError> {
        if run_id == "a1-broken" {
            return Err(StoreError::Unavailable("db down".into()));
        }
        Ok(self.0.get(run_id).copied().unwrap_or(RunState::Unknown))
    }
}

#[tokio::test]
async fn keys_of_finished_and_unknown_runs_are_revoked_active_and_young_ones_kept() {
    let gw = Arc::new(FakeModelGateway::new());
    let active = gw
        .mint_key(&request("a1-active:coder:1", Some("a1-active")))
        .await
        .unwrap();
    let finished = gw
        .mint_key(&request("a1-done:coder:1", Some("a1-done")))
        .await
        .unwrap();
    let unknown = gw.mint_key(&request("a1-lost:qa:1", None)).await.unwrap();
    let young = gw
        .mint_key(&request("a1-young:qa:1", Some("a1-young")))
        .await
        .unwrap();
    let broken = gw
        .mint_key(&request("a1-broken:qa:1", Some("a1-broken")))
        .await
        .unwrap();
    let other = gw
        .mint_key(&request("exp:memory:SHOP-1:r1", Some("x")))
        .await
        .unwrap();
    for k in [&active, &finished, &unknown, &broken, &other] {
        gw.set_created_at(&k.key_id, "2026-11-01T10:00:00Z");
    }
    gw.set_created_at(&young.key_id, "2026-11-01T11:55:00Z");

    let runs = Runs(
        [
            ("a1-active".to_string(), RunState::Active),
            ("a1-done".to_string(), RunState::Finished),
        ]
        .into(),
    );
    let reaper = KeyReaper::new(gw.clone(), Arc::new(runs))
        .with_alias_prefix("a1-")
        .with_min_age(Duration::from_secs(15 * 60));
    let report = reaper.reap(now()).await.unwrap();

    assert_eq!(report.listed, 5);
    assert_eq!(
        report.revoked,
        vec![
            (
                "a1-done:coder:1".to_string(),
                "run a1-done finished".to_string()
            ),
            (
                "a1-lost:qa:1".to_string(),
                "run a1-lost unknown".to_string()
            ),
        ]
    );
    assert_eq!(report.kept, 3);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(report.errors[0].contains("db down"));
    assert!(gw.is_active(&active.key_id));
    assert!(gw.is_active(&young.key_id));
    assert!(gw.is_active(&broken.key_id));
    assert!(gw.is_active(&other.key_id), "outside the prefix");
    assert!(!gw.is_active(&finished.key_id));
    assert!(!gw.is_active(&unknown.key_id));

    // Idempotent: a second pass has nothing more to revoke.
    let again = reaper.reap(now()).await.unwrap();
    assert!(again.revoked.is_empty());
}

#[derive(Default)]
struct FakeNetworks {
    networks: Mutex<Vec<NetworkInfo>>,
    fail_rm: Option<String>,
}

#[async_trait::async_trait]
impl DockerNetworks for FakeNetworks {
    async fn list(&self, label: &str) -> Result<Vec<NetworkInfo>, String> {
        assert_eq!(label, "guard.sandbox");
        Ok(self.networks.lock().unwrap().clone())
    }
    async fn remove(&self, name: &str) -> Result<(), String> {
        if self.fail_rm.as_deref() == Some(name) {
            return Err("has active endpoints".into());
        }
        self.networks.lock().unwrap().retain(|n| n.name != name);
        Ok(())
    }
}

fn net(name: &str, containers: usize, created: &str) -> NetworkInfo {
    NetworkInfo {
        name: format!("{name}-net"),
        sandbox: name.to_string(),
        containers,
        created_at: Some(created.to_string()),
    }
}

#[tokio::test]
async fn empty_old_sandbox_networks_are_removed() {
    let docker = Arc::new(FakeNetworks {
        networks: Mutex::new(vec![
            net("guard-sbx-1", 0, "2026-11-01T10:00:00.1Z"),
            net("guard-sbx-2", 2, "2026-11-01T10:00:00Z"),
            net("guard-sbx-3", 0, "2026-11-01T11:59:00Z"),
            net("other-4", 0, "2026-11-01T10:00:00Z"),
            net("guard-sbx-5", 0, "2026-11-01T10:00:00Z"),
        ]),
        fail_rm: Some("guard-sbx-5-net".into()),
    });
    let report = NetworkReaper::new(docker.clone())
        .reap(now())
        .await
        .unwrap();
    assert_eq!(report.listed, 4);
    assert_eq!(report.removed, vec!["guard-sbx-1-net".to_string()]);
    assert_eq!(report.kept, 2);
    assert_eq!(report.errors.len(), 1);
    let left: Vec<_> = docker
        .networks
        .lock()
        .unwrap()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(
        left,
        [
            "guard-sbx-2-net",
            "guard-sbx-3-net",
            "other-4-net",
            "guard-sbx-5-net"
        ]
    );
}

#[tokio::test]
#[ignore = "live Postgres: DATABASE_URL"]
async fn pg_run_activity_reads_live_leases_and_recorded_runs() {
    let Some(pg) = common::pg().await else { return };
    let insert = |run_id: &str, ticket: &str, state: &str, lease: &str| {
        let (locked_by, locked_until) = if state == "running" {
            ("'w1'".to_string(), format!("now() + interval '{lease}'"))
        } else {
            ("NULL".to_string(), "NULL".to_string())
        };
        format!(
            "INSERT INTO task_queue (repo, ticket, role, payload, state, locked_by, locked_until)
             VALUES ('acme/shop', '{ticket}', 'coder', '{{\"input\": {{\"run_id\": \"{run_id}\"}}}}',
                     '{state}', {locked_by}, {locked_until})"
        )
    };
    for sql in [
        insert("run-live", "SHOP-1", "running", "10 minutes"),
        insert("run-dead-lease", "SHOP-2", "running", "-10 minutes"),
        insert("run-done", "SHOP-3", "succeeded", ""),
    ] {
        guard_store::sqlx::query(&sql)
            .execute(&pg.pool)
            .await
            .unwrap();
    }
    guard_store::sqlx::query(
        "INSERT INTO agent_runs (run_id, repo, ticket, role, attempt, agent, runtime, model,
                                 base_commit, status)
         VALUES ('run-done', 'acme/shop', 'SHOP-3', 'coder', 1, 'coder', 'fake', 'm',
                 'abc1234', 'completed')",
    )
    .execute(&pg.pool)
    .await
    .unwrap();

    let runs = PgRunActivity::new(pg.pool.clone());
    assert_eq!(runs.state("run-live").await.unwrap(), RunState::Active);
    assert_eq!(
        runs.state("run-dead-lease").await.unwrap(),
        RunState::Unknown
    );
    assert_eq!(runs.state("run-done").await.unwrap(), RunState::Finished);
    assert_eq!(runs.state("never").await.unwrap(), RunState::Unknown);

    // End to end with the fake gateway.
    let gw = Arc::new(FakeModelGateway::new());
    let live = gw
        .mint_key(&request("run-live:coder:1", Some("run-live")))
        .await
        .unwrap();
    let done = gw
        .mint_key(&request("run-done:coder:1", Some("run-done")))
        .await
        .unwrap();
    let report = KeyReaper::new(gw.clone(), Arc::new(runs))
        .with_alias_prefix("run-")
        .with_run_shape(|run| run.starts_with("run-"))
        .with_min_age(Duration::ZERO)
        .reap(now())
        .await
        .unwrap();
    assert_eq!(report.revoked.len(), 1);
    assert!(gw.is_active(&live.key_id));
    assert!(!gw.is_active(&done.key_id));
    pg.cleanup().await;
}

fn docker_tests() -> bool {
    std::env::var("GUARD_DOCKER_TESTS").as_deref() == Ok("1")
}

async fn docker(args: &[&str]) -> std::process::Output {
    tokio::process::Command::new("docker")
        .args(args)
        .output()
        .await
        .expect("run docker")
}

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1 and GUARD_SANDBOX_TEST_IMAGE"]
async fn the_cli_reaper_removes_only_empty_guard_networks() {
    if !docker_tests() {
        common::missing_live_env("GUARD_DOCKER_TESTS");
        return;
    }
    let Ok(image) = std::env::var("GUARD_SANDBOX_TEST_IMAGE") else {
        common::missing_live_env("GUARD_SANDBOX_TEST_IMAGE");
        return;
    };
    let prefix = format!("guard-reap-{}", std::process::id());
    let empty = format!("{prefix}-a");
    let busy = format!("{prefix}-b");
    for sandbox in [&empty, &busy] {
        let label = format!("guard.sandbox={sandbox}");
        let out = docker(&[
            "network",
            "create",
            "--internal",
            "--label",
            &label,
            &format!("{sandbox}-net"),
        ])
        .await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let container = format!("{busy}-c");
    let busy_net = format!("{busy}-net");
    let out = docker(&[
        "run",
        "-d",
        "--rm",
        "--name",
        &container,
        "--network",
        &busy_net,
        "--entrypoint",
        "sleep",
        &image,
        "120",
    ])
    .await;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let cli = Arc::new(CliDockerNetworks::default());
    let listed = cli.list("guard.sandbox").await.unwrap();
    let ours: Vec<_> = listed
        .iter()
        .filter(|n| n.sandbox.starts_with(&prefix))
        .collect();
    assert_eq!(ours.len(), 2, "{listed:?}");
    assert!(
        ours.iter()
            .all(|n| n.created_at.as_deref().and_then(parse_rfc3339).is_some())
    );

    // Young networks are kept.
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let kept = NetworkReaper::new(cli.clone())
        .with_sandbox_prefix(&prefix)
        .reap(now_unix)
        .await
        .unwrap();
    assert!(kept.removed.is_empty(), "{kept:?}");

    let report = NetworkReaper::new(cli.clone())
        .with_sandbox_prefix(&prefix)
        .with_min_age(Duration::ZERO)
        .reap(now_unix + 5)
        .await
        .unwrap();
    let _ = docker(&["rm", "-f", &container]).await;
    let _ = docker(&["network", "rm", &busy_net]).await;
    let _ = docker(&["network", "rm", &format!("{empty}-net")]).await;
    assert_eq!(report.removed, vec![format!("{empty}-net")], "{report:?}");
    assert_eq!(report.kept, 1);
}

#[tokio::test]
async fn the_default_reaper_only_touches_keys_shaped_like_lane_keys() {
    let gw = Arc::new(FakeModelGateway::new());
    let mut foreign = vec![];
    for (alias, run) in [
        ("analytics-dashboard", None),
        ("alice-laptop", None),
        ("api:prod", None),
        ("alice:dev:1", None),
        ("alpha:baseline:SHOP-1:r1", Some("a1793528430-shop-1")),
        ("a1793528430-shop-1", None),
        ("a1793528430-shop-1:admin:1", None),
        ("a1793528430-shop-1:coder:x", None),
        ("abc-shop-1:coder:1", None),
        // Lane-shaped alias whose metadata names another run.
        ("a1793528430-shop-1:qa:1", Some("someone-else")),
    ] {
        foreign.push(gw.mint_key(&request(alias, run)).await.unwrap());
    }
    let lane = gw
        .mint_key(&request(
            "a1793528430-shop-1:coder:2:c2:claude",
            Some("a1793528430-shop-1"),
        ))
        .await
        .unwrap();
    for k in foreign.iter().chain([&lane]) {
        gw.set_created_at(&k.key_id, "2026-11-01T10:00:00Z");
    }
    let runs = Runs(BTreeMap::new());
    let report = KeyReaper::new(gw.clone(), Arc::new(runs))
        .reap(now())
        .await
        .unwrap();
    assert_eq!(
        report.revoked,
        vec![(
            "a1793528430-shop-1:coder:2:c2:claude".to_string(),
            "run a1793528430-shop-1 unknown".to_string()
        )]
    );
    assert_eq!(report.listed, 1, "only guard's own keys are counted");
    for k in &foreign {
        assert!(gw.is_active(&k.key_id), "{k:?} is not a lane key");
    }
}
