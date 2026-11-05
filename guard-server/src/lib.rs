//! `guard serve`.
//!
//! An axum application wired only through `guard_core::ports` traits: webhooks
//! from Jira, GitHub and GitLab, the triage use case, `/api/*` for the CLI
//! (`guard test --affected`, `guard gate finalize`, `guard repro`) and the
//! read-only HTML dashboard. Routes, auth and payloads: `docs/contracts/server-api.md`.
//!
//! Adapters are injected as `guard_core::ports` trait objects: the
//! integrator (`guard-cli serve`) builds them from the environment, fills an
//! [`AppState`] and calls [`serve`]. Tests drive [`router`] with in-memory
//! fakes. Two crates are linked for orchestrator logic, not adapters:
//! `guard-store`'s fact writers and staleness job ([`memory`],
//! [`staleness`]) and `guard-impact`'s import graph (staleness fan-out).
//! Periodic work (staleness, reapers) runs as [`jobs::PeriodicJob`]s.

pub mod api;
pub mod auth;
pub mod body;
pub mod clock;
pub mod dashboard;
pub mod deliveries;
pub mod error;
pub mod gate_runs;
pub mod jobs;
pub mod memory;
pub mod report;
pub mod staleness;
pub mod tasks;
pub mod triage;
pub mod webhooks;

pub use auth::{AuthOutcome, AuthProvider, AuthRequest, Principal};
pub use clock::{Clock, FixedClock, SystemClock};
pub use report::{GateReporter, PortGateReporter, ReporterOptions};
pub use tasks::BackgroundTasks;
pub use triage::AgentLane;

use axum::Router;
use axum::middleware::from_fn_with_state;
use axum::routing::{get, post};
use guard_core::catalog::Catalog;
use guard_core::config::TargetConfig;
use guard_core::ports::{
    CodeHost, DecisionEngine, DecisionLog, EvidenceStore, GateHistory, ImpactAnalyzer,
    IssueTracker, Thresholds,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// Everything a request handler can reach. The fields up to `dashboard_token`
/// are the contract surface of server-api.md; the rest are injection points
/// with defaults (see [`AppState::new`]).
#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<dyn DecisionEngine>,
    pub tracker: Option<Arc<dyn IssueTracker>>,
    pub code_host: Option<Arc<dyn CodeHost>>,
    pub decisions: Option<Arc<dyn DecisionLog>>,
    pub gates: Option<Arc<dyn GateHistory>>,
    pub evidence: Option<Arc<dyn EvidenceStore>>,
    pub impact: Arc<dyn ImpactAnalyzer>,
    pub config: TargetConfig,
    pub catalog: Catalog,
    pub thresholds: Thresholds,
    pub api_token: String,
    pub dashboard_token: Option<String>,
    /// Jira reporting of `/api/gate-result`. `None`: [`PortGateReporter`]
    /// over `tracker` + `evidence` (the integrator plugs
    /// `guard_tracker::report::report_gate` here).
    pub reporter: Option<Arc<dyn GateReporter>>,
    /// Dashboard login. `None`: the token provider when `dashboard_token` is
    /// set, else open. GitHub/GitLab OAuth (Phase 3) plugs in here.
    pub dashboard_auth: Option<Arc<dyn AuthProvider>>,
    pub clock: Arc<dyn Clock>,
    /// Webhook processing started after the `202`.
    pub tasks: BackgroundTasks,
    /// Per-run reporting outcomes: `/api/gate-result` reports a run once.
    pub gate_runs: gate_runs::GateRuns,
    /// Deadline of the Jira reporting of one gate result.
    pub report_timeout: std::time::Duration,
    /// Duplicate suppression of Jira webhook redeliveries.
    pub deliveries: deliveries::RecentDeliveries,
    /// Retries of accepted webhook processing.
    pub webhook_retry: deliveries::RetryPolicy,
    /// Phase 4 agent lane: receives audited lane-A tickets in active mode
    /// only (never in Shadow Mode). `None`: lane A is labelled, not worked.
    pub agent_lane: Option<Arc<dyn AgentLane>>,
    /// Orchestrator fact writers (memory.md "Writers"): gate results, flaky
    /// history and label overrides. `None`: no facts are written.
    pub memory: Option<Arc<memory::MemoryWriters>>,
    /// Started by [`serve`], stopped at shutdown (staleness, reapers).
    pub jobs: Vec<jobs::PeriodicJob>,
}

/// Default [`AppState::report_timeout`]: well under the CLI's 120 s.
pub const REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

impl AppState {
    /// The mandatory ports; every optional adapter starts unconfigured.
    pub fn new(
        engine: Arc<dyn DecisionEngine>,
        impact: Arc<dyn ImpactAnalyzer>,
        config: TargetConfig,
        catalog: Catalog,
        thresholds: Thresholds,
        api_token: String,
    ) -> Self {
        Self {
            engine,
            tracker: None,
            code_host: None,
            decisions: None,
            gates: None,
            evidence: None,
            impact,
            config,
            catalog,
            thresholds,
            api_token,
            dashboard_token: None,
            reporter: None,
            dashboard_auth: None,
            clock: Arc::new(SystemClock),
            tasks: BackgroundTasks::new(),
            gate_runs: gate_runs::GateRuns::new(),
            report_timeout: REPORT_TIMEOUT,
            deliveries: deliveries::RecentDeliveries::new(),
            webhook_retry: deliveries::RetryPolicy::default(),
            agent_lane: None,
            memory: None,
            jobs: Vec::new(),
        }
    }

    pub fn with_memory(mut self, memory: Arc<memory::MemoryWriters>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_job(mut self, job: jobs::PeriodicJob) -> Self {
        self.jobs.push(job);
        self
    }

    pub fn with_agent_lane(mut self, lane: Arc<dyn AgentLane>) -> Self {
        self.agent_lane = Some(lane);
        self
    }

    pub fn with_webhook_retry(
        mut self,
        attempts: u32,
        initial_backoff: std::time::Duration,
    ) -> Self {
        self.webhook_retry = deliveries::RetryPolicy {
            attempts,
            initial_backoff,
        };
        self
    }

    pub fn with_report_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.report_timeout = timeout;
        self
    }

    pub fn with_tracker(mut self, tracker: Arc<dyn IssueTracker>) -> Self {
        self.tracker = Some(tracker);
        self
    }

    pub fn with_code_host(mut self, code_host: Arc<dyn CodeHost>) -> Self {
        self.code_host = Some(code_host);
        self
    }

    pub fn with_decisions(mut self, decisions: Arc<dyn DecisionLog>) -> Self {
        self.decisions = Some(decisions);
        self
    }

    pub fn with_gates(mut self, gates: Arc<dyn GateHistory>) -> Self {
        self.gates = Some(gates);
        self
    }

    pub fn with_evidence(mut self, evidence: Arc<dyn EvidenceStore>) -> Self {
        self.evidence = Some(evidence);
        self
    }

    pub fn with_dashboard_token(mut self, token: String) -> Self {
        self.dashboard_token = Some(token);
        self
    }

    pub fn with_dashboard_auth(mut self, auth: Arc<dyn AuthProvider>) -> Self {
        self.dashboard_auth = Some(auth);
        self
    }

    pub fn with_reporter(mut self, reporter: Arc<dyn GateReporter>) -> Self {
        self.reporter = Some(reporter);
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// Every route of server-api.md.
pub fn router(state: AppState) -> Router {
    let state = Arc::new(state);

    let api = Router::new()
        .route("/api/impact", post(api::impact))
        .route("/api/gate-result", post(api::gate_result))
        .route("/api/pr-context", get(api::pr_context))
        .route("/api/{*rest}", axum::routing::any(api::not_found))
        .route_layer(from_fn_with_state(state.clone(), api::require_api_token));

    let webhooks = Router::new()
        .route("/webhooks/jira", post(webhooks::jira))
        .route("/webhooks/github", post(webhooks::github))
        .route("/webhooks/gitlab", post(webhooks::gitlab));

    let dashboard = Router::new()
        .route("/", get(dashboard::overview))
        .route("/catalog", get(dashboard::catalog))
        .route("/coverage", get(dashboard::coverage))
        .route("/flaky", get(dashboard::flaky))
        .route("/durations", get(dashboard::durations_view))
        .route("/guards", get(dashboard::guards))
        .route("/decisions", get(dashboard::decisions_view))
        .route("/agents", get(dashboard::agents))
        .route_layer(from_fn_with_state(state.clone(), dashboard::require_login))
        .layer(axum::middleware::map_response(dashboard::security_headers));

    Router::new()
        .merge(api)
        .merge(webhooks)
        .merge(dashboard)
        .route("/healthz", get(api::healthz))
        .route(dashboard::HTMX_PATH, get(dashboard::htmx))
        .fallback(api::not_found)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeOptions {
    pub listen: SocketAddr,
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("cannot bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("server error: {0}")]
    Io(#[from] std::io::Error),
}

/// How long shutdown waits for accepted webhook deliveries: under the 10 s
/// grace period of `docker stop` and the 30 s of Kubernetes.
pub const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(8);

/// Resolves on Ctrl-C, or on SIGTERM (`docker stop`, Kubernetes) on unix.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let terminate = async {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut signal) => {
                    signal.recv().await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot listen for SIGTERM");
                    std::future::pending::<()>().await;
                }
            }
        };
        tokio::select! {
            () = ctrl_c => {},
            () = terminate => {},
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}

/// Binds `options.listen` and serves [`router`] until Ctrl-C or SIGTERM,
/// running `state.jobs` meanwhile, then stops the jobs and waits up to
/// [`SHUTDOWN_GRACE`] for accepted webhook deliveries.
pub async fn serve(options: ServeOptions, state: AppState) -> Result<(), ServeError> {
    let tasks = state.tasks.clone();
    let jobs = state.jobs.clone();
    let listener = tokio::net::TcpListener::bind(options.listen)
        .await
        .map_err(|source| ServeError::Bind {
            addr: options.listen,
            source,
        })?;
    tracing::info!(addr = %options.listen, "guard serve listening");
    let running = jobs::start(&jobs);
    let served = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await;
    running.stop().await;
    served?;
    if !tasks.drain(SHUTDOWN_GRACE).await {
        tracing::warn!(
            in_flight = tasks.in_flight(),
            "shutdown grace period over; abandoning webhook deliveries in flight"
        );
    }
    Ok(())
}
