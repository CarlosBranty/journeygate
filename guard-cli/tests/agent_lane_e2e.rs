//! End-to-end dry run of the Phase 4 agent lane on live Docker and Postgres,
//! with the scripted fake runtime, fake Jev, fake Jira and a fake gateway
//! container. No model and no provider is ever called.
//!
//! ```text
//! GUARD_DOCKER_TESTS=1 \
//! GUARD_SANDBOX_TEST_IMAGE=<repo>@sha256:<digest of deploy/sandbox/Dockerfile> \
//! DATABASE_URL=postgres://postgres:<pw>@localhost:55432/postgres \
//!   cargo test -p guard-cli --test agent_lane_e2e -- --include-ignored --nocapture
//! ```
//!
//! Flow: a Jira `issue_created` webhook for SHOP-42 (catalog area of
//! examples/demo-shop) → `guard serve --fake` triages it to lane A (thresholds
//! in active mode) → the agent lane queues (SHOP-42, qa) → QA attempt in a
//! Docker sandbox writes a spec → quality checks → red-first in a check
//! sandbox (a fresh container from the same image and `DockerSandbox`, the
//! base tree plus the spec copied in, no mounts, no egress) with real
//! Playwright (must fail by an assertion) → sealed → Coder attempt 1 also
//! touches a test file and is `diff_rejected` → Coder attempt 2 applies the
//! fix → guard gate (sealed spec and smoke, in another check sandbox) passes
//! → branch `agent/SHOP-42` → orchestrator writes agent_runs,
//! spend_records (from the fake gateway) and memory facts → `guard mcp`
//! `guard_history` (read-only reader login) returns those facts. Inside the
//! sandboxes the fake runtime probes egress: the gateway answers, example.com
//! and 1.1.1.1 do not, and the Coder cannot write the hidden tests dir.
//! The fixture repo has no `node_modules`: the checks use the image's
//! Playwright, and their reports come out under
//! `.guard/agents/work/check-results/`.

use guard_store::sqlx::{self, Row};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TICKET: &str = "SHOP-42";
const MODEL: &str = "anthropic/claude-sonnet-4-5";
const GATEWAY_HOST: &str = "gateway.guard.internal";
const SPEC: &str = "e2e-playwright/tests/features/SHOP-42.spec.ts";
const ROUTES: &str = "examples/demo-shop/src/products/routes.mjs";

const SPEC_SOURCE: &str = r#"import { expect, test } from '../../src/fixtures';

test.describe('SHOP-42: product pages warn when stock is low', { tag: ['@ticket:SHOP-42'] }, () => {
  test('shows how many are left when 3 or fewer remain', async ({ data, productPage }) => {
    const product = await data.product({ name: 'Last lamp', stock: 2, priceCents: 4900 });
    await productPage.goto(product.slug);
    await expect(productPage.stock).toHaveText('Only 2 left');
  });

  test('keeps the plain count when stock is high', async ({ data, productPage }) => {
    const product = await data.product({ name: 'Many mugs', stock: 12, priceCents: 900 });
    await productPage.goto(product.slug);
    await expect(productPage.stock).toHaveText('12 in stock');
  });
});
"#;

fn real_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn run(cmd: &mut Command) -> (bool, String) {
    let o = cmd.output().expect("command runs");
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn docker(args: &[&str]) -> String {
    let (ok, out) = run(Command::new("docker").args(args));
    assert!(ok, "docker {args:?}: {out}");
    out.trim().to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (ok, out) = run(Command::new("git")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "user.name=e2e",
            "-c",
            "user.email=e2e@test",
        ])
        .args(args)
        .current_dir(dir));
    assert!(ok, "git {args:?}: {out}");
    out.trim().to_string()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn exec_step(argv: &[&str], expect: i32) -> Value {
    json!({"exec": {"argv": argv, "expect_exit": expect}})
}

/// `node -e` that POSTs one chat completion through the gateway with the
/// sandbox's virtual key; exit 0 only on HTTP 200.
fn gateway_call() -> Value {
    let js = format!(
        "fetch('http://{GATEWAY_HOST}:4000/v1/chat/completions',{{method:'POST',headers:{{authorization:'Bearer '+process.env.GUARD_GATEWAY_KEY,'content-type':'application/json'}},body:JSON.stringify({{model:'{MODEL}',messages:[{{role:'user',content:'hi'}}]}})}}).then(r=>{{console.log('gateway',r.status);process.exit(r.status===200?0:9)}}).catch(e=>{{console.log(String(e));process.exit(8)}})"
    );
    exec_step(&["node", "-e", &js], 0)
}

/// Exit 7 when `url` is unreachable (the expected outcome), 0 when it answers.
fn blocked(url: &str) -> Value {
    let js = format!(
        "fetch('{url}',{{signal:AbortSignal.timeout(8000)}}).then(r=>{{console.log('reached',r.status);process.exit(0)}}).catch(e=>{{console.log('blocked',e.cause?e.cause.code:String(e));process.exit(7)}})"
    );
    exec_step(&["node", "-e", &js], 7)
}

fn model_event() -> Value {
    json!({"event": {"kind": "model_call", "model": MODEL,
        "usage": {"input_tokens": 1200, "output_tokens": 40, "cached_input_tokens": 1000, "cache_creation_input_tokens": 0},
        "cost_usd": 0.0123}})
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

/// Copies the git-tracked files under `prefixes` of the real checkout.
fn copy_tracked(real: &Path, to: &Path, prefixes: &[&str]) {
    let files = git(real, &["ls-files", "-z", "--"]);
    for f in files.split('\0').filter(|f| !f.is_empty()) {
        if !prefixes
            .iter()
            .any(|p| f == *p || f.starts_with(&format!("{p}/")))
        {
            continue;
        }
        let target = to.join(f);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        // The committed bytes (LF), not the CRLF working copy.
        let bytes = Command::new("git")
            .args(["show", &format!("HEAD:{f}")])
            .current_dir(real)
            .output()
            .unwrap()
            .stdout;
        std::fs::write(target, bytes).unwrap();
    }
}

/// `(role, attempt, outcome, diff_paths, detail)` of one `agent_runs` row.
type RunRow = (String, i32, Option<String>, Vec<String>, Option<String>);

struct Cleanup {
    containers: Vec<String>,
    networks: Vec<String>,
    serve: Option<std::process::Child>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(mut c) = self.serve.take() {
            let _ = Command::new("taskkill")
                .args(["/PID", &c.id().to_string(), "/T", "/F"])
                .output();
            let _ = c.kill();
            let _ = c.wait();
        }
        for c in &self.containers {
            let _ = Command::new("docker").args(["rm", "-f", c]).output();
        }
        // Sandboxes of this ticket that a failure may have left behind
        // (only these: other live suites may share the daemon).
        if let Ok(o) = Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                "label=guard.sandbox",
                "--filter",
                &format!("label=guard.ticket={TICKET}"),
            ])
            .output()
        {
            for id in String::from_utf8_lossy(&o.stdout).split_whitespace() {
                let _ = Command::new("docker").args(["rm", "-f", id]).output();
            }
        }
        for n in &self.networks {
            let _ = Command::new("docker").args(["network", "rm", n]).output();
        }
    }
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn guard_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_guard"))
}

#[test]
#[ignore = "live Docker + Postgres: GUARD_DOCKER_TESTS=1, GUARD_SANDBOX_TEST_IMAGE, DATABASE_URL"]
fn lane_a_ticket_flows_through_qa_coder_gate_and_pr() {
    if std::env::var("GUARD_DOCKER_TESTS").as_deref() != Ok("1") {
        missing_live_env("GUARD_DOCKER_TESTS=1");
        return;
    }
    let image = std::env::var("GUARD_SANDBOX_TEST_IMAGE")
        .expect("GUARD_SANDBOX_TEST_IMAGE=<repo>@sha256:<digest>");
    let admin_url = std::env::var("DATABASE_URL").expect("DATABASE_URL");
    let id = format!(
        "{}{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 1_000_000
    );
    let real = real_root();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut cleanup = Cleanup {
        containers: vec![],
        networks: vec![],
        serve: None,
    };

    // ------------------------------------------------------------ fixture repo
    let dir = tempfile::Builder::new()
        .prefix("guard-e2e-")
        .tempdir()
        .unwrap();
    let repo = dir.path().join("repo");
    // Sandbox work dirs outside the fixture (its node_modules are junctions).
    let work = tempfile::Builder::new()
        .prefix("guard-e2e-work-")
        .tempdir()
        .unwrap();
    copy_tracked(
        &real,
        &repo,
        &[
            "examples/demo-shop",
            "e2e-playwright",
            "agents",
            ".gitignore",
        ],
    );
    let target = std::fs::read_to_string(real.join("target.toml"))
        .unwrap()
        .replace("\r\n", "\n");
    std::fs::write(
        repo.join("target.toml"),
        format!("{target}\n[tracker]\nkind = \"jira\"\nproject = \"SHOP\"\n"),
    )
    .unwrap();
    std::fs::write(repo.join("thresholds.toml"), "mode = \"active\"\n").unwrap();
    std::fs::write(
        repo.join("agents.toml"),
        format!(
            r#"version = 1

[gateway]
url = "http://{GATEWAY_HOST}:4000"
master_key_env = "GUARD_E2E_GATEWAY_MASTER_KEY"
key_env = "GUARD_GATEWAY_KEY"
key_duration_secs = 3600

[roles]
qa = "qa"
coder = "coder"

[agents.qa]
role = "qa"
runtime = "fake"
model = "{MODEL}"
budget_usd = 1.0
max_attempts = 1
timeout_secs = 600

[agents.coder]
role = "coder"
runtime = "fake"
model = "{MODEL}"
budget_usd = 1.0
max_attempts = 3
timeout_secs = 600

[scheduler]
host_slots = 1
daily_usd_cap = 5.0
provider_slots = {{ anthropic = 1 }}
lease_secs = 900
image = "{image}"
"#
        ),
    )
    .unwrap();
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "demo-shop base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    println!("[fixture] repo {} at base {base}", repo.display());

    // ------------------------------------------------------------ fake plans
    let plans = dir.path().join("plans").join(TICKET);
    let routes = std::fs::read_to_string(repo.join(ROUTES)).unwrap();
    let old = "const stock = product.stock > 0 ? `${product.stock} in stock` : 'Out of stock';";
    assert!(routes.contains(old), "routes.mjs changed; update the fix");
    let fixed = routes.replace(
        old,
        "const stock =\n    product.stock > 3\n      ? `${product.stock} in stock`\n      : product.stock > 0\n        ? `Only ${product.stock} left`\n        : 'Out of stock';",
    );
    write_json(
        &plans.join("qa-1.json"),
        &json!({
            "steps": [
                gateway_call(),
                blocked("http://example.com/"),
                blocked("http://1.1.1.1/"),
                {"edit": {"path": SPEC, "content": SPEC_SOURCE}},
                model_event(),
            ],
            "result": {"status": "completed", "summary": "Spec for the low-stock warning",
                       "memory_proposals": [{"topic_key": "hint/catalog-stock-text",
                         "body": "The stock text of product pages is rendered in examples/demo-shop/src/products/routes.mjs"}]}
        }),
    );
    let hidden_write = "try{require('fs').writeFileSync('/workspace/e2e-playwright/tests/features/hack.spec.ts','x');console.log('wrote');process.exit(0)}catch(e){console.log('denied',e.code);process.exit(7)}";
    write_json(
        &plans.join("coder-1.json"),
        &json!({
            "steps": [
                exec_step(&["node", "-e", hidden_write], 7),
                {"edit": {"path": ROUTES, "content": fixed}},
                {"edit": {"path": "examples/demo-shop/test/stock-text.test.mjs",
                          "content": "// weakened test the Coder must not write\n"}},
                model_event(),
            ],
            "result": {"status": "completed", "summary": "Fix plus a test edit"}
        }),
    );
    write_json(
        &plans.join("coder-2.json"),
        &json!({
            "steps": [
                gateway_call(),
                blocked("http://example.com/"),
                {"edit": {"path": ROUTES, "content": fixed}},
                model_event(),
            ],
            "result": {"status": "completed", "summary": "Show 'Only N left' when 1 <= stock <= 3"}
        }),
    );

    // ------------------------------------------------------------ fake gateway
    let network = format!("guard-e2e-{id}");
    docker(&["network", "create", &network]);
    cleanup.networks.push(network.clone());
    let gw = format!("guard-e2e-gw-{id}");
    let master = format!("mk-{id}");
    let srv = real.join("deploy/litellm");
    docker(&[
        "run",
        "-d",
        "--name",
        &gw,
        "--network",
        &network,
        "--network-alias",
        GATEWAY_HOST,
        "-p",
        "127.0.0.1::4000",
        "-e",
        &format!("FAKE_GATEWAY_MASTER_KEY={master}"),
        "-v",
        &format!("{}:/srv:ro", srv.display()),
        "node:24-bookworm-slim",
        "node",
        "/srv/fake-gateway.mjs",
    ]);
    cleanup.containers.push(gw.clone());
    let gw_port = docker(&["port", &gw, "4000/tcp"])
        .lines()
        .next()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .to_string();
    let gw_admin = format!("http://127.0.0.1:{gw_port}");
    let http = reqwest::Client::new();
    rt.block_on(async {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(r) = http.get(format!("{gw_admin}/health")).send().await {
                if r.status().is_success() {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "fake gateway did not start");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    });
    println!("[gateway] fake gateway {gw} on {network} as {GATEWAY_HOST}:4000, admin {gw_admin}");

    // ------------------------------------------------------------ postgres
    let db = format!("guard_e2e_{id}");
    let reader_role = format!("guard_e2e_reader_{id}");
    let db_url = format!("{}/{db}", admin_url.rsplit_once('/').unwrap().0);
    let reader_url = db_url.replacen(
        &admin_url
            .split("://")
            .nth(1)
            .unwrap()
            .split('@')
            .next()
            .unwrap()
            .to_string(),
        &format!("{reader_role}:reader"),
        1,
    );
    let admin = rt.block_on(async {
        let admin = sqlx::PgPool::connect(&admin_url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE {db}"))
            .execute(&admin)
            .await
            .unwrap();
        sqlx::query(&format!(
            "CREATE ROLE {reader_role} LOGIN PASSWORD 'reader'"
        ))
        .execute(&admin)
        .await
        .unwrap();
        admin
    });
    println!("[postgres] database {db}, reader login {reader_role}");

    // ------------------------------------------------------------ guard serve
    let listen = free_port();
    let log_path = dir.path().join("serve.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let common_env: Vec<(&str, String)> = vec![
        ("DATABASE_URL", db_url.clone()),
        ("GUARD_REPO", "acme/demo-shop".into()),
        ("GUARD_SERVER_TOKEN", "e2e-api-token".into()),
        ("GUARD_JIRA_WEBHOOK_SECRET", "e2e-webhook".into()),
        (
            "GUARD_AGENT_FAKE_PLANS",
            dir.path().join("plans").display().to_string(),
        ),
        ("GUARD_GATEWAY_ADMIN_URL", gw_admin.clone()),
        ("GUARD_E2E_GATEWAY_MASTER_KEY", master.clone()),
        ("GUARD_SANDBOX_EGRESS_NETWORK", network.clone()),
        ("GUARD_TEST_TOKEN", "e2e-test-token".into()),
        ("GUARD_AGENT_WORK_DIR", work.path().display().to_string()),
        // The attempt dirs are inspected below (they are removed by default).
        ("GUARD_AGENT_KEEP_WORK", "1".to_string()),
    ];
    let serve = Command::new(guard_bin())
        .args([
            "serve",
            "--fake",
            "--listen",
            &format!("127.0.0.1:{listen}"),
        ])
        .current_dir(&repo)
        .envs(common_env.iter().map(|(k, v)| (*k, v.as_str())))
        .env_remove("GUARD_MEMORY_READ_DATABASE_URL")
        // The checks must run sandboxed (the default).
        .env_remove("GUARD_UNSAFE_HOST_CHECKS")
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    cleanup.serve = Some(serve);
    let base_url = format!("http://127.0.0.1:{listen}");
    rt.block_on(async {
        let deadline = Instant::now() + Duration::from_secs(120);
        while http
            .get(format!("{base_url}/healthz"))
            .send()
            .await
            .is_err()
        {
            assert!(
                Instant::now() < deadline,
                "guard serve did not start: {}",
                std::fs::read_to_string(&log_path).unwrap_or_default()
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
    let serve_log = std::fs::read_to_string(&log_path).unwrap_or_default();
    println!(
        "[serve] {}",
        serve_log
            .lines()
            .find(|l| l.contains("listening"))
            .unwrap_or("?")
    );
    assert!(serve_log.contains("agents fake runtime"), "{serve_log}");
    // Memory writers, staleness and the lane's reapers are wired; --fake
    // keeps agent PRs on the local branch.
    assert!(serve_log.contains("memory postgres"), "{serve_log}");
    assert!(
        serve_log
            .contains("serve: jobs: memory staleness, sandbox reaper, key reaper, network reaper"),
        "{serve_log}"
    );
    assert!(
        serve_log.contains("agents: PRs: local branch (--fake)"),
        "{serve_log}"
    );
    assert!(
        !serve_log.contains("UNSAFE"),
        "host checks are off: {serve_log}"
    );
    // The reader login exists once the migration created guard_memory_reader.
    rt.block_on(async {
        sqlx::query(&format!("GRANT guard_memory_reader TO {reader_role}"))
            .execute(&admin)
            .await
            .unwrap();
    });

    // ------------------------------------------------------------ the webhook
    let hook = json!({
        "webhookEvent": "jira:issue_created",
        "user": {"accountId": "human"},
        "issue": {"key": TICKET, "fields": {
            "summary": "Catalog: product pages warn when stock is low",
            "issuetype": {"name": "Story"},
            "labels": [],
            "description": "Product pages in `examples/demo-shop/src/products/routes.mjs` must warn shoppers.\n- With 1 to 3 items left the page shows Only N left\n- With more it keeps N in stock"
        }}
    });
    let status = rt.block_on(async {
        http.post(format!("{base_url}/webhooks/jira"))
            .bearer_auth("e2e-webhook")
            .json(&hook)
            .send()
            .await
            .unwrap()
            .status()
    });
    println!("[jira] POST /webhooks/jira issue_created {TICKET} -> {status}");
    assert_eq!(status.as_u16(), 202);

    // ------------------------------------------------------------ wait
    let started = Instant::now();
    let status_json = loop {
        std::thread::sleep(Duration::from_secs(5));
        let out = Command::new(guard_bin())
            .args(["agents", "status", "--json", "--ticket", TICKET])
            .current_dir(&repo)
            .envs(common_env.iter().map(|(k, v)| (*k, v.as_str())))
            .output()
            .unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        let tasks = v["tasks"].as_array().cloned().unwrap_or_default();
        let runs = v["runs"].as_array().cloned().unwrap_or_default();
        let terminal = |t: &Value| t["state"] != "queued" && t["state"] != "running";
        let qa_gave_up = runs
            .iter()
            .any(|r| r["role"] == "qa" && r["outcome"] != "passed" && !r["outcome"].is_null())
            && tasks.iter().all(terminal);
        let done = tasks.iter().any(|t| t["role"] == "coder" && terminal(t))
            || tasks.iter().any(|t| t["state"] == "failed")
            || qa_gave_up;
        if done {
            break v;
        }
        assert!(
            started.elapsed() < Duration::from_secs(1500),
            "timed out; serve log:\n{}",
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
    };
    let serve_log = std::fs::read_to_string(&log_path).unwrap_or_default();
    println!("[serve log]\n{serve_log}");
    // Every periodic job ran at startup without failing (the key reaper
    // lists keys through the fake gateway's GET /key/list).
    for job in [
        "memory staleness",
        "sandbox reaper",
        "key reaper",
        "network reaper",
    ] {
        assert!(
            serve_log.contains(&format!("jobs: {job}: ")),
            "{job} never ran:\n{serve_log}"
        );
        assert!(
            !serve_log.contains(&format!("jobs: {job} failed")),
            "{job} failed:\n{serve_log}"
        );
    }
    println!(
        "[status] after {}s:\n{}",
        started.elapsed().as_secs(),
        serde_json::to_string_pretty(&status_json).unwrap()
    );

    // ------------------------------------------------------------ agent_runs
    let runs = rt.block_on(async {
        let pool = sqlx::PgPool::connect(&db_url).await.unwrap();
        let runs = sqlx::query(
            "SELECT role, attempt, status, outcome, diff_paths, key_alias, sealed_spec_sha256,
                    facts_brief_ids, result->>'detail' AS detail
             FROM agent_runs WHERE ticket = $1 ORDER BY role DESC, attempt",
        )
        .bind(TICKET)
        .fetch_all(&pool)
        .await
        .unwrap();
        let rows: Vec<RunRow> = runs
            .iter()
            .map(|r| {
                (
                    r.get("role"),
                    r.get("attempt"),
                    r.get("outcome"),
                    r.get("diff_paths"),
                    r.get("detail"),
                )
            })
            .collect();
        let spend = sqlx::query(
            "SELECT key_alias, model, spend_usd::float8 AS usd, prompt_tokens, cached_tokens
             FROM spend_records ORDER BY start_time",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let spend: Vec<(Option<String>, f64, i64, i64)> = spend
            .iter()
            .map(|r| {
                (
                    r.get("key_alias"),
                    r.get("usd"),
                    r.get("prompt_tokens"),
                    r.get("cached_tokens"),
                )
            })
            .collect();
        let facts =
            sqlx::query("SELECT id, topic_key, body, source, status FROM memory_facts ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let facts: Vec<(i64, String, String)> = facts
            .iter()
            .map(|r| (r.get("id"), r.get("topic_key"), r.get("body")))
            .collect();
        let candidates: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_fact_candidates")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        (rows, spend, facts, candidates)
    });
    let (rows, spend, facts, candidates) = runs;
    println!("[agent_runs]");
    for r in &rows {
        println!(
            "  {} #{} outcome={:?} paths={:?} detail={:?}",
            r.0, r.1, r.2, r.3, r.4
        );
    }
    println!("[spend_records]");
    for s in &spend {
        println!(
            "  alias={:?} usd={} prompt={} cached={}",
            s.0, s.1, s.2, s.3
        );
    }
    println!("[memory_facts]");
    for f in &facts {
        println!("  #{} {} :: {}", f.0, f.1, f.2);
    }
    println!("[memory_fact_candidates] {candidates}");

    let summary: Vec<(&str, i32, Option<&str>)> = rows
        .iter()
        .map(|r| (r.0.as_str(), r.1, r.2.as_deref()))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("qa", 1, Some("passed")),
            ("coder", 1, Some("diff_rejected")),
            ("coder", 2, Some("passed")),
        ],
        "serve log:\n{serve_log}"
    );
    assert!(
        rows[1]
            .4
            .as_deref()
            .unwrap_or("")
            .contains("examples/demo-shop/test/stock-text.test.mjs")
    );
    assert_eq!(rows[2].3, vec![ROUTES.to_string()]);
    let aliases: Vec<&str> = spend.iter().filter_map(|s| s.0.as_deref()).collect();
    assert!(aliases.iter().any(|a| a.ends_with(":qa:1")), "{aliases:?}");
    assert!(
        aliases.iter().any(|a| a.ends_with(":coder:2")),
        "{aliases:?}"
    );
    let topics: Vec<&str> = facts.iter().map(|f| f.1.as_str()).collect();
    assert_eq!(
        topics,
        vec!["ticket/shop-42/sealed-spec", "ticket/shop-42/agent-pr"]
    );
    assert_eq!(
        candidates, 1,
        "the agent proposal is a candidate, not a fact"
    );

    // ------------------------------------------------------------ check sandboxes
    // Red-first and the gate ran in check sandboxes: their reports were
    // copied out, and no check worktree (the old host path) was created.
    let work_dir = repo.join(".guard/agents/work");
    let results: Vec<PathBuf> = std::fs::read_dir(work_dir.join("check-results"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    println!("[checks] results {results:?}");
    for what in ["red-first", "gate"] {
        let dir = results
            .iter()
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(&format!("-{what}-"))
            })
            .unwrap_or_else(|| panic!("no {what} check results in {results:?}"));
        let report: Value =
            serde_json::from_slice(&std::fs::read(dir.join("report.json")).unwrap()).unwrap();
        println!("[checks] {what}: stats {}", report["stats"]);
        assert!(
            report["config"]["rootDir"]
                .as_str()
                .unwrap_or_default()
                .starts_with("/workspace/"),
            "Playwright ran inside the container: {}",
            report["config"]["rootDir"]
        );
        assert!(dir.join("app.log").is_file());
    }
    let red = results
        .iter()
        .find(|p| p.to_string_lossy().contains("-red-first-"))
        .unwrap();
    let shots = walk_files(&red.join("test-results"));
    println!("[checks] red-first screenshots {shots:?}");
    assert!(
        shots
            .iter()
            .any(|p| p.extension().is_some_and(|e| e == "png")),
        "the red-first failure screenshots are copied out"
    );
    let worktrees: Vec<_> = std::fs::read_dir(work_dir.join("checks"))
        .map(|d| {
            d.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        worktrees.is_empty(),
        "no host check worktree: {worktrees:?}"
    );

    // ------------------------------------------------------------ sandbox evidence
    // The /task dirs of every attempt: the egress probes (exec:node tool calls,
    // each with its expected exit code) and the facts brief in task.json.
    let run_dir = std::fs::read_dir(work.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.join(TICKET).is_dir())
        .expect("attempt dirs")
        .join(TICKET);
    for attempt in ["qa-1-qa", "coder-1-coder", "coder-2-coder"] {
        let task_dir = run_dir.join(attempt).join("task");
        let events = std::fs::read_to_string(task_dir.join("events.jsonl")).unwrap_or_default();
        let probes: Vec<String> = events
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|e| e["tool"].as_str().is_some_and(|t| t.starts_with("exec:")))
            .map(|e| format!("{} ok={}", e["tool"], e["ok"]))
            .collect();
        let task: Value =
            serde_json::from_slice(&std::fs::read(task_dir.join("task.json")).unwrap()).unwrap();
        println!(
            "[sandbox] {attempt}: probes {probes:?}; tools {}; facts_brief ids {}",
            task["tools"], task["facts_brief"]["fact_ids"]
        );
        assert!(probes.iter().all(|p| p.ends_with("ok=true")), "{probes:?}");
    }
    let coder_task: Value = serde_json::from_slice(
        &std::fs::read(run_dir.join("coder-2-coder/task/task.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        coder_task["facts_brief"]["fact_ids"],
        json!([facts[0].0]),
        "the Coder's task.json carries the orchestrator's sealed-spec fact"
    );
    println!(
        "[sandbox] coder-2 facts_brief text:
{}",
        coder_task["facts_brief"]["text"].as_str().unwrap_or("")
    );

    // ------------------------------------------------------------ the PR branch
    let branch_routes = git(&repo, &["show", &format!("agent/{TICKET}:{ROUTES}")]);
    assert!(branch_routes.contains("Only ${product.stock} left"));
    let branch_spec = git(&repo, &["show", &format!("agent/{TICKET}:{SPEC}")]);
    assert_eq!(branch_spec.trim_end(), SPEC_SOURCE.trim_end());
    assert_eq!(
        git(&repo, &["rev-parse", &format!("agent/{TICKET}^")]),
        base
    );
    assert_eq!(git(&repo, &["rev-parse", "main"]), base, "nothing merged");
    let files = git(
        &repo,
        &["diff", "--name-only", &base, &format!("agent/{TICKET}")],
    );
    println!("[pr] agent/{TICKET} files:\n{files}");
    let record =
        std::fs::read_to_string(repo.join(format!(".guard/agents/prs/{TICKET}.md"))).unwrap();
    println!("[pr] record:\n{record}");
    assert!(record.contains("human approval required"));

    // ------------------------------------------------------------ guard_history
    let mut mcp = Command::new(guard_bin())
        .arg("mcp")
        .current_dir(&repo)
        .envs(common_env.iter().map(|(k, v)| (*k, v.as_str())))
        .env("GUARD_MEMORY_READ_DATABASE_URL", &reader_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = mcp.stdin.take().unwrap();
    let mut out = std::io::BufReader::new(mcp.stdout.take().unwrap());
    fn ask(
        stdin: &mut std::process::ChildStdin,
        out: &mut std::io::BufReader<std::process::ChildStdout>,
        msg: Value,
    ) -> Value {
        writeln!(stdin, "{msg}").unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    ask(
        &mut stdin,
        &mut out,
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "1"}}}),
    );
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .unwrap();
    let history = ask(
        &mut stdin,
        &mut out,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "guard_history", "arguments": {"topic_prefix": "ticket/shop-42/"}}}),
    );
    let map = ask(
        &mut stdin,
        &mut out,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "guard_map", "arguments": {"focus_files": [ROUTES], "token_budget": 256}}}),
    );
    drop(stdin);
    let _ = mcp.wait();
    println!(
        "[guard_history]\n{}",
        serde_json::to_string_pretty(&history["result"]["structuredContent"]).unwrap()
    );
    println!(
        "[guard_map]\n{}",
        serde_json::to_string_pretty(&map["result"]["structuredContent"]).unwrap()
    );
    assert_eq!(history["result"]["isError"], false, "{history}");
    let got: Vec<&str> = history["result"]["structuredContent"]["facts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["topic_key"].as_str().unwrap())
        .collect();
    assert_eq!(
        got,
        vec!["ticket/shop-42/sealed-spec", "ticket/shop-42/agent-pr"]
    );
    assert_eq!(map["result"]["isError"], false, "{map}");

    // The reader login cannot write memory (enforced by Postgres, not by convention).
    rt.block_on(async {
        let pool = sqlx::PgPool::connect(&reader_url).await.unwrap();
        let err = sqlx::query("DELETE FROM memory_facts")
            .execute(&pool)
            .await
            .unwrap_err();
        println!("[reader] DELETE FROM memory_facts as {reader_role}: {err}");
        assert!(err.to_string().contains("permission denied"), "{err}");
        pool.close().await;
    });

    // ------------------------------------------------------------ cleanup
    drop(cleanup);
    rt.block_on(async {
        let _ = sqlx::query(&format!("DROP DATABASE {db} WITH (FORCE)"))
            .execute(&admin)
            .await;
        let _ = sqlx::query(&format!("DROP ROLE {reader_role}"))
            .execute(&admin)
            .await;
    });
}

/// A live test was asked to run (`--include-ignored`) without `var`: fail,
/// so a CI run that forgot it never reports a vacuous `ok`.
/// `GUARD_ALLOW_SKIPPED_LIVE_TESTS=1` turns that into a skip.
fn missing_live_env(var: &str) {
    if std::env::var("GUARD_ALLOW_SKIPPED_LIVE_TESTS").as_deref() == Ok("1") {
        eprintln!("skipped: {var} not set (GUARD_ALLOW_SKIPPED_LIVE_TESTS=1)");
        return;
    }
    panic!("{var} is not set; set it, or GUARD_ALLOW_SKIPPED_LIVE_TESTS=1 to skip live tests");
}
