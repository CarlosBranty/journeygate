//! `guard agents check|sync|run|status` (docs/contracts/agents-toml.md, cli.md).
//!
//! - `check`: validates `agents.toml` (exit 2 on error).
//! - `sync [--check]`: renders `agents/` per runtime (exit 1 with `--check`
//!   when out of date).
//! - `run <TICKET>`: enqueues `(ticket, qa)` and, unless `--enqueue-only`,
//!   runs a worker in this process until the ticket reaches its PR branch
//!   `agent/<KEY>` (exit 0) or the human lane (exit 1). Every agent PR needs
//!   a human approval; nothing merges.
//! - `status`: queue tasks and `agent_runs` of this repo.

use crate::agent_lane::{self, LaneOptions, TicketSource};
use crate::cli::AgentsRunArgs;
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_FAILED, EXIT_OK};
use crate::runtime::block_on;
use guard_agents::config::{AgentsConfig, LoadOptions, RuntimeKind};
use guard_agents::lane::{AfterStage, LoopStep, step};
use guard_agents::worker::{Disposition, Idle, enqueue_ticket};
use guard_core::ports::{Issue, StatusCategory};
use guard_store::sqlx::{self, Row};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

pub fn check(ctx: &Context) -> CliResult<u8> {
    let path = agent_lane::agents_toml_path(ctx);
    let config = AgentsConfig::load(&path, &LoadOptions::from_env())
        .map_err(|e| CliError::usage(format!("{}: {e}", ctx.display(&path))))?;
    let runtimes: Vec<&str> = config
        .runtimes_in_use()
        .iter()
        .map(|r| r.as_str())
        .collect();
    println!(
        "agents: {} ok ({} agents; runtimes {}; qa = {}, coder = {})",
        ctx.display(&path),
        config.agents.len(),
        runtimes.join(", "),
        config.roles.qa,
        config.roles.coder
    );
    for rt in config.runtimes_in_use() {
        if guard_runtimes::admission(rt.as_str()) == Some(guard_runtimes::Admission::Provisional) {
            crate::error::warning(format!(
                "runtime {} is provisional: part of its gateway pinning is unverified",
                rt.as_str()
            ));
        }
    }
    Ok(EXIT_OK)
}

pub fn sync(ctx: &Context, check: bool, runtimes: &[String]) -> CliResult<u8> {
    let mut chosen = BTreeSet::new();
    for name in runtimes {
        chosen.insert(
            RuntimeKind::from_name(name)
                .ok_or_else(|| CliError::usage(format!("unknown runtime {name:?}")))?,
        );
    }
    if chosen.is_empty() {
        let path = agent_lane::agents_toml_path(ctx);
        chosen = if path.is_file() {
            AgentsConfig::load(&path, &LoadOptions::from_env())
                .map_err(|e| CliError::usage(format!("{}: {e}", ctx.display(&path))))?
                .runtimes_in_use()
        } else {
            [
                RuntimeKind::ClaudeCode,
                RuntimeKind::Codex,
                RuntimeKind::CopilotCli,
            ]
            .into()
        };
    }
    chosen.remove(&RuntimeKind::Fake);
    let report = guard_agents::sync::sync(
        &ctx.root,
        &guard_agents::sync::SyncOptions {
            runtimes: chosen,
            check,
        },
    )
    .map_err(|e| CliError::usage(e.to_string()))?;
    let verb = if check { "would write" } else { "wrote" };
    for p in &report.written {
        println!("sync: {verb} {p}");
    }
    for p in &report.removed {
        println!(
            "sync: {} {p}",
            if check { "would remove" } else { "removed" }
        );
    }
    println!(
        "sync: {} written, {} removed, {} unchanged",
        report.written.len(),
        report.removed.len(),
        report.unchanged.len()
    );
    Ok(if check && !report.is_clean() {
        EXIT_FAILED
    } else {
        EXIT_OK
    })
}

fn issue_from(ctx: &Context, args: &AgentsRunArgs) -> CliResult<Issue> {
    if let Some(path) = &args.ticket_file {
        let text = std::fs::read_to_string(super::absolute(path))
            .map_err(|e| CliError::usage(format!("{}: {e}", path.display())))?;
        let mut value: Value = serde_json::from_str(&text)
            .map_err(|e| CliError::usage(format!("{}: {e}", path.display())))?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| CliError::usage("the ticket file must be a JSON object"))?;
        obj.entry("key").or_insert(json!(args.ticket));
        for (k, v) in [
            ("url", json!("")),
            ("description", json!("")),
            ("issue_type", json!("Story")),
            ("status", json!("To Do")),
            ("status_category", json!(StatusCategory::ToDo)),
            ("labels", json!([])),
        ] {
            obj.entry(k).or_insert(v);
        }
        let issue: Issue = serde_json::from_value(value)
            .map_err(|e| CliError::usage(format!("{}: {e}", path.display())))?;
        if issue.key != args.ticket {
            return Err(CliError::usage(format!(
                "{} holds {}, not {}",
                path.display(),
                issue.key,
                args.ticket
            )));
        }
        return Ok(issue);
    }
    let config = ctx.load_config()?;
    let tracker = config
        .tracker
        .as_ref()
        .ok_or_else(|| CliError::usage("no [tracker] in target.toml; pass --ticket-file"))?;
    let jira = guard_tracker::JiraConfig::from_env(
        tracker.project.as_deref().unwrap_or_default(),
        guard_tracker::DEFAULT_TOKEN_VAR,
    )
    .and_then(guard_tracker::JiraTracker::new)
    .map_err(|e| CliError::env(format!("jira: {e}")))?;
    block_on(async {
        use guard_core::ports::IssueTracker;
        jira.get_issue(&args.ticket)
            .await
            .map_err(|e| CliError::env(format!("jira: {e}")))
    })
}

fn print_attempts(step: &LoopStep) {
    let LoopStep::Ran(ran, after) = step else {
        return;
    };
    if let Some(report) = &ran.report {
        for a in &report.attempts {
            println!(
                "agents: {} {} attempt {} ({}, {}): {}{}",
                ran.ticket,
                a.role.as_str(),
                a.attempt,
                a.agent,
                a.runtime,
                a.outcome.map_or("no outcome", |o| o.as_str()),
                a.detail
                    .as_deref()
                    .map(|d| format!(" - {d}"))
                    .unwrap_or_default()
            );
        }
    }
    let what = match &ran.disposition {
        Disposition::Sealed { coder_task } => format!("sealed; coder task {} queued", coder_task.0),
        Disposition::Passed { pr } => format!("PR {} opened (human approval required)", pr.url),
        Disposition::HumanLane(r) => format!(
            "human lane: {}",
            serde_json::to_string(r).unwrap_or_default()
        ),
        Disposition::Retrying { error, after } => {
            format!("retrying in {}s: {error}", after.as_secs())
        }
        Disposition::Failed { error } => format!("failed: {error}"),
    };
    println!("agents: {} {}: {what}", ran.ticket, ran.role.as_str());
    print_after(after);
}

fn print_after(after: &AfterStage) {
    println!(
        "agents: orchestrator recorded {} agent_runs row(s), {} spend record(s), {} memory candidate(s), facts {:?}",
        after.runs_recorded, after.spend_records, after.candidates_enqueued, after.facts_recorded
    );
    for w in &after.warnings {
        crate::error::warning(format!("agents: {w}"));
    }
}

pub fn run(ctx: &Context, args: &AgentsRunArgs) -> CliResult<u8> {
    let runtime = args
        .runtime
        .as_deref()
        .map(|n| {
            RuntimeKind::from_name(n)
                .ok_or_else(|| CliError::usage(format!("unknown runtime {n:?}")))
        })
        .transpose()?;
    let target = ctx.load_config()?;
    let mut issue = issue_from(ctx, args)?;
    issue
        .labels
        .extend(args.area.iter().map(|a| format!("area:{a}")));
    let base = crate::git::head_sha(&ctx.root).map_err(|e| CliError::env(e.to_string()))?;
    block_on(async {
        let lane = agent_lane::build(
            ctx,
            &target,
            &LaneOptions {
                fake: args.fake,
                runtime,
            },
        )
        .await?;
        eprintln!("agents: PRs: {}", lane.publisher.describe());
        let input = agent_lane::ticket_input(
            &TicketSource {
                config: &target,
                repo: &lane.repo,
                tests_dir: &lane.tests_dir,
                base_commit: &base,
                impact: None,
            },
            &issue,
            &[],
        )
        .map_err(CliError::usage)?;
        let queued = enqueue_ticket(lane.queue.as_ref(), &input, 0)
            .await
            .map_err(|e| CliError::env(e.to_string()))?;
        println!(
            "agents: {} queued as task {} (run {}, base {})",
            input.ticket,
            queued.id.0,
            input.run_id,
            &base[..12]
        );
        if args.enqueue_only {
            return Ok(EXIT_OK);
        }
        let deadline = Instant::now() + Duration::from_secs(args.timeout);
        let mut outcome: Option<Value> = None;
        while Instant::now() < deadline {
            // One step runs a whole stage: bound it by the remaining time.
            // A step cut short leaves its task leased; the lease expires and
            // the queue hands it out again.
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(s) =
                tokio::time::timeout(remaining, step(&lane.worker, &lane.orchestrator)).await
            else {
                break;
            };
            let s = s.map_err(|e| CliError::env(format!("queue: {e}")))?;
            print_attempts(&s);
            match &s {
                LoopStep::Ran(ran, _) if ran.ticket == input.ticket => match &ran.disposition {
                    Disposition::Passed { pr } => {
                        outcome = Some(
                            json!({"ticket": input.ticket, "result": "pr", "pr": pr.url, "run_id": input.run_id}),
                        );
                        break;
                    }
                    Disposition::HumanLane(r) => {
                        outcome = Some(
                            json!({"ticket": input.ticket, "result": "human_lane", "reason": r, "run_id": input.run_id}),
                        );
                        break;
                    }
                    Disposition::Failed { error } => {
                        outcome = Some(
                            json!({"ticket": input.ticket, "result": "failed", "error": error, "run_id": input.run_id}),
                        );
                        break;
                    }
                    _ => {}
                },
                LoopStep::Idle(Idle::DailyCapReached) => {
                    outcome = Some(json!({"ticket": input.ticket, "result": "daily_cap_reached"}));
                    break;
                }
                LoopStep::Idle(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                _ => {}
            }
        }
        let outcome =
            outcome.unwrap_or_else(|| json!({"ticket": input.ticket, "result": "timeout"}));
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&outcome).unwrap_or_default()
            );
        }
        Ok(if outcome["result"] == "pr" {
            EXIT_OK
        } else {
            EXIT_FAILED
        })
    })
}

pub fn status(ctx: &Context, ticket: Option<&str>, json_out: bool) -> CliResult<u8> {
    let target = ctx.load_config()?;
    let repo = agent_lane::repo_slug(ctx, &target);
    let url = std::env::var(guard_store::DATABASE_URL_ENV)
        .ok()
        .filter(|u| u.starts_with("postgres"))
        .ok_or_else(|| CliError::env("guard agents status needs DATABASE_URL (Postgres)"))?;
    let value = block_on(async {
        let store = guard_store::PgStore::connect(&url)
            .await
            .map_err(|e| CliError::env(format!("database: {e}")))?;
        store
            .migrate()
            .await
            .map_err(|e| CliError::env(format!("database migrations: {e}")))?;
        let pool = store.pool();
        let tasks = sqlx::query(
            "SELECT id, ticket, role, state, attempts, last_error FROM task_queue
             WHERE repo = $1 AND ($2::text IS NULL OR ticket = $2) ORDER BY id DESC LIMIT 50",
        )
        .bind(&repo)
        .bind(ticket)
        .fetch_all(pool)
        .await
        .map_err(|e| CliError::env(format!("task_queue: {e}")))?;
        let runs = sqlx::query(
            "SELECT run_id, ticket, role, attempt, agent, runtime, key_alias, status, outcome,
                    diff_paths, cost_usd::float8 AS cost, result->>'detail' AS detail
             FROM agent_runs WHERE repo = $1 AND ($2::text IS NULL OR ticket = $2)
             ORDER BY id DESC LIMIT 50",
        )
        .bind(&repo)
        .bind(ticket)
        .fetch_all(pool)
        .await
        .map_err(|e| CliError::env(format!("agent_runs: {e}")))?;
        let tasks: Vec<Value> = tasks
            .iter()
            .map(|r| {
                json!({
                    "id": r.get::<i64, _>("id"), "ticket": r.get::<String, _>("ticket"),
                    "role": r.get::<String, _>("role"), "state": r.get::<String, _>("state"),
                    "attempts": r.get::<i32, _>("attempts"),
                    "last_error": r.get::<Option<String>, _>("last_error"),
                })
            })
            .collect();
        let runs: Vec<Value> = runs
            .iter()
            .map(|r| {
                json!({
                    "run_id": r.get::<String, _>("run_id"), "ticket": r.get::<String, _>("ticket"),
                    "role": r.get::<String, _>("role"), "attempt": r.get::<i32, _>("attempt"),
                    "agent": r.get::<String, _>("agent"), "runtime": r.get::<String, _>("runtime"),
                    "key_alias": r.get::<Option<String>, _>("key_alias"),
                    "status": r.get::<String, _>("status"),
                    "outcome": r.get::<Option<String>, _>("outcome"),
                    "diff_paths": r.get::<Vec<String>, _>("diff_paths"),
                    "cost_usd": r.get::<Option<f64>, _>("cost"),
                    "detail": r.get::<Option<String>, _>("detail"),
                })
            })
            .collect();
        Ok::<_, CliError>(json!({"repo": repo, "tasks": tasks, "runs": runs}))
    })?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return Ok(EXIT_OK);
    }
    println!("agents: {}", value["repo"].as_str().unwrap_or_default());
    for t in value["tasks"].as_array().into_iter().flatten() {
        println!(
            "task {} {} {} {} (attempts {}){}",
            t["id"],
            t["ticket"].as_str().unwrap_or(""),
            t["role"].as_str().unwrap_or(""),
            t["state"].as_str().unwrap_or(""),
            t["attempts"],
            t["last_error"]
                .as_str()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        );
    }
    for r in value["runs"].as_array().into_iter().flatten() {
        println!(
            "run {} {} {} #{} {} {}: {} / {}",
            r["run_id"].as_str().unwrap_or(""),
            r["ticket"].as_str().unwrap_or(""),
            r["role"].as_str().unwrap_or(""),
            r["attempt"],
            r["agent"].as_str().unwrap_or(""),
            r["runtime"].as_str().unwrap_or(""),
            r["status"].as_str().unwrap_or(""),
            r["outcome"].as_str().unwrap_or("-")
        );
    }
    Ok(EXIT_OK)
}

/// `guard agents ab-report`: the pre-registered 4-arm report (tokens.md).
pub fn ab_report(experiment: &str, out: Option<&std::path::Path>) -> CliResult<u8> {
    let url = std::env::var(guard_store::DATABASE_URL_ENV)
        .ok()
        .filter(|u| u.starts_with("postgres"))
        .ok_or_else(|| CliError::env("guard agents ab-report needs DATABASE_URL (Postgres)"))?;
    let markdown = block_on(async {
        let store = guard_store::PgStore::connect(&url)
            .await
            .map_err(|e| CliError::env(format!("database: {e}")))?;
        store
            .migrate()
            .await
            .map_err(|e| CliError::env(format!("database migrations: {e}")))?;
        let inputs = guard_store::spend_report::load_ab_inputs(store.pool(), experiment)
            .await
            .map_err(|e| CliError::env(format!("A/B inputs: {e}")))?;
        let report = guard_store::spend_report::compute_ab_report(
            &inputs,
            &guard_store::spend_report::Preregistration::default(),
        );
        Ok::<_, CliError>(guard_store::spend_report::render_markdown(&report))
    })?;
    print!("{markdown}");
    if let Some(path) = out {
        std::fs::write(path, &markdown)
            .map_err(|e| CliError::env(format!("{}: {e}", path.display())))?;
    }
    Ok(EXIT_OK)
}
