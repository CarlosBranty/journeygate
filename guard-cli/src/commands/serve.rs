//! `guard serve`: builds every adapter from the environment and runs
//! `guard_server::serve` (server-api.md, "Running").

use crate::cli::ServeArgs;
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK, warning};
use crate::fake::{FakeEngine, FakeJira};
use crate::runtime::block_on;
use crate::settings::load_thresholds;
use guard_core::config::TargetConfig;
use guard_core::ports::{
    CodeHost, DecisionEngine, DecisionLog, DiffSummary, EngineError, EvidenceFile, EvidenceStore,
    GateHistory, GateReport, IssueTracker, JourneyQuestion, MemoryFactsWriter, ReportOutcome,
    TriageAnswers, TriageRequest,
};
use guard_server::jobs::PeriodicJob;
use guard_server::memory::MemoryWriters;
use guard_server::staleness::{FanOutSource, GitRepo, StalenessJob};
use guard_server::{AppState, GateReporter, ServeOptions};
use guard_tracker::report::{ReportOptions, ScreenshotSource, report_gate};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:8080";
/// Fake mode switches (server-api.md, "Fake mode").
pub const JEV_FAKE_ENV: &str = "GUARD_JEV_FAKE";
pub const JIRA_FAKE_ENV: &str = "GUARD_JIRA_FAKE";
/// `DATABASE_URL` value selecting the in-memory decision log and gate history.
pub const MEMORY_DATABASE_URL: &str = "memory://";

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// `1`, `true` or `yes` (case-insensitive).
pub fn is_on(value: Option<&str>) -> bool {
    value.is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

/// `--listen`, else `GUARD_LISTEN`, else `0.0.0.0:8080`.
pub fn listen_addr(flag: Option<&str>, env_value: Option<String>) -> CliResult<SocketAddr> {
    let raw = flag
        .map(str::to_string)
        .or(env_value)
        .unwrap_or_else(|| DEFAULT_LISTEN.to_string());
    raw.parse()
        .map_err(|_| CliError::usage(format!("invalid listen address {raw:?}")))
}

/// The engine when no Jev key is configured: every call fails, so triage
/// goes to the human lane and `/api/impact` answers 503 (fail closed).
struct UnavailableEngine(String);

#[async_trait::async_trait]
impl DecisionEngine for UnavailableEngine {
    fn model(&self) -> &str {
        guard_jev::MODEL
    }

    async fn triage(&self, _: &TriageRequest) -> Result<TriageAnswers, EngineError> {
        Err(EngineError::Unconfigured(self.0.clone()))
    }

    async fn impact(
        &self,
        _: &DiffSummary,
        journeys: &[JourneyQuestion],
    ) -> Result<BTreeMap<String, f64>, EngineError> {
        if journeys.is_empty() {
            return Ok(BTreeMap::new());
        }
        Err(EngineError::Unconfigured(self.0.clone()))
    }
}

/// Screenshots for Jira attachments, read back from the evidence store.
struct EvidenceScreenshots(Option<Arc<dyn EvidenceStore>>);

#[async_trait::async_trait]
impl ScreenshotSource for EvidenceScreenshots {
    async fn read(&self, file: &EvidenceFile) -> Option<Vec<u8>> {
        let store = self.0.as_ref()?;
        store.get_object(file.key.as_deref()?).await.ok()
    }
}

/// `guard_tracker::report::report_gate` behind the server's reporter port.
pub struct TrackerReporter {
    tracker: Arc<dyn IssueTracker>,
    screenshots: EvidenceScreenshots,
    options: ReportOptions,
}

#[async_trait::async_trait]
impl GateReporter for TrackerReporter {
    async fn report(&self, report: &GateReport) -> ReportOutcome {
        report_gate(
            self.tracker.as_ref(),
            &self.screenshots,
            report,
            &self.options,
        )
        .await
    }
}

/// `ReportOptions` for `[tracker].project` with `JIRA_PRE_EXISTING_PRIORITY`.
pub fn report_options(config: &TargetConfig) -> Option<ReportOptions> {
    let project = config.tracker.as_ref()?.project.clone()?;
    let mut options = ReportOptions::new(project);
    if let Some(priority) = env("JIRA_PRE_EXISTING_PRIORITY") {
        options.pre_existing_priority = priority;
    }
    Some(options)
}

/// Loads guard's Jira account id, retrying with exponential backoff (from
/// `initial`, capped at 5 minutes) until `GET /myself` answers: until then
/// every human label change is ignored. Returns the number of attempts.
pub async fn load_account_id_with_retry(
    jira: Arc<guard_tracker::JiraTracker>,
    initial: std::time::Duration,
) -> u32 {
    let max = std::time::Duration::from_secs(300);
    let mut delay = initial;
    let mut attempts = 0;
    loop {
        attempts += 1;
        match jira.load_self_account_id().await {
            Ok(_) => {
                if attempts > 1 {
                    eprintln!("jira: GET /rest/api/3/myself answered; label changes are recorded");
                }
                return attempts;
            }
            Err(e) => {
                if attempts == 1 {
                    warning(format!(
                        "jira: cannot read GET /rest/api/3/myself ({e}); label changes are ignored until it answers (retrying)"
                    ));
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(max);
            }
        }
    }
}

/// The startup line about dashboard login.
pub fn dashboard_line(has_auth: bool) -> String {
    if has_auth {
        "dashboard: token login".into()
    } else {
        "dashboard: OPEN to anyone who can reach the listener (set GUARD_DASHBOARD_TOKEN to require a login)".into()
    }
}

/// What `guard serve` wired, printed at startup.
struct Wiring {
    engine: &'static str,
    tracker: &'static str,
    store: &'static str,
    code_host: &'static str,
    evidence: &'static str,
    agents: &'static str,
    memory: &'static str,
}

/// `GUARD_STALENESS_INTERVAL` (`900`, `900s`, `15m`, `2h`), else 15 minutes;
/// an unreadable value warns and keeps the default.
pub fn staleness_interval(raw: Option<&str>) -> std::time::Duration {
    match raw {
        None => guard_server::staleness::DEFAULT_STALENESS_INTERVAL,
        Some(raw) => guard_server::staleness::parse_interval(raw).unwrap_or_else(|e| {
            warning(format!(
                "{}: {e}; using the default",
                guard_server::staleness::STALENESS_INTERVAL_ENV
            ));
            guard_server::staleness::DEFAULT_STALENESS_INTERVAL
        }),
    }
}

/// `job` with its runs on stderr: failures always, successes when their
/// summary differs from the previous run's (so an idle reaper stays quiet).
pub fn logged(job: PeriodicJob) -> PeriodicJob {
    let name = job.name().to_string();
    let interval = job.interval();
    let last: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    PeriodicJob::new(name.clone(), interval, move || {
        let (job, name, last) = (job.clone(), name.clone(), last.clone());
        async move {
            let result = job.run_once().await;
            match &result {
                Ok(summary) => {
                    let mut last = last.lock().unwrap_or_else(|p| p.into_inner());
                    if last.as_deref() != Some(summary.as_str()) {
                        eprintln!("jobs: {name}: {summary}");
                        *last = Some(summary.clone());
                    }
                }
                Err(e) => warning(format!("jobs: {name} failed: {e}")),
            }
            result
        }
    })
}

/// Memory writers and the staleness job over the Postgres store
/// (memory-facts.md): gate results and label overrides become facts, and
/// facts whose files changed on the default branch become `needs_review`.
fn memory_wiring(
    ctx: &Context,
    config: &TargetConfig,
    store: &Arc<guard_store::PgStore>,
    evidence_url: Option<&str>,
) -> (Arc<MemoryWriters>, PeriodicJob) {
    let facts: Arc<dyn MemoryFactsWriter> =
        Arc::new(guard_store::PgMemoryFacts::new(store.pool().clone()));
    let repo = crate::agent_lane::repo_slug(ctx, config);
    let git = Arc::new(GitRepo::new(
        &ctx.root,
        crate::git::default_branch(&ctx.root),
    ));
    let mut writers = MemoryWriters::new(facts.clone(), repo.clone())
        .with_flaky(store.clone())
        .with_head(git.clone());
    if let Some(base) = evidence_url.and_then(crate::agent_lane::evidence_base) {
        writers = writers.with_evidence_base(base);
    }
    let job = StalenessJob::new(facts, repo, git)
        .with_fan_out(FanOutSource::rebuild(&ctx.root, config.clone()))
        .with_gates(store.clone() as Arc<dyn GateHistory>)
        .periodic(staleness_interval(
            env(guard_server::staleness::STALENESS_INTERVAL_ENV).as_deref(),
        ));
    (Arc::new(writers), job)
}

fn unix_now() -> i64 {
    i64::try_from(crate::commands::unix_now()).unwrap_or(i64::MAX)
}

/// The agent lane's reapers: expired sandboxes, gateway keys of finished or
/// unknown runs, and empty sandbox networks.
fn reaper_jobs(lane: &crate::agent_lane::Lane) -> Vec<PeriodicJob> {
    use guard_agents::reaper::{CliDockerNetworks, KeyReaper, NetworkReaper};
    let every = std::time::Duration::from_secs(300);
    let sandboxes = lane.sandboxes.clone();
    let keys = Arc::new(
        KeyReaper::new(lane.gateway.clone(), lane.runs.clone()).with_min_age(lane.key_min_age),
    );
    let networks = Arc::new(NetworkReaper::new(Arc::new(CliDockerNetworks::default())));
    vec![
        PeriodicJob::new("sandbox reaper", every, move || {
            let sandboxes = sandboxes.clone();
            async move {
                sandboxes
                    .reap_expired()
                    .await
                    .map(|n| format!("{} expired sandbox(es) removed {n:?}", n.len()))
                    .map_err(|e| e.to_string())
            }
        }),
        PeriodicJob::new("key reaper", every, move || {
            let keys = keys.clone();
            async move {
                let r = keys.reap(unix_now()).await.map_err(|e| e.to_string())?;
                Ok(format!(
                    "{} key(s) listed, {} revoked {:?}, {} kept, {} error(s) {:?}",
                    r.listed,
                    r.revoked.len(),
                    r.revoked,
                    r.kept,
                    r.errors.len(),
                    r.errors
                ))
            }
        }),
        PeriodicJob::new("network reaper", every, move || {
            let networks = networks.clone();
            async move {
                let r = networks.reap(unix_now()).await?;
                Ok(format!(
                    "{} network(s) listed, {} removed {:?}, {} kept, {} error(s) {:?}",
                    r.listed,
                    r.removed.len(),
                    r.removed,
                    r.kept,
                    r.errors.len(),
                    r.errors
                ))
            }
        }),
    ]
}

/// `GUARD_AGENTS_FAKE=1` (or `--fake`): the agent lane uses the scripted
/// fake runtime (plans from `GUARD_AGENT_FAKE_PLANS`); no model is called.
pub const AGENTS_FAKE_ENV: &str = "GUARD_AGENTS_FAKE";

/// Whether `guard serve` works lane-A tickets: only with `agents.toml`, a
/// Postgres store and thresholds in active mode (never in Shadow Mode).
pub fn agent_lane_gate(
    has_agents_toml: bool,
    postgres: bool,
    shadow: bool,
) -> Result<(), &'static str> {
    match (has_agents_toml, postgres, shadow) {
        (false, _, _) => Err("disabled (no agents.toml)"),
        (true, false, _) => Err("disabled (the agent lane needs DATABASE_URL on Postgres)"),
        (true, true, true) => Err("off in Shadow Mode (lane A is labelled, never worked)"),
        (true, true, false) => Ok(()),
    }
}

pub fn run(ctx: &Context, args: &ServeArgs) -> CliResult<u8> {
    let owned;
    let ctx = match &args.repo {
        Some(repo) => {
            owned = Context::at(&super::absolute(repo));
            &owned
        }
        None => ctx,
    };
    let config = ctx.load_config()?;
    let catalog = ctx.load_catalog(&config)?;
    let thresholds = load_thresholds(ctx, args.thresholds.as_deref())?;
    let api_token = env(crate::server_client::TOKEN_ENV).ok_or_else(|| {
        CliError::usage(format!(
            "{} must be set for guard serve",
            crate::server_client::TOKEN_ENV
        ))
    })?;
    let listen = listen_addr(args.listen.as_deref(), env("GUARD_LISTEN"))?;
    let fake_jev = args.fake || is_on(env(JEV_FAKE_ENV).as_deref());
    let fake_jira = args.fake || is_on(env(JIRA_FAKE_ENV).as_deref());
    let database_url = env(guard_store::DATABASE_URL_ENV)
        .or_else(|| args.fake.then(|| MEMORY_DATABASE_URL.to_string()));

    let impact = guard_impact::ImpactIndex::build(&ctx.root, &config).map_err(|e| match e {
        guard_core::ports::ImpactError::InvalidGlob(_) => CliError::usage(e.to_string()),
        guard_core::ports::ImpactError::Io(_) => CliError::env(e.to_string()),
    })?;

    let mut wiring = Wiring {
        engine: "jev",
        tracker: "disabled",
        store: "disabled",
        code_host: "disabled",
        evidence: "disabled",
        agents: "disabled",
        memory: "disabled",
    };
    let engine: Arc<dyn DecisionEngine> = if fake_jev {
        wiring.engine = "fake";
        Arc::new(FakeEngine)
    } else {
        match guard_jev::JevEngine::from_env() {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                warning(format!("engine: {e}; triage goes to the human lane"));
                wiring.engine = "unavailable";
                Arc::new(UnavailableEngine(e.to_string()))
            }
        }
    };

    block_on(async move {
        let mut state = AppState::new(
            engine,
            Arc::new(impact),
            config.clone(),
            catalog,
            thresholds,
            api_token,
        );

        if let Some(tracker) = &config.tracker {
            let project = tracker.project.clone().unwrap_or_default();
            if fake_jira {
                wiring.tracker = "fake";
                state = state.with_tracker(Arc::new(FakeJira::new(
                    &project,
                    env("GUARD_JIRA_WEBHOOK_SECRET"),
                )));
            } else {
                match guard_tracker::JiraConfig::from_env(
                    &project,
                    guard_tracker::DEFAULT_TOKEN_VAR,
                )
                .and_then(guard_tracker::JiraTracker::new)
                {
                    Ok(jira) => {
                        let jira = Arc::new(jira);
                        if jira.load_self_account_id().await.is_err() {
                            // Keep serving; the retry loop warns and logs recovery.
                            tokio::spawn(load_account_id_with_retry(
                                jira.clone(),
                                std::time::Duration::from_secs(5),
                            ));
                        }
                        wiring.tracker = "jira";
                        state = state.with_tracker(jira);
                    }
                    Err(e) => warning(format!("jira: {e}")),
                }
            }
        }

        let mut pg_store: Option<Arc<guard_store::PgStore>> = None;
        match database_url.as_deref() {
            Some(MEMORY_DATABASE_URL) => {
                let store = Arc::new(guard_store::MemoryStore::new());
                wiring.store = "memory";
                state = state
                    .with_decisions(store.clone() as Arc<dyn DecisionLog>)
                    .with_gates(store as Arc<dyn GateHistory>);
            }
            Some(url) => {
                let store = guard_store::PgStore::connect(url)
                    .await
                    .map_err(|e| CliError::env(format!("database: {e}")))?;
                store
                    .migrate()
                    .await
                    .map_err(|e| CliError::env(format!("database migrations: {e}")))?;
                let store = Arc::new(store);
                wiring.store = "postgres";
                pg_store = Some(store.clone());
                state = state
                    .with_decisions(store.clone() as Arc<dyn DecisionLog>)
                    .with_gates(store as Arc<dyn GateHistory>);
            }
            None => warning("DATABASE_URL is not set; triage and gate results are disabled"),
        }

        if let Some(code_host) = &config.code_host {
            match guard_codehost::from_env(code_host.kind) {
                Ok(host) => {
                    wiring.code_host = code_host.kind.as_str();
                    state = state.with_code_host(host as Arc<dyn CodeHost>);
                }
                Err(e) => warning(format!("code host: {e}")),
            }
        }

        if let Some(url) = env(&config.evidence.storage_url_env) {
            match guard_store::evidence_store_from_url(&url) {
                Ok(store) => {
                    wiring.evidence = "configured";
                    state = state.with_evidence(store);
                }
                Err(e) => warning(format!("evidence: {e}")),
            }
        }

        if let Some(store) = &pg_store {
            let evidence_url = env(&config.evidence.storage_url_env);
            let (writers, staleness) = memory_wiring(ctx, &config, store, evidence_url.as_deref());
            wiring.memory = "postgres";
            state = state.with_memory(writers).with_job(logged(staleness));
        }

        if let (Some(tracker), Some(options)) = (state.tracker.clone(), report_options(&config)) {
            let screenshots = EvidenceScreenshots(state.evidence.clone());
            state = state.with_reporter(Arc::new(TrackerReporter {
                tracker,
                screenshots,
                options,
            }));
        }
        let agents_path = crate::agent_lane::agents_toml_path(ctx);
        match agent_lane_gate(
            agents_path.is_file(),
            wiring.store == "postgres",
            state.thresholds.mode.is_shadow(),
        ) {
            Err(why) => {
                if agents_path.is_file() {
                    warning(format!("agents: {why}"));
                }
            }
            Ok(()) => {
                let fake_agents = args.fake || is_on(env(AGENTS_FAKE_ENV).as_deref());
                match crate::agent_lane::build(
                    ctx,
                    &config,
                    &crate::agent_lane::LaneOptions {
                        fake: fake_agents,
                        runtime: None,
                    },
                )
                .await
                {
                    Ok(lane) => {
                        let impact = state.impact.clone();
                        state = state.with_agent_lane(Arc::new(crate::agent_lane::QueueLane {
                            queue: lane.queue.clone(),
                            config: config.clone(),
                            repo: lane.repo.clone(),
                            tests_dir: lane.tests_dir.clone(),
                            root: ctx.root.clone(),
                            impact,
                        }));
                        wiring.agents = if fake_agents {
                            "fake runtime"
                        } else {
                            "active"
                        };
                        eprintln!("agents: PRs: {}", lane.publisher.describe());
                        for job in reaper_jobs(&lane) {
                            state = state.with_job(logged(job));
                        }
                        tokio::spawn(async move {
                            let result = guard_agents::lane::run_loop(
                                &lane.worker,
                                &lane.orchestrator,
                                std::time::Duration::from_secs(2),
                                |step| {
                                    if let guard_agents::lane::LoopStep::Error(e) = step {
                                        warning(format!("agents: worker step failed, backing off: {e}"));
                                    }
                                    if let guard_agents::lane::LoopStep::Ran(ran, after) = step {
                                        eprintln!(
                                            "agents: {} {} -> {:?}; {} run row(s), {} spend record(s), facts {:?}",
                                            ran.ticket,
                                            ran.role.as_str(),
                                            ran.disposition,
                                            after.runs_recorded,
                                            after.spend_records,
                                            after.facts_recorded
                                        );
                                        for w in &after.warnings {
                                            warning(format!("agents: {w}"));
                                        }
                                    }
                                    false
                                },
                            )
                            .await;
                            if let Err(e) = result {
                                warning(format!("agents: worker loop stopped: {e}"));
                            }
                        });
                    }
                    Err(e) => warning(format!(
                        "agents: {}; lane A is labelled, not worked",
                        e.messages.join("; ")
                    )),
                }
            }
        }

        let dashboard_token = env("GUARD_DASHBOARD_TOKEN");
        let has_dashboard_auth = dashboard_token.is_some();
        if let Some(token) = dashboard_token {
            state = state.with_dashboard_token(token);
        }

        println!(
            "serve: listening on http://{listen} (engine {}, tracker {}, store {}, code host {}, evidence {}, agents {}, memory {})",
            wiring.engine,
            wiring.tracker,
            wiring.store,
            wiring.code_host,
            wiring.evidence,
            wiring.agents,
            wiring.memory
        );
        if !state.jobs.is_empty() {
            let names: Vec<&str> = state.jobs.iter().map(|j| j.name()).collect();
            println!("serve: jobs: {}", names.join(", "));
        }
        let line = dashboard_line(has_dashboard_auth);
        if has_dashboard_auth {
            println!("serve: {line}");
        } else {
            warning(&line);
        }
        guard_server::serve(ServeOptions { listen }, state)
            .await
            .map_err(|e| CliError::env(e.to_string()))?;
        println!("serve: stopped");
        Ok(EXIT_OK)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_read_common_truthy_values() {
        for on in ["1", "true", "YES", " yes "] {
            assert!(is_on(Some(on)), "{on}");
        }
        for off in ["0", "", "no", "false"] {
            assert!(!is_on(Some(off)), "{off}");
        }
        assert!(!is_on(None));
    }

    #[test]
    fn listen_address_order_is_flag_env_default() {
        assert_eq!(
            listen_addr(None, None).unwrap(),
            "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            listen_addr(None, Some("127.0.0.1:9000".into()))
                .unwrap()
                .port(),
            9000
        );
        assert_eq!(
            listen_addr(Some("127.0.0.1:1"), Some("127.0.0.1:9000".into()))
                .unwrap()
                .port(),
            1
        );
        assert_eq!(listen_addr(Some("nope"), None).unwrap_err().code, 2);
    }

    #[tokio::test]
    async fn the_jira_account_id_is_retried_until_myself_answers() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"accountId": "guard-1"})),
            )
            .mount(&server)
            .await;
        let base = server.uri();
        let lookup = |name: &str| match name {
            "JIRA_BASE_URL" => Some(base.clone()),
            "JIRA_EMAIL" => Some("g@acme.test".into()),
            "JIRA_API_TOKEN" => Some("t".into()),
            _ => None,
        };
        let config =
            guard_tracker::JiraConfig::from_lookup("SHOP", "JIRA_API_TOKEN", lookup).unwrap();
        let jira = Arc::new(guard_tracker::JiraTracker::new(config).unwrap());
        let attempts = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            load_account_id_with_retry(jira.clone(), std::time::Duration::from_millis(10)),
        )
        .await
        .expect("retry loop finished");
        assert_eq!(attempts, 3);
        assert_eq!(jira.load_self_account_id().await.unwrap(), "guard-1");
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }

    #[test]
    fn the_agent_lane_runs_only_in_active_mode_with_postgres() {
        assert!(agent_lane_gate(true, true, false).is_ok());
        assert!(
            agent_lane_gate(true, true, true)
                .unwrap_err()
                .contains("Shadow")
        );
        assert!(agent_lane_gate(true, false, false).is_err());
        assert!(agent_lane_gate(false, true, false).is_err());
    }

    #[test]
    fn the_staleness_interval_comes_from_the_env_else_15_minutes() {
        use std::time::Duration;
        assert_eq!(staleness_interval(None), Duration::from_secs(900));
        assert_eq!(staleness_interval(Some("2h")), Duration::from_secs(7200));
        assert_eq!(staleness_interval(Some("soon")), Duration::from_secs(900));
    }

    #[tokio::test]
    async fn logged_jobs_keep_their_name_interval_and_result() {
        use std::time::Duration;
        let job = guard_server::jobs::PeriodicJob::new("j", Duration::from_secs(9), || async {
            Err::<String, String>("boom".into())
        });
        let job = logged(job);
        assert_eq!(job.name(), "j");
        assert_eq!(job.interval(), Duration::from_secs(9));
        assert_eq!(job.run_once().await.unwrap_err(), "boom");
    }

    #[test]
    fn the_dashboard_auth_line_warns_when_open() {
        assert!(dashboard_line(false).contains("OPEN"));
        assert!(dashboard_line(false).contains("GUARD_DASHBOARD_TOKEN"));
        assert_eq!(dashboard_line(true), "dashboard: token login");
    }

    #[tokio::test]
    async fn the_unavailable_engine_fails_closed() {
        let engine = UnavailableEngine("no key".into());
        assert!(
            engine
                .impact(&DiffSummary::default(), &[])
                .await
                .unwrap()
                .is_empty()
        );
        let questions = [JourneyQuestion {
            id: "a".into(),
            impact_question: "q".into(),
        }];
        assert!(matches!(
            engine.impact(&DiffSummary::default(), &questions).await,
            Err(EngineError::Unconfigured(_))
        ));
    }
}
