//! The agent lane (architecture sections 4 and 16) wired from the
//! environment: `agents.toml`, the Postgres queue / runs / spend / memory,
//! the Docker sandbox, the runtime adapters, the LiteLLM gateway, and the
//! host seams ([`host`], [`checks`]). Used by `guard agents run|status` and
//! by `guard serve` (lane-A tickets in active mode only).
//!
//! Memory: the orchestrator ([`guard_agents::lane::Orchestrator`]) holds the
//! only `MemoryFactsWriter`; the pipeline gets a reader
//! (`GUARD_MEMORY_READ_DATABASE_URL`, else the writer's `read_only` view).
//!
//! Environment (names only; see docs/contracts/agents-toml.md):
//! - `DATABASE_URL` (required, Postgres), `GUARD_MEMORY_READ_DATABASE_URL`.
//! - `GUARD_AGENTS_TOML` (default `<repo>/agents.toml`).
//! - `GUARD_GATEWAY_ADMIN_URL`: where the orchestrator reaches the gateway
//!   admin API when it differs from `[gateway].url` (the in-sandbox URL).
//! - `GUARD_SANDBOX_EGRESS_NETWORK` (default `bridge`),
//!   `GUARD_SANDBOX_RUNTIME` (`auto` | `runc` | `runsc`).
//! - `GUARD_AGENT_FAKE_PLANS`: fake runtime plans dir (with `--fake`).
//! - `GUARD_UNSAFE_HOST_CHECKS=1`: run red-first and the gate on the host
//!   instead of a check sandbox (development only; warned loudly).
//! - `GUARD_REPO` (else `GITHUB_REPOSITORY`, `GITLAB_PROJECT`, the origin
//!   URL, `local/<app name>`).
//! - Agent PRs (docs/contracts/pr-publisher.md): with `target.toml`
//!   `[code_host]`, the token (`GITHUB_TOKEN` / `GITLAB_TOKEN`, or the var
//!   named by `GUARD_PR_TOKEN_ENV`), the repository and the optional
//!   `GUARD_PR_PUSH_URL`, `GUARD_PR_BASE_BRANCH`, `GUARD_PR_LABELS`, the
//!   branch is pushed and a draft PR / MR opened; else (or with `--fake`)
//!   it stays a local branch.

pub mod checks;
pub mod host;

use crate::context::Context;
use crate::error::{CliError, CliResult};
use guard_agents::config::{AgentsConfig, LoadOptions, RuntimeKind};
use guard_agents::lane::Orchestrator;
use guard_agents::pg_queue::{PgSpendLedger, PgTaskQueue};
use guard_agents::pipeline::seams::TicketInput;
use guard_agents::pipeline::{PipelineDeps, SecretAliases, TicketPipeline};
use guard_agents::reaper::{PgRunActivity, RunActivity};
use guard_agents::runs::PgRunLog;
use guard_agents::scheduler::Slots;
use guard_agents::worker::{Worker, enqueue_ticket};
use guard_core::config::TargetConfig;
use guard_core::ports::{
    AgentRuntime, DecisionId, EvidenceStore, ImpactAnalyzer, Issue, MemoryFactsReader,
    MemoryFactsWriter, ModelGateway, SandboxProvider, TaskQueue, read_only,
};
use guard_sandbox::docker::cli::ProcessDockerCli;
use guard_sandbox::{DockerConfig, DockerSandbox, RuntimePreference};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub const AGENTS_TOML_ENV: &str = "GUARD_AGENTS_TOML";
pub const GATEWAY_ADMIN_URL_ENV: &str = "GUARD_GATEWAY_ADMIN_URL";
pub const EGRESS_NETWORK_ENV: &str = "GUARD_SANDBOX_EGRESS_NETWORK";
pub const SANDBOX_RUNTIME_ENV: &str = "GUARD_SANDBOX_RUNTIME";
pub const FAKE_PLANS_ENV: &str = "GUARD_AGENT_FAKE_PLANS";
pub const REPO_ENV: &str = "GUARD_REPO";
pub const WORK_DIR_ENV: &str = "GUARD_AGENT_WORK_DIR";
/// `1`: keep each attempt dir (clone, `/task`) after the attempt, for inspection.
pub const KEEP_WORK_ENV: &str = "GUARD_AGENT_KEEP_WORK";

/// Where per-attempt sandbox clones, `/task` dirs and mount skeletons live:
/// `GUARD_AGENT_WORK_DIR`, else `<temp>/guard-agents/<repo>`. Deliberately
/// outside the checkout: Docker Desktop answers EIO on a bind mount whose
/// host ancestors hold a directory junction (pnpm's `node_modules`), and
/// the dir must be shareable with the daemon (under the user profile).
pub fn sandbox_work_root(ctx: &Context, config: &TargetConfig) -> PathBuf {
    env(WORK_DIR_ENV).map(PathBuf::from).unwrap_or_else(|| {
        let slug: String = repo_slug(ctx, config)
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        std::env::temp_dir().join("guard-agents").join(slug)
    })
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

pub fn agents_toml_path(ctx: &Context) -> PathBuf {
    env(AGENTS_TOML_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| ctx.root.join("agents.toml"))
}

/// Loads `agents.toml`; `fake` (the `--fake` flag) is the explicit opt-in
/// that `GUARD_ALLOW_FAKE_RUNTIME=1` gives, and `runtime` overrides every
/// agent's runtime (validated again afterwards).
pub fn load_agents(
    path: &Path,
    fake: bool,
    runtime: Option<RuntimeKind>,
) -> CliResult<AgentsConfig> {
    let mut options = LoadOptions::from_env();
    options.allow_fake_runtime |= fake;
    let mut config = AgentsConfig::load(path, &options)
        .map_err(|e| CliError::usage(format!("{}: {e}", path.display())))?;
    let runtime = runtime.or(fake.then_some(RuntimeKind::Fake));
    if let Some(rt) = runtime {
        for agent in config.agents.values_mut() {
            agent.runtime = rt;
        }
        config.validate(&options).map_err(|e| {
            CliError::usage(format!(
                "{} with runtime {}: {e}",
                path.display(),
                rt.as_str()
            ))
        })?;
    }
    Ok(config)
}

/// `owner/name` of the repo the lane works on.
pub fn repo_slug(ctx: &Context, config: &TargetConfig) -> String {
    for name in [REPO_ENV, "GITHUB_REPOSITORY", "GITLAB_PROJECT"] {
        if let Some(v) = env(name) {
            return v;
        }
    }
    crate::git::origin_url(&ctx.root)
        .and_then(|u| slug_from_url(&u))
        .unwrap_or_else(|| format!("local/{}", config.app.name))
}

/// `git@host:owner/name.git`, `https://host/owner/name(.git)` → `owner/name`.
pub fn slug_from_url(url: &str) -> Option<String> {
    let path = url
        .trim()
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit_once(['/', ':'])
        .map(|(head, name)| (head.rsplit(['/', ':']).next().unwrap_or(""), name))?;
    let (owner, name) = path;
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

/// `a<unix>-<ticket>` (runner-env.md RUN_ID charset).
pub fn new_run_id(ticket: &str) -> String {
    let t: String = ticket
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut id = format!("a{}-{t}", crate::commands::unix_now());
    id.truncate(64);
    id
}

/// Acceptance criteria: the description's list items, else the whole text.
pub fn acceptance_criteria(description: &str) -> Vec<String> {
    let items: Vec<String> = description
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    if items.is_empty() && !description.trim().is_empty() {
        vec![description.trim().to_string()]
    } else {
        items
    }
}

/// What a lane-A ticket becomes on the queue.
pub struct TicketSource<'a> {
    pub config: &'a TargetConfig,
    pub repo: &'a str,
    /// `<e2e dir>/tests`, repo-relative.
    pub tests_dir: &'a str,
    pub base_commit: &'a str,
    pub impact: Option<&'a dyn ImpactAnalyzer>,
}

/// The ticket input of `issue`. The Coder may write only the paths of the
/// ticket's `area:<key>` labels; a ticket without a known area is refused.
pub fn ticket_input(
    src: &TicketSource<'_>,
    issue: &Issue,
    labels: &[String],
) -> Result<TicketInput, String> {
    let mut app_paths: Vec<String> = Vec::new();
    let mut areas = Vec::new();
    for label in labels.iter().chain(&issue.labels) {
        if let Some(key) = label.strip_prefix("area:") {
            if let Some(area) = src.config.areas.get(key) {
                if !areas.contains(&key) {
                    areas.push(key);
                    app_paths.extend(area.paths.iter().cloned());
                }
            }
        }
    }
    if app_paths.is_empty() {
        return Err(format!(
            "{}: no `area:<key>` label of target.toml [areas]; the Coder would have no writable paths",
            issue.key
        ));
    }
    let predicted_files = src
        .impact
        .map(|i| i.resolve_mentions(&guard_core::triage::mentioned_modules(issue)))
        .unwrap_or_default();
    let mut all_labels: Vec<String> = issue.labels.iter().chain(labels).cloned().collect();
    all_labels.sort();
    all_labels.dedup();
    Ok(TicketInput {
        repo: src.repo.to_string(),
        ticket: issue.key.clone(),
        run_id: new_run_id(&issue.key),
        base_commit: src.base_commit.to_string(),
        summary: issue.summary.clone(),
        acceptance_criteria: acceptance_criteria(&issue.description),
        app_paths,
        tests_dir: src.tests_dir.to_string(),
        labels: all_labels,
        predicted_files,
        preview: None,
        package_mirror: None,
        experiment: None,
        areas: areas.iter().map(|a| a.to_string()).collect(),
        journeys: vec![],
        claim: 0,
    })
}

/// `s3://<bucket>` of an S3 evidence URL (`s3://bucket`,
/// `s3+http://key:secret@host/bucket`); `None` for any other store.
pub fn evidence_base(url: &str) -> Option<String> {
    url.split_once("://")
        .and_then(|(_, rest)| rest.trim_end_matches('/').rsplit('/').next())
        .filter(|b| !b.is_empty() && url.starts_with("s3"))
        .map(|bucket| format!("s3://{bucket}"))
}

/// How old a gateway key must be before the key reaper may revoke it: the
/// longest agent `timeout_secs` plus `KEY_GRACE_SECS`, never below the
/// reaper's default.
pub fn key_reaper_min_age(config: &AgentsConfig) -> std::time::Duration {
    let longest = config
        .agents
        .values()
        .map(|a| a.timeout_secs)
        .max()
        .unwrap_or(0);
    std::time::Duration::from_secs(longest + guard_agents::config::KEY_GRACE_SECS)
        .max(guard_agents::reaper::DEFAULT_MIN_AGE)
}

/// The evidence store of `[evidence].storage_url_env`, else `.guard/evidence`,
/// and the `s3://<bucket>` / `file://` base of its URIs.
fn evidence(
    ctx: &Context,
    config: &TargetConfig,
) -> CliResult<(Arc<dyn EvidenceStore>, Option<String>)> {
    match env(&config.evidence.storage_url_env) {
        Some(url) => {
            let store = guard_store::evidence_store_from_url(&url)
                .map_err(|e| CliError::env(format!("evidence: {e}")))?;
            Ok((store, evidence_base(&url)))
        }
        None => {
            let root = ctx.guard_dir().join("evidence");
            Ok((
                Arc::new(guard_store::FsEvidenceStore::new(&root)),
                Some(format!("file://{}", host::slash(&root))),
            ))
        }
    }
}

/// Where red-first and the gate run: a check sandbox of `docker` from the
/// agent sandbox image, unless `GUARD_UNSAFE_HOST_CHECKS=1` (dev only;
/// warned loudly) puts them back on the host.
fn check_isolation(config: &AgentsConfig, docker: &Arc<DockerSandbox>) -> checks::CheckIsolation {
    if checks::unsafe_host_checks_requested(
        std::env::var(checks::UNSAFE_HOST_CHECKS_ENV)
            .ok()
            .as_deref(),
    ) {
        eprintln!("{}", checks::UNSAFE_HOST_CHECKS_WARNING);
        return checks::CheckIsolation::UnsafeHost;
    }
    let image = config.scheduler.image.clone().unwrap_or_else(|| {
        config
            .image_for(config.role_agent(guard_core::ports::Role::Qa).1)
            .to_string()
    });
    checks::CheckIsolation::Sandbox {
        provider: docker.clone(),
        image,
    }
}

/// Everything a worker loop needs.
pub struct Lane {
    pub queue: Arc<PgTaskQueue>,
    /// The Docker provider, for the periodic reaper of expired sandboxes.
    pub sandboxes: Arc<DockerSandbox>,
    pub worker: Worker,
    pub orchestrator: Orchestrator,
    pub repo: String,
    pub tests_dir: String,
    /// The LiteLLM gateway, for the key reaper.
    pub gateway: Arc<dyn ModelGateway>,
    /// Lane run activity (task-queue leases), for the key reaper.
    pub runs: Arc<dyn RunActivity>,
    /// [`key_reaper_min_age`] of `agents.toml`.
    pub key_min_age: std::time::Duration,
    /// Where agent PRs go ([`host::pr_publisher`]).
    pub publisher: host::PublisherKind,
}

pub struct LaneOptions {
    pub fake: bool,
    pub runtime: Option<RuntimeKind>,
}

/// Connects Postgres (and migrates), builds the adapters and the worker.
pub async fn build(ctx: &Context, target: &TargetConfig, opts: &LaneOptions) -> CliResult<Lane> {
    let config = Arc::new(load_agents(
        &agents_toml_path(ctx),
        opts.fake,
        opts.runtime,
    )?);
    let url = env(guard_store::DATABASE_URL_ENV)
        .filter(|u| u.starts_with("postgres"))
        .ok_or_else(|| CliError::env("the agent lane needs DATABASE_URL (Postgres)"))?;
    let store = guard_store::PgStore::connect(&url)
        .await
        .map_err(|e| CliError::env(format!("database: {e}")))?;
    store
        .migrate()
        .await
        .map_err(|e| CliError::env(format!("database migrations: {e}")))?;
    let pool = store.pool().clone();
    let queue = Arc::new(PgTaskQueue::new(pool.clone()));
    let writer: Arc<dyn MemoryFactsWriter> =
        Arc::new(guard_store::PgMemoryFacts::new(pool.clone()));
    let reader: Arc<dyn MemoryFactsReader> = match env(guard_store::MEMORY_READ_DATABASE_URL_ENV) {
        Some(_) => Arc::new(
            guard_store::PgMemoryFactsReader::from_env()
                .await
                .map_err(|e| CliError::env(format!("memory reader: {e}")))?,
        ),
        None => read_only(writer.clone()),
    };

    let work_root = sandbox_work_root(ctx, target);
    let runtime = match env(SANDBOX_RUNTIME_ENV).as_deref() {
        Some("runc") | Some("default") => RuntimePreference::Default,
        Some("runsc") => RuntimePreference::Runsc,
        _ => RuntimePreference::Auto,
    };
    let docker = DockerConfig {
        egress_network: env(EGRESS_NETWORK_ENV).unwrap_or_else(|| "bridge".into()),
        scratch_root: work_root.join("scratch"),
        runtime,
        ..DockerConfig::default()
    };
    let docker_sandbox = Arc::new(DockerSandbox::new(
        Arc::new(ProcessDockerCli::new("docker")),
        docker,
    ));
    // Sandboxes left behind by a crashed or killed orchestrator.
    if let Err(e) = docker_sandbox.reap_expired().await {
        eprintln!("guard: warning: agents: sandbox reaper: {e}");
    }
    let sandbox: Arc<dyn SandboxProvider> = docker_sandbox.clone();
    let mut runtimes: Vec<Arc<dyn AgentRuntime>> = Vec::new();
    for rt in config.runtimes_in_use() {
        let adapter = guard_runtimes::runtime_for(rt.as_str(), sandbox.clone())
            .ok_or_else(|| CliError::usage(format!("no adapter for runtime {}", rt.as_str())))?;
        runtimes.push(adapter);
    }
    let admin = env(GATEWAY_ADMIN_URL_ENV).unwrap_or_else(|| config.gateway.url.clone());
    let gateway = Arc::new(
        guard_runtimes::gateway::LiteLlmGateway::from_env(&admin, &config.gateway.master_key_env)
            .map_err(|e| CliError::env(format!("gateway: {e}")))?,
    );
    let (evidence, evidence_base) = evidence(ctx, target)?;
    let e2e_rel = ctx.display(&ctx.e2e_dir);
    let tests_dir = format!("{e2e_rel}/tests");
    let token = std::env::var(&target.testability.token_env)
        .ok()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| format!("guard-agent-{}", std::process::id()));
    let slots = Arc::new(Slots::new(
        &config.scheduler,
        Arc::new(PgSpendLedger::new(pool.clone())),
    ));
    let aliases: SecretAliases = Arc::new(guard_runtimes::secret_env_aliases);
    let (publisher, publisher_kind) = host::pr_publisher(
        opts.fake,
        target,
        host::LocalBranchPublisher {
            repo_root: ctx.root.clone(),
            work_root: ctx.guard_dir().join("agents").join("work"),
        },
        |name| std::env::var(name).ok(),
    );
    let pipeline = TicketPipeline::new(PipelineDeps {
        config: config.clone(),
        slots: slots.clone(),
        runtimes,
        sandbox,
        gateway: gateway.clone(),
        evidence,
        layouts: Arc::new(host::HostLayout {
            repo_root: ctx.root.clone(),
            work_root: work_root.clone(),
            rendered_root: ctx.guard_dir().join("agents").join("rendered"),
            fake_plans: if opts.fake {
                env(FAKE_PLANS_ENV).map(PathBuf::from)
            } else {
                None
            },
            keep_work: env(KEEP_WORK_ENV).as_deref() == Some("1"),
        }),
        checks: Arc::new(checks::HostChecks::new(
            &ctx.root,
            &e2e_rel,
            target.clone(),
            token,
            check_isolation(&config, &docker_sandbox),
        )),
        publisher,
        facts: Some(reader),
    })
    .map_err(|e| CliError::usage(e.to_string()))?
    .with_secret_aliases(aliases)
    .with_spend_sink(Arc::new(guard_store::spend::PgSpendStore::new(
        pool.clone(),
    )));
    let host = env("COMPUTERNAME")
        .or_else(|| env("HOSTNAME"))
        .unwrap_or_else(|| "host".into());
    let worker = Worker::new(
        format!("{host}:{}", std::process::id()),
        queue.clone(),
        slots,
        Arc::new(pipeline),
        config.scheduler.lease(),
    );
    let key_min_age = key_reaper_min_age(&config);
    let orchestrator = Orchestrator {
        runs: Arc::new(PgRunLog::new(pool.clone())),
        facts: Some(writer),
        gateway: gateway.clone(),
        spend: Some(Arc::new(guard_store::spend::PgSpendStore::new(
            pool.clone(),
        ))),
        evidence_base,
    };
    Ok(Lane {
        queue,
        worker,
        orchestrator,
        sandboxes: docker_sandbox,
        repo: repo_slug(ctx, target),
        tests_dir,
        gateway,
        runs: Arc::new(PgRunActivity::new(pool)),
        key_min_age,
        publisher: publisher_kind,
    })
}

/// `guard serve`'s hand-off of audited lane-A tickets: `(ticket, qa)` on the queue.
pub struct QueueLane {
    pub queue: Arc<dyn TaskQueue>,
    pub config: TargetConfig,
    pub repo: String,
    pub tests_dir: String,
    pub root: PathBuf,
    pub impact: Arc<dyn ImpactAnalyzer>,
}

#[async_trait::async_trait]
impl guard_server::AgentLane for QueueLane {
    async fn enqueue(
        &self,
        issue: &Issue,
        decision: DecisionId,
        labels: &[String],
    ) -> Result<(), String> {
        let base = crate::git::head_sha(&self.root).map_err(|e| format!("base commit: {e}"))?;
        let input = ticket_input(
            &TicketSource {
                config: &self.config,
                repo: &self.repo,
                tests_dir: &self.tests_dir,
                base_commit: &base,
                impact: Some(self.impact.as_ref()),
            },
            issue,
            labels,
        )?;
        let queued = enqueue_ticket(self.queue.as_ref(), &input, 0)
            .await
            .map_err(|e| e.to_string())?;
        eprintln!(
            "agents: {} (decision {}) queued as task {} ({}), run {}",
            issue.key,
            decision.0,
            queued.id.0,
            if queued.created {
                "new"
            } else {
                "already queued"
            },
            input.run_id
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TargetConfig {
        guard_core::config::parse_target(
            r#"version = 1
[app]
name = "shop"
languages = ["javascript"]
base_url = "http://localhost:3000"
[areas.catalog]
description = "c"
paths = ["src/products/**"]
[areas.checkout]
description = "k"
paths = ["src/checkout/**", "src/cart/**"]
"#,
        )
        .unwrap()
    }

    fn issue(description: &str, labels: &[&str]) -> Issue {
        Issue {
            key: "SHOP-42".into(),
            url: "https://jira.test/browse/SHOP-42".into(),
            summary: "Low stock warning".into(),
            description: description.into(),
            issue_type: "Story".into(),
            status: "To Do".into(),
            status_category: guard_core::ports::StatusCategory::ToDo,
            labels: labels.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn slugs_come_from_common_remote_urls() {
        assert_eq!(
            slug_from_url("git@github.com:acme/shop.git").unwrap(),
            "acme/shop"
        );
        assert_eq!(
            slug_from_url("https://github.com/acme/shop").unwrap(),
            "acme/shop"
        );
        assert_eq!(
            slug_from_url("https://gitlab.com/acme/shop.git/").unwrap(),
            "acme/shop"
        );
        assert_eq!(slug_from_url("shop"), None);
    }

    #[test]
    fn criteria_are_list_items_else_the_description() {
        assert_eq!(
            acceptance_criteria("Intro\n- Shows Only 2 left\n* Hides it at 0\n"),
            vec!["Shows Only 2 left", "Hides it at 0"]
        );
        assert_eq!(acceptance_criteria("  Just text  "), vec!["Just text"]);
        assert!(acceptance_criteria("").is_empty());
    }

    #[test]
    fn the_ticket_input_takes_paths_from_the_area_labels() {
        let config = config();
        let src = TicketSource {
            config: &config,
            repo: "acme/shop",
            tests_dir: "e2e-playwright/tests",
            base_commit: "abc1234",
            impact: None,
        };
        let input = ticket_input(
            &src,
            &issue("- a", &[]),
            &["area:catalog".into(), "lane:agent".into()],
        )
        .unwrap();
        assert_eq!(input.app_paths, vec!["src/products/**"]);
        assert_eq!(input.tests_dir, "e2e-playwright/tests");
        assert_eq!(input.labels, vec!["area:catalog", "lane:agent"]);
        assert!(input.run_id.starts_with('a') && input.run_id.ends_with("-shop-42"));
        assert!(guard_core::runner_env::is_valid_run_id(&input.run_id));
        let err = ticket_input(&src, &issue("", &[]), &["lane:agent".into()]).unwrap_err();
        assert!(err.contains("no `area:<key>` label"), "{err}");
        let both = ticket_input(
            &src,
            &issue("", &["area:checkout"]),
            &["area:catalog".into()],
        )
        .unwrap();
        assert_eq!(both.app_paths.len(), 3);
    }

    #[test]
    fn evidence_bases_are_s3_buckets_only() {
        assert_eq!(
            evidence_base("s3+http://u:p@localhost:58333/guard-evidence").as_deref(),
            Some("s3://guard-evidence")
        );
        assert_eq!(evidence_base("s3://bucket").as_deref(), Some("s3://bucket"));
        assert_eq!(evidence_base("file:///tmp/e"), None);
    }

    #[test]
    fn keys_are_reaped_only_after_the_longest_agent_timeout_plus_grace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        std::fs::write(
            &path,
            include_str!("../../../guard-agents/tests/fixtures/agents.toml"),
        )
        .unwrap();
        let config = load_agents(&path, true, None).unwrap();
        let longest = config
            .agents
            .values()
            .map(|a| a.timeout_secs)
            .max()
            .unwrap();
        let age = key_reaper_min_age(&config);
        assert_eq!(
            age.as_secs(),
            (longest + guard_agents::config::KEY_GRACE_SECS)
                .max(guard_agents::reaper::DEFAULT_MIN_AGE.as_secs())
        );
    }

    #[test]
    fn fake_runtime_needs_the_explicit_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents.toml");
        std::fs::write(
            &path,
            include_str!("../../../guard-agents/tests/fixtures/agents.toml"),
        )
        .unwrap();
        let config = load_agents(&path, true, None).unwrap();
        assert!(
            config
                .agents
                .values()
                .all(|a| a.runtime == RuntimeKind::Fake)
        );
        let config = load_agents(&path, false, None).unwrap();
        assert!(
            config
                .agents
                .values()
                .all(|a| a.runtime != RuntimeKind::Fake)
        );
    }
}
