//! The egui layer: state, event draining, project switching and the window
//! shell. Every decision it needs (argv, gate, parsing, project rules) comes
//! from the tested modules of this crate.

use crate::ai_settings::{self, AiSettings};
use crate::args::{Screenshot, ShotView};
use crate::catalog::{self, JourneyRow};
use crate::commands::{DEFAULT_UP_TIMEOUT_SECS, ExportFormat, GuardCommand, RunMode};
use crate::guard_bin::NotFound;
use crate::integrations::{self, IntegrationSettings};
use crate::log_buffer::{DEFAULT_CAP, LogBuffer};
use crate::models::{self, FetchKey, ModelCatalog};
use crate::projects::{self, RecentProjects, Validation};
use crate::results::{self, JourneyResult};
use crate::run_progress::RunProgress;
use crate::runner::{self, Captured, Exit, PROBE_TIMEOUT, Run, RunEvent};
use crate::safety::{self, Gate};
use crate::status::{self, Affected, StatusReport};
use crate::suspects::SuspectsDoc;
use crate::{env_status, flow, guard_bin, theme};
use eframe::egui::{self, RichText};
use guard_core::durations::DurationHistory;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

mod ai_view;
mod demo;
mod flow_view;
mod integrations_view;
mod run_view;
mod shell;
mod suspects_view;
#[cfg(test)]
mod tests;
mod tests_view;
mod widgets;

const RECENTS_KEY: &str = "recent_projects";
const LAYOUT_KEY: &str = "layout";
/// Provider, base URL, model and language; never the key.
const AI_SETTINGS_KEY: &str = "ai_settings";
/// Jira site and email, gateway URL; never the Jira token.
const INTEGRATIONS_KEY: &str = "integration_settings";
/// The [`GuardApp::explain`] key of the Problems tab's "Explain failures"
/// (every failing/flaky journey). Journey ids never contain `*`.
const ALL_FAILURES: &str = "*all-failures*";
const BRANCH_TIMEOUT: Duration = Duration::from_secs(10);
/// A model call can take much longer than a status probe.
const EXPLAIN_TIMEOUT: Duration = Duration::from_secs(180);

/// The main view, switched from the left tool-window stripe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum View {
    Tests,
    Flow,
    Run,
}

/// The tabs of the bottom tool window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum BottomTab {
    Log,
    Problems,
}

/// What survives a restart besides the recent projects.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Layout {
    view: View,
    bottom_open: bool,
    bottom_tab: BottomTab,
    up_timeout: u32,
    /// How `guard test` drives the browser; absent in layouts saved before it.
    #[serde(default)]
    run_mode: RunMode,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            view: View::Tests,
            bottom_open: true,
            bottom_tab: BottomTab::Log,
            up_timeout: DEFAULT_UP_TIMEOUT_SECS,
            run_mode: RunMode::Headless,
        }
    }
}

/// A short read-only command whose output is parsed when it finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProbeKind {
    Status,
    Affected,
}

/// Background work is tagged with the project epoch it was started for, so
/// nothing from a previous project can land in the current one.
struct Probe {
    kind: ProbeKind,
    epoch: u64,
    rx: Receiver<Result<Captured, String>>,
}

/// Per-journey state of the AI Summary panel.
#[derive(Debug, Clone)]
enum ExplainState {
    Running,
    Done(String),
    Error(String),
}

/// An "Export .md" / "Export Excel" of one explain key.
#[derive(Debug, Clone)]
enum ExportState {
    Running(ExportFormat),
    Done(PathBuf),
    Error(String),
}

/// A `guard explain --format <md|xlsx> --out <path>` call in flight.
struct ExportJob {
    key: String,
    epoch: u64,
    path: PathBuf,
    rx: Receiver<Result<Captured, String>>,
}

/// A `guard explain --list-models --json` call in flight, for the settings
/// it was started with (see [`ModelCatalog`]).
struct ModelsJob {
    key: FetchKey,
    rx: Receiver<Result<Captured, String>>,
}

/// A `guard explain --json` call in flight for one journey. Epoch-tagged so a
/// project switch drops its result; the key is the journey it explains.
struct ExplainJob {
    key: String,
    epoch: u64,
    rx: Receiver<Result<Captured, String>>,
}

/// A `guard suspects --json` call in flight for one report.
struct SuspectsJob {
    stamp: Option<results::ReportStamp>,
    epoch: u64,
    rx: Receiver<Result<Captured, String>>,
}

/// A `guard status --integrations --json` call in flight.
struct IntegrationsJob {
    epoch: u64,
    rx: Receiver<Result<Captured, String>>,
    /// It ran with the side panel's own Jira site
    /// ([`integrations::mark_gui_jira_probe`]).
    gui_jira: bool,
}

struct ActiveRun {
    run: Run,
    command: GuardCommand,
    epoch: u64,
    /// The report before the run started, to tell a report this run wrote
    /// from an earlier one.
    report_before: Option<results::ReportStamp>,
}

/// A run waiting for the user's confirmation (the safety gate).
struct Pending {
    command: GuardCommand,
    journeys: Vec<String>,
    note: Option<String>,
}

struct Project {
    root: PathBuf,
    name: String,
    validation: Validation,
}

/// The screenshot harness (`--screenshot`): a few frames, one PNG, exit.
struct Shot {
    path: PathBuf,
    frames: u32,
    requested: bool,
    started: Instant,
}

/// What `main` passes in.
pub struct Launch {
    pub repo: Option<PathBuf>,
    pub cwd: PathBuf,
    pub screenshot: Option<Screenshot>,
}

pub struct GuardApp {
    project: Option<Project>,
    recents: RecentProjects,
    /// Bumped on every project switch.
    epoch: u64,
    branch: Option<String>,
    branch_rx: Option<(u64, Receiver<Option<String>>)>,
    folder_rx: Option<Receiver<Option<PathBuf>>>,
    /// A switch that waits for the user because a run is in progress.
    pending_switch: Option<(PathBuf, Validation)>,

    guard: Result<PathBuf, NotFound>,
    env_names: Vec<String>,
    view: View,
    bottom_open: bool,
    bottom_tab: BottomTab,

    rows: Vec<JourneyRow>,
    catalog_error: Option<String>,
    results: BTreeMap<String, JourneyResult>,
    results_note: Option<String>,
    /// AI Summary state per journey id (or [`ALL_FAILURES`]), and the single
    /// in-flight explain call.
    explain: BTreeMap<String, ExplainState>,
    explain_job: Option<ExplainJob>,
    /// The last export per explain key, and the single export in flight.
    exports: BTreeMap<String, ExportState>,
    export_job: Option<ExportJob>,
    /// Why the failures summary did not start by itself after the last run.
    auto_summary_hint: Option<String>,
    /// A run's summary waiting for the explanation in flight to finish.
    auto_summary_queued: bool,
    /// "Possibly related changes": `guard suspects --json` for the report
    /// with this stamp (read once per report), and the call in flight.
    suspects: Option<(Option<results::ReportStamp>, Result<SuspectsDoc, String>)>,
    suspects_job: Option<SuspectsJob>,
    /// AI settings (persisted, non-secret) and the settings window. The key
    /// itself is never held here: only whether one is saved in the OS store
    /// for `ai_key_host` (the settings' current host, see
    /// [`Self::sync_key_saved`]), and the password field's text until Save
    /// clears it.
    ai: AiSettings,
    ai_key_host: String,
    ai_key_saved: bool,
    ai_key_input: String,
    ai_window: bool,
    ai_message: Option<(bool, String)>,
    /// The Model combo's lists, fetched off the UI thread.
    models: ModelCatalog,
    models_job: Option<ModelsJob>,
    /// "Other" is chosen in the Model combo: the id is typed in a text field.
    model_other: bool,
    /// Where `guard explain --list-models` runs without an open project.
    cwd: PathBuf,
    /// The report the results were read from (age for the status bar).
    report_stamp: Option<results::ReportStamp>,
    durations: Option<DurationHistory>,

    search: String,
    collapsed: BTreeSet<String>,
    selected: BTreeSet<String>,
    focused: Option<String>,
    shown_images: Vec<String>,

    affected: Option<Result<Affected, String>>,
    status: Option<Result<StatusReport, String>>,
    probe: Option<Probe>,

    run: Option<ActiveRun>,
    log: LogBuffer,
    last_exit: Option<(String, Exit)>,
    up_timeout: u32,
    /// The run-mode selector (persisted with the layout).
    run_mode: RunMode,

    pending: Option<Pending>,
    /// Run affected was asked for (in this mode): gate it once a fresh
    /// preview is in.
    gate_affected: Option<RunMode>,
    notice: Option<String>,

    shot: Option<Shot>,

    flow: flow_view::FlowState,
    /// How the last test run of this project ended (the Flow's live result).
    last_test: Option<flow::Verdict>,
    /// How far the running test run is (from its log lines).
    run_progress: RunProgress,

    /// The integrations report of this project, the call in flight and when
    /// the last one started (see [`integrations::poll_due`]).
    integrations: Option<Result<integrations::Report, String>>,
    integrations_job: Option<IntegrationsJob>,
    integrations_at: Option<Instant>,
    /// The side panel's settings (persisted, non-secret; only ever set
    /// through their checks), whether a Jira token is saved for their site,
    /// and the form fields being edited, copied into the settings by
    /// "Guardar" (the token field is cleared once saved).
    integration_settings: IntegrationSettings,
    jira_token_saved: bool,
    jira_base_url_input: String,
    jira_email_input: String,
    gateway_url_input: String,
    jira_token_input: String,
    jira_project_input: String,
    server_url_input: String,
    /// "Cuentas y roles": the `[testability]` fields being edited and the
    /// role typed before "Añadir".
    testability_input: crate::accounts::TestabilityEdit,
    new_role_input: String,
    config_message: Option<(bool, String)>,
    /// The OS asks for less motion: the diagram stays still.
    reduced_motion: bool,
}

impl GuardApp {
    pub fn new(cc: &eframe::CreationContext<'_>, launch: Launch) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        let shot_mode = launch.screenshot.is_some();
        // The harness never reads or writes the user's saved state.
        let storage = cc.storage.filter(|_| !shot_mode);
        let recents = storage
            .and_then(|s| eframe::get_value::<RecentProjects>(s, RECENTS_KEY))
            .map(|r| RecentProjects::from_paths(r.paths().iter().cloned()))
            .unwrap_or_default();
        let layout: Layout = storage
            .and_then(|s| eframe::get_value(s, LAYOUT_KEY))
            .unwrap_or_default();
        let ai: AiSettings = storage
            .and_then(|s| eframe::get_value(s, AI_SETTINGS_KEY))
            .unwrap_or_default();
        // Only whether a key is saved for this host; the harness never
        // touches the store.
        let ai_key_host = ai.host();
        let ai_key_saved = !shot_mode && key_saved_for(&ai_key_host);
        let integration_settings: IntegrationSettings = storage
            .and_then(|s| eframe::get_value(s, INTEGRATIONS_KEY))
            .unwrap_or_default();
        let jira_token_saved =
            !shot_mode && integrations_view::jira_token_saved(&integration_settings);
        let mut app = Self {
            project: None,
            recents,
            epoch: 0,
            branch: None,
            branch_rx: None,
            folder_rx: None,
            pending_switch: None,
            guard: guard_bin::from_process(),
            env_names: env_status::from_process(),
            view: layout.view,
            bottom_open: layout.bottom_open,
            bottom_tab: layout.bottom_tab,
            rows: Vec::new(),
            catalog_error: None,
            results: BTreeMap::new(),
            results_note: None,
            explain: BTreeMap::new(),
            explain_job: None,
            exports: BTreeMap::new(),
            export_job: None,
            auto_summary_hint: None,
            auto_summary_queued: false,
            suspects: None,
            suspects_job: None,
            ai,
            ai_key_host,
            ai_key_saved,
            ai_key_input: String::new(),
            ai_window: false,
            ai_message: None,
            models: ModelCatalog::default(),
            models_job: None,
            model_other: false,
            cwd: launch.cwd.clone(),
            report_stamp: None,
            durations: None,
            search: String::new(),
            collapsed: BTreeSet::new(),
            selected: BTreeSet::new(),
            focused: None,
            shown_images: Vec::new(),
            affected: None,
            status: None,
            probe: None,
            run: None,
            log: LogBuffer::new(DEFAULT_CAP),
            last_exit: None,
            up_timeout: layout.up_timeout,
            run_mode: layout.run_mode,
            pending: None,
            gate_affected: None,
            notice: None,
            shot: None,
            flow: flow_view::FlowState::default(),
            last_test: None,
            run_progress: RunProgress::default(),
            integrations: None,
            integrations_job: None,
            integrations_at: None,
            jira_base_url_input: integration_settings.jira_base_url.clone(),
            jira_email_input: integration_settings.jira_email.clone(),
            gateway_url_input: integration_settings.gateway_url.clone(),
            integration_settings,
            jira_token_saved,
            jira_token_input: String::new(),
            jira_project_input: String::new(),
            server_url_input: String::new(),
            testability_input: Default::default(),
            new_role_input: String::new(),
            config_message: None,
            reduced_motion: !shot_mode && integrations_view::os_reduced_motion(),
        };
        let ctx = &cc.egui_ctx;
        if let Some(shot) = launch.screenshot {
            app.view = match shot.view {
                ShotView::Tests | ShotView::AiSettings | ShotView::Problems => View::Tests,
                ShotView::Run => View::Run,
                ShotView::Flow => View::Flow,
            };
            app.ai_window = shot.view == ShotView::AiSettings;
            // The Flow diagram is tall: its shots leave the tool window shut.
            app.bottom_open = shot.view != ShotView::Flow;
            app.flow = flow_view::FlowState::from_shot(&shot.flow);
            app.bottom_tab = match shot.view {
                ShotView::Tests | ShotView::AiSettings | ShotView::Problems => BottomTab::Problems,
                _ => BottomTab::Log,
            };
            app.shot = Some(Shot {
                path: shot.path,
                frames: 0,
                requested: false,
                started: Instant::now(),
            });
            match launch.repo {
                Some(repo) => app.open_project(ctx, repo, false),
                None => app.load_demo(shot.view),
            }
            return app;
        }
        let initial = projects::initial_project(
            launch.repo.clone(),
            &app.recents,
            &launch.cwd,
            projects::is_project_fs,
        );
        if let Some(path) = initial {
            // --repo opens even an incomplete folder and says what is missing.
            app.open_project(ctx, path, launch.repo.is_none());
        }
        app
    }

    fn repo(&self) -> Option<&Path> {
        self.project.as_ref().map(|p| p.root.as_path())
    }

    /// Opens `path` as the current project. `strict` refuses a folder without
    /// `target.toml` (the selector and the folder dialog); `--repo` is not
    /// strict and shows what is missing instead.
    fn open_project(&mut self, ctx: &egui::Context, path: PathBuf, strict: bool) {
        let root = projects::canonical(&path);
        // Opening the project that is already open changes nothing.
        if projects::is_current(self.repo(), &root, cfg!(windows)) {
            return;
        }
        let validation = projects::validate_fs(&root);
        if strict && !validation.is_project() {
            self.notice = Some(format!(
                "{} is not a guard project: it has no target.toml.",
                root.display()
            ));
            return;
        }
        if self.running() {
            self.pending_switch = Some((root, validation));
            return;
        }
        self.switch_to(ctx, root, validation);
    }

    fn switch_to(&mut self, ctx: &egui::Context, root: PathBuf, validation: Validation) {
        // Everything in flight belongs to the previous project: stop it and
        // drop its channels so none of its results can land in this one.
        if let Some(active) = self.run.take() {
            active.run.cancel();
        }
        self.probe = None;
        self.explain.clear();
        self.explain_job = None;
        self.exports.clear();
        self.export_job = None;
        self.auto_summary_hint = None;
        self.auto_summary_queued = false;
        self.suspects = None;
        self.suspects_job = None;
        self.branch_rx = None;
        self.gate_affected = None;
        self.pending = None;
        self.epoch += 1;

        self.log.clear();
        self.last_exit = None;
        self.selected.clear();
        self.set_focus(ctx, None);
        self.affected = None;
        self.status = None;
        self.branch = None;
        self.search.clear();
        self.collapsed.clear();
        self.last_test = None;
        self.run_progress = RunProgress::default();
        self.flow.live_hidden = false;
        self.integrations = None;
        self.integrations_job = None;
        self.integrations_at = None;
        self.config_message = None;
        self.jira_token_input.clear();
        self.new_role_input.clear();

        if validation.is_project() && self.shot.is_none() {
            self.recents.touch(root.clone());
        }
        self.project = Some(Project {
            name: projects::display_name(&root),
            root,
            validation,
        });
        self.reload_catalog();
        self.reload_results();
        self.load_config_inputs();
        self.start_branch(ctx);
        if self.shot.is_none() {
            self.start_probe(ctx, ProbeKind::Status);
        }
    }

    fn browse_for_project(&mut self, ctx: &egui::Context) {
        if self.folder_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let start = self.repo().map(Path::to_path_buf);
        let repaint = ctx.clone();
        // The OS dialog blocks: keep it off the UI thread.
        std::thread::spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title("Open project");
            if let Some(dir) = start.as_deref().and_then(Path::parent) {
                dialog = dialog.set_directory(dir);
            }
            let _ = tx.send(dialog.pick_folder());
            repaint.request_repaint();
        });
        self.folder_rx = Some(rx);
    }

    fn start_branch(&mut self, ctx: &egui::Context) {
        let Some(root) = self.repo().map(Path::to_path_buf) else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let args = ["rev-parse", "--abbrev-ref", "HEAD"].map(String::from);
            let branch = runner::capture(Path::new("git"), &args, &root, BRANCH_TIMEOUT)
                .ok()
                .filter(|c| c.code == Some(0))
                .and_then(|c| projects::parse_branch(&c.stdout));
            let _ = tx.send(branch);
            repaint.request_repaint();
        });
        self.branch_rx = Some((self.epoch, rx));
    }

    fn reload_catalog(&mut self) {
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            self.rows.clear();
            self.catalog_error = None;
            return;
        };
        match catalog::load(&repo) {
            Ok(rows) => {
                self.rows = rows;
                self.catalog_error = None;
            }
            Err(e) => {
                self.rows.clear();
                self.catalog_error = Some(e.to_string());
            }
        }
        let known: BTreeSet<&str> = self.rows.iter().map(|r| r.id.as_str()).collect();
        self.selected.retain(|id| known.contains(id.as_str()));
    }

    fn reload_results(&mut self) {
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            self.results.clear();
            self.results_note = None;
            self.report_stamp = None;
            self.durations = None;
            return;
        };
        self.report_stamp = results::report_stamp(&repo);
        match results::load(&repo) {
            Ok(Some(map)) => {
                self.results = map;
                self.results_note = None;
            }
            Ok(None) => {
                self.results.clear();
                self.results_note = Some("No report yet: run some tests.".into());
            }
            Err(e) => {
                self.results.clear();
                self.results_note = Some(e);
            }
        }
        self.durations = match results::load_durations(&repo) {
            Ok(history) => history,
            Err(e) => {
                self.results_note = Some(format!(".guard/durations.json: {e}"));
                None
            }
        };
    }

    fn set_focus(&mut self, ctx: &egui::Context, id: Option<String>) {
        if self.focused != id {
            // Decoded screenshots are the biggest allocations: free them.
            for uri in self.shown_images.drain(..) {
                ctx.forget_image(&uri);
            }
            self.focused = id;
        }
    }

    fn row(&self, id: &str) -> Option<&JourneyRow> {
        self.rows.iter().find(|r| r.id == id)
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    fn affected_ids(&self) -> &[String] {
        match &self.affected {
            Some(Ok(a)) => &a.ids,
            _ => &[],
        }
    }

    /// The state icon of a journey.
    fn state_of(&self, row: &JourneyRow) -> theme::TestState {
        theme::journey_state(
            self.results.get(&row.id).map(|r| r.outcome),
            self.affected_ids().contains(&row.id),
            row.quarantined,
        )
    }

    /// Runs the safety gate for a test command, then starts it or asks first.
    /// Run affected first refreshes the affected preview, then is gated in
    /// [`Self::gate_suite`] when that preview comes back.
    fn request_test(&mut self, ctx: &egui::Context, command: GuardCommand) {
        if self.project.is_none() {
            self.notice = Some("Open a project first.".into());
            return;
        }
        match command {
            GuardCommand::TestJourneys(ids, mode) => {
                let classes: Vec<(&str, safety::Safety)> = ids
                    .iter()
                    .map(|id| {
                        let class = self
                            .row(id)
                            .map_or(safety::Safety::Unclassified, |r| r.safety);
                        (id.as_str(), class)
                    })
                    .collect();
                let gate = safety::check(&classes);
                self.apply_gate(ctx, GuardCommand::TestJourneys(ids, mode), gate, None);
            }
            GuardCommand::TestAll(mode) => self.gate_suite(ctx, GuardCommand::TestAll(mode)),
            GuardCommand::TestAffected(mode) => {
                // No guard binary: start_probe says so and nothing is awaited.
                self.gate_affected = self.guard.is_ok().then_some(mode);
                self.start_probe(ctx, ProbeKind::Affected);
            }
            command => self.start(ctx, command),
        }
    }

    /// The gate for Run all / Run affected over every journey guard may run,
    /// from a freshly reloaded catalog (and, for affected, a fresh preview).
    fn gate_suite(&mut self, ctx: &egui::Context, command: GuardCommand) {
        self.reload_catalog();
        let catalog: Vec<safety::Candidate<'_>> = self
            .rows
            .iter()
            .map(|r| safety::Candidate {
                id: &r.id,
                smoke: r.smoke,
                quarantined: r.quarantined,
                safety: r.safety,
            })
            .collect();
        let (suite, note) = match (&command, &self.affected) {
            (GuardCommand::TestAffected(_), Some(Ok(a))) => {
                let ticket = a.tags.iter().find(|t| t.starts_with("@ticket:"));
                let note = match ticket {
                    Some(t) => format!(
                        "{t} is selected: any journey whose spec carries it may run, so every journey is checked."
                    ),
                    None => {
                        "Fresh affected preview plus every @smoke journey (quarantined ones too)."
                            .to_string()
                    }
                };
                let preview = safety::AffectedPreview {
                    ids: &a.ids,
                    tags: &a.tags,
                };
                (safety::Suite::Affected(Some(preview)), note)
            }
            (GuardCommand::TestAffected(_), _) => (
                safety::Suite::Affected(None),
                "The affected preview failed: any journey may run, so every journey is checked."
                    .to_string(),
            ),
            _ => (
                safety::Suite::All,
                "Every active journey plus every @smoke journey (quarantined ones too)."
                    .to_string(),
            ),
        };
        let gate = safety::suite_gate(suite, &catalog);
        self.apply_gate(ctx, command, gate, Some(note));
    }

    fn apply_gate(
        &mut self,
        ctx: &egui::Context,
        command: GuardCommand,
        gate: Gate,
        note: Option<String>,
    ) {
        match gate {
            Gate::Allowed => self.start(ctx, command),
            Gate::Confirm(journeys) => {
                self.pending = Some(Pending {
                    command,
                    journeys,
                    note,
                })
            }
            Gate::Blocked(journeys) => {
                self.notice = Some(format!(
                    "Not run: dangerous journeys cannot run from the GUI: {}",
                    journeys.join(", ")
                ));
            }
            Gate::Empty => self.notice = Some("Nothing to run.".into()),
        }
    }

    /// Starts `command` in the runner (one run at a time).
    fn start(&mut self, ctx: &egui::Context, command: GuardCommand) {
        if self.running() {
            self.notice = Some("A run is already in progress.".into());
            return;
        }
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            self.notice = Some("Open a project first.".into());
            return;
        };
        let Ok(guard) = self.guard.clone() else {
            self.notice = Some("guard binary not found: set GUARD_BIN or add it to PATH.".into());
            return;
        };
        self.log.clear();
        self.log.push(format!("$ {}", command.display()));
        self.last_exit = None;
        self.bottom_open = true;
        self.bottom_tab = BottomTab::Log;
        // A new run shows up live in the Flow view.
        self.flow.live_hidden = false;
        self.run_progress = RunProgress::default();
        let report_before = results::report_stamp(&repo);
        let repaint = ctx.clone();
        match Run::spawn(&guard, &command.args(), &repo, move || {
            repaint.request_repaint()
        }) {
            Ok(run) => {
                self.run = Some(ActiveRun {
                    run,
                    command,
                    epoch: self.epoch,
                    report_before,
                });
            }
            Err(e) => self.log.push(format!("cannot start guard: {e}")),
        }
    }

    fn start_probe(&mut self, ctx: &egui::Context, kind: ProbeKind) {
        if self.probe.is_some() {
            return;
        }
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            return;
        };
        let Ok(guard) = self.guard.clone() else {
            if kind == ProbeKind::Affected {
                self.notice =
                    Some("guard binary not found: set GUARD_BIN or add it to PATH.".into());
            }
            return;
        };
        let command = match kind {
            ProbeKind::Status => GuardCommand::Status,
            ProbeKind::Affected => GuardCommand::Affected,
        };
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let out = runner::capture(&guard, &command.args(), &repo, PROBE_TIMEOUT)
                .map_err(|e| format!("{}: {e}", command.display()));
            let _ = tx.send(out);
            repaint.request_repaint();
        });
        self.probe = Some(Probe {
            kind,
            epoch: self.epoch,
            rx,
        });
    }

    /// Starts `guard explain --json [--journey <id>]` off the UI thread; `key`
    /// is a journey id, or [`ALL_FAILURES`] for every failure. One at a time;
    /// the result lands in [`Self::drain`] and fills the AI Summary panel.
    ///
    /// The AI settings and the saved key reach the child through its
    /// environment only ([`ai_settings::child_env`]): never its argv, which
    /// other processes can read, and never the run log. The key is read from
    /// the credential store on the worker thread and dropped with it; any
    /// output shown afterwards has it redacted.
    fn start_explain(&mut self, ctx: &egui::Context, key: String) {
        if self.explain_job.is_some() {
            return;
        }
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            return;
        };
        let Ok(guard) = self.guard.clone() else {
            self.notice = Some("guard binary not found: set GUARD_BIN or add it to PATH.".into());
            return;
        };
        let command = GuardCommand::Explain {
            journey: (key != ALL_FAILURES).then(|| key.clone()),
        };
        self.explain.insert(key.clone(), ExplainState::Running);
        let rx = self.spawn_explain_child(ctx, guard, command, repo);
        self.explain_job = Some(ExplainJob {
            key,
            epoch: self.epoch,
            rx,
        });
    }

    /// Runs one `guard explain` call in `repo` off the UI thread with the
    /// explain child environment ([`ai_settings::child_env`]): the saved key
    /// is read on the worker thread, goes through the environment only, and
    /// is redacted from the captured output.
    fn spawn_explain_child(
        &mut self,
        ctx: &egui::Context,
        guard: PathBuf,
        command: GuardCommand,
        repo: PathBuf,
    ) -> Receiver<Result<Captured, String>> {
        self.sync_key_saved();
        let settings = self.ai.clone();
        let host = self.ai_key_host.clone();
        let use_saved_key = self.ai_key_saved;
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let secret = if use_saved_key {
                crate::credentials::load(&host).ok().flatten()
            } else {
                None
            };
            let env = ai_settings::child_env(&settings, secret.as_deref());
            let out =
                runner::capture_with_env(&guard, &command.args(), &env, &repo, EXPLAIN_TIMEOUT)
                    .map(|mut c| {
                        c.stdout = ai_settings::redact(&c.stdout, secret.as_deref());
                        c.stderr = ai_settings::redact(&c.stderr, secret.as_deref());
                        c
                    })
                    .map_err(|e| format!("{}: {e}", command.display()));
            drop(env);
            let _ = tx.send(out);
            repaint.request_repaint();
        });
        rx
    }

    /// Starts `guard explain --format <md|xlsx> --out <path> [--journey <id>]`
    /// for `key` off the UI thread, with the same child environment as
    /// [`Self::start_explain`]. The file goes to
    /// `<repo>/.guard/explain/explain-<stamp>.<ext>` ([`crate::export`]).
    fn start_export(&mut self, ctx: &egui::Context, key: String, format: ExportFormat) {
        if self.export_job.is_some() {
            return;
        }
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            return;
        };
        let Ok(guard) = self.guard.clone() else {
            self.exports.insert(
                key,
                ExportState::Error(
                    "guard binary not found: set GUARD_BIN or add it to PATH.".into(),
                ),
            );
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = crate::export::export_path(&repo, format, now);
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            self.exports
                .insert(key, ExportState::Error(format!("{}: {e}", dir.display())));
            return;
        }
        let command = GuardCommand::ExplainExport {
            journey: (key != ALL_FAILURES).then(|| key.clone()),
            format,
            out: path.clone(),
        };
        self.exports
            .insert(key.clone(), ExportState::Running(format));
        let rx = self.spawn_explain_child(ctx, guard, command, repo);
        self.export_job = Some(ExportJob {
            key,
            epoch: self.epoch,
            path,
            rx,
        });
    }

    /// Starts `guard explain --list-models --json` for `key` off the UI
    /// thread, with the same child environment as explain: the saved key
    /// goes through the environment only, and is redacted from the output.
    pub(super) fn start_model_fetch(&mut self, ctx: &egui::Context, key: FetchKey) {
        self.models.started(&key);
        let Ok(guard) = self.guard.clone() else {
            self.models.finished(
                &key,
                Err("guard binary not found: set GUARD_BIN or add it to PATH.".into()),
            );
            return;
        };
        let cwd = self
            .repo()
            .map_or_else(|| self.cwd.clone(), Path::to_path_buf);
        let settings = self.ai.clone();
        let host = self.ai_key_host.clone();
        let use_saved_key = self.ai_key_saved;
        let command = GuardCommand::ListModels;
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let secret = if use_saved_key {
                crate::credentials::load(&host).ok().flatten()
            } else {
                None
            };
            let env = ai_settings::child_env(&settings, secret.as_deref());
            let out = runner::capture_with_env(&guard, &command.args(), &env, &cwd, PROBE_TIMEOUT)
                .map(|mut c| {
                    c.stdout = ai_settings::redact(&c.stdout, secret.as_deref());
                    c.stderr = ai_settings::redact(&c.stderr, secret.as_deref());
                    c
                })
                .map_err(|e| format!("{}: {e}", command.display()));
            drop(env);
            let _ = tx.send(out);
            repaint.request_repaint();
        });
        self.models_job = Some(ModelsJob { key, rx });
    }

    /// Re-reads whether a key is saved when the endpoint host changed
    /// (another provider, an edited base URL); a no-op otherwise.
    pub(super) fn sync_key_saved(&mut self) {
        let host = self.ai.host();
        if host != self.ai_key_host {
            self.ai_key_saved = self.shot.is_none() && key_saved_for(&host);
            self.ai_key_host = host;
        }
    }

    /// Abandons the in-flight explain for `journey`: the epoch/key guard drops
    /// the late result, and the panel returns to its button.
    fn cancel_explain(&mut self, journey: &str) {
        if self.explain_job.as_ref().is_some_and(|j| j.key == journey) {
            self.explain_job = None;
        }
        self.explain.remove(journey);
    }

    /// Takes new run output and finished background work; cheap when nothing
    /// changed. Anything tagged with an older epoch is dropped unread.
    fn drain(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.folder_rx
            && let Ok(picked) = rx.try_recv()
        {
            self.folder_rx = None;
            if let Some(path) = picked {
                self.open_project(ctx, path, true);
            }
        }
        if let Some((epoch, rx)) = &self.branch_rx
            && let Ok(branch) = rx.try_recv()
        {
            if *epoch == self.epoch {
                self.branch = branch;
            }
            self.branch_rx = None;
        }
        let mut exited = None;
        if let Some(active) = &self.run {
            let testing = active.command.is_test();
            while let Ok(event) = active.run.events().try_recv() {
                match event {
                    RunEvent::Line(line) => {
                        if testing {
                            self.run_progress.feed(&line);
                        }
                        self.log.push(line)
                    }
                    RunEvent::Exited(exit) => exited = Some(exit),
                }
            }
        }
        if let Some(exit) = exited
            && let Some(active) = self.run.take()
        {
            self.log.push(format!("[{exit}]"));
            let command = active.command;
            self.last_exit = Some((command.display(), exit));
            if active.epoch == self.epoch {
                match command {
                    ref command if command.is_test() => {
                        let fresh = self.repo().is_some_and(|repo| {
                            results::report_is_fresh(
                                active.report_before,
                                results::report_stamp(repo),
                            )
                        });
                        self.reload_results();
                        if !fresh && self.results_note.is_none() {
                            self.results_note = Some(
                                "This run wrote no new report.json: the results shown are from an earlier run."
                                    .into(),
                            );
                        }
                        self.record_test_verdict(exit, fresh);
                        let (failed, flaky) = results::run_counts(fresh, &self.results);
                        self.auto_summarize(ctx, exit == Exit::Cancelled, failed + flaky);
                    }
                    GuardCommand::Up { .. } | GuardCommand::Down => {
                        self.start_probe(ctx, ProbeKind::Status);
                        // The local app's readiness changed.
                        self.refresh_integrations(ctx);
                    }
                    _ => {}
                }
            }
        }
        let finished = self
            .probe
            .as_ref()
            .and_then(|p| p.rx.try_recv().ok().map(|out| (p.kind, p.epoch, out)));
        if let Some((kind, epoch, out)) = finished {
            self.probe = None;
            if epoch == self.epoch {
                self.probe_done(ctx, kind, out);
            }
        }
        // Run affected waited for a busy probe slot: refresh the preview now.
        if self.gate_affected.is_some() && self.probe.is_none() {
            self.start_probe(ctx, ProbeKind::Affected);
        }
        if let Some(job) = &self.models_job
            && let Ok(out) = job.rx.try_recv()
        {
            let key = job.key.clone();
            self.models_job = None;
            self.models.finished(&key, parse_models(out));
        }
        let explain_finished = self.explain_job.as_ref().and_then(|j| {
            j.rx.try_recv()
                .ok()
                .map(|out| (j.key.clone(), j.epoch, out))
        });
        if let Some((key, epoch, out)) = explain_finished {
            self.explain_job = None;
            if epoch == self.epoch {
                let state = match parse_explain(out) {
                    Ok(summary) => ExplainState::Done(summary),
                    Err(e) => ExplainState::Error(e),
                };
                self.explain.insert(key, state);
            }
        }
        let export_finished = self.export_job.as_ref().and_then(|j| {
            j.rx.try_recv()
                .ok()
                .map(|out| (j.key.clone(), j.epoch, j.path.clone(), out))
        });
        if let Some((key, epoch, path, out)) = export_finished {
            self.export_job = None;
            if epoch == self.epoch {
                let state = match parse_export(out, &path, Path::is_file) {
                    Ok(path) => ExportState::Done(path),
                    Err(e) => ExportState::Error(e),
                };
                self.exports.insert(key, state);
            }
        }
        if self.auto_summary_queued && self.explain_job.is_none() {
            self.auto_summary_queued = false;
            self.auto_summary_hint = None;
            self.start_explain(ctx, ALL_FAILURES.to_string());
        }
        if let Some(job) = &self.suspects_job
            && let Ok(out) = job.rx.try_recv()
        {
            let (stamp, epoch) = (job.stamp, job.epoch);
            self.suspects_job = None;
            if epoch == self.epoch {
                self.suspects = Some((stamp, parse_suspects(out)));
            }
        }
        self.refresh_suspects(ctx);
        self.drain_integrations();
    }

    /// After a test run: start "Explain failures" when the run ended with
    /// failures and the setting is on ([`ai_settings::auto_summary`]), and
    /// show it in the Problems tab; without a key, a hint instead.
    fn auto_summarize(&mut self, ctx: &egui::Context, cancelled: bool, failures: usize) {
        self.auto_summary_hint = None;
        self.auto_summary_queued = false;
        self.sync_key_saved();
        let busy = self.explain_job.is_some();
        match ai_settings::auto_summary(&self.ai, self.key_source(), cancelled, failures, busy) {
            ai_settings::AutoSummary::Start => {
                self.start_explain(ctx, ALL_FAILURES.to_string());
                self.bottom_open = true;
                self.bottom_tab = BottomTab::Problems;
            }
            ai_settings::AutoSummary::Queue => {
                // Started from drain once the explanation in flight is done.
                self.auto_summary_queued = true;
                self.auto_summary_hint = Some(
                    "An explanation is still running; this run is summarized when it finishes."
                        .into(),
                );
                self.bottom_open = true;
                self.bottom_tab = BottomTab::Problems;
            }
            ai_settings::AutoSummary::NeedsKey => {
                self.auto_summary_hint = Some(format!(
                    "Not summarized automatically: no API key for {}. Save one in AI settings.",
                    self.ai.host()
                ));
            }
            ai_settings::AutoSummary::Skip => {}
        }
    }

    /// Starts `guard suspects --json` off the UI thread when the report has
    /// failures and changed since the suspects were last read.
    fn refresh_suspects(&mut self, ctx: &egui::Context) {
        if self.suspects_job.is_some() || self.running() {
            return;
        }
        if self
            .suspects
            .as_ref()
            .is_some_and(|(stamp, _)| *stamp == self.report_stamp)
        {
            return;
        }
        let Some(repo) = self.repo().map(Path::to_path_buf) else {
            return;
        };
        let Ok(guard) = self.guard.clone() else {
            return;
        };
        if self.problem_ids().is_empty() {
            return;
        }
        let command = GuardCommand::Suspects;
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let out = runner::capture(&guard, &command.args(), &repo, PROBE_TIMEOUT)
                .map_err(|e| format!("{}: {e}", command.display()));
            let _ = tx.send(out);
            repaint.request_repaint();
        });
        self.suspects_job = Some(SuspectsJob {
            stamp: self.report_stamp,
            epoch: self.epoch,
            rx,
        });
    }

    fn probe_done(&mut self, ctx: &egui::Context, kind: ProbeKind, out: Result<Captured, String>) {
        let parsed = out.and_then(|captured| {
            let text = captured.stdout.trim();
            if text.is_empty() {
                Err(error_lines(&captured))
            } else {
                Ok(text.to_string())
            }
        });
        match kind {
            ProbeKind::Status => {
                self.status = Some(parsed.and_then(|t| status::parse_status(&t)));
            }
            ProbeKind::Affected => {
                self.affected = Some(parsed.and_then(|t| status::parse_affected(&t)));
                if let Some(mode) = self.gate_affected.take() {
                    self.gate_suite(ctx, GuardCommand::TestAffected(mode));
                }
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut confirmed = None;
        if let Some(pending) = &self.pending {
            let mut close = false;
            let response = egui::Modal::new(egui::Id::new("safety-confirm")).show(ctx, |ui| {
                ui.set_max_width(480.0);
                ui.heading("Confirm run");
                ui.add_space(4.0);
                ui.label(RichText::new(pending.command.display()).monospace());
                if !pending.journeys.is_empty() {
                    ui.add_space(4.0);
                    ui.label(
                        "These journeys are not classified as safe (contained or unclassified):",
                    );
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for id in &pending.journeys {
                                ui.label(
                                    RichText::new(format!("- {id}"))
                                        .monospace()
                                        .color(theme::TEXT_SECONDARY),
                                );
                            }
                        });
                }
                if let Some(note) = &pending.note {
                    ui.label(RichText::new(note).color(theme::AT_RISK));
                }
                ui.label("They may write data or call real services. Run anyway?");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if widgets::run_button(ui, "Run", true).clicked() {
                        confirmed = Some(pending.command.clone());
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close || response.should_close() {
                self.pending = None;
            }
        }
        if let Some(command) = confirmed {
            self.start(ctx, command);
        }
        let mut switch = None;
        if let Some((root, _)) = &self.pending_switch {
            let mut close = false;
            let response = egui::Modal::new(egui::Id::new("switch-confirm")).show(ctx, |ui| {
                ui.set_max_width(460.0);
                ui.heading("Switch project?");
                ui.add_space(4.0);
                ui.label(
                    "A run is in progress. Switching cancels it, and its results are dropped.",
                );
                ui.label(RichText::new(root.display().to_string()).monospace());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel run and switch").clicked() {
                        switch = Some(());
                        close = true;
                    }
                    if ui.button("Keep the current project").clicked() {
                        close = true;
                    }
                });
            });
            if switch.is_some()
                && let Some((root, validation)) = self.pending_switch.take()
            {
                self.switch_to(ctx, root, validation);
            } else if close || response.should_close() {
                self.pending_switch = None;
            }
        }
        if let Some(notice) = &self.notice {
            let mut close = false;
            let response = egui::Modal::new(egui::Id::new("notice")).show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(notice);
                ui.add_space(6.0);
                if ui.button("OK").clicked() {
                    close = true;
                }
            });
            if close || response.should_close() {
                self.notice = None;
            }
        }
    }

    /// The screenshot harness: wait a few frames (and the branch), ask the
    /// viewport for a screenshot, save it and close.
    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else {
            return;
        };
        // Only this harness repaints continuously, and only until it exits.
        ctx.request_repaint();
        shot.frames += 1;
        let elapsed = shot.started.elapsed();
        let settled = self.branch_rx.is_none() || elapsed > Duration::from_secs(3);
        if !shot.requested && shot.frames >= 5 && settled {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, pixels) {
                Some(img) => match img.save(&shot.path) {
                    Ok(()) => eprintln!("saved {}", shot.path.display()),
                    Err(e) => eprintln!("error: cannot save {}: {e}", shot.path.display()),
                },
                None => eprintln!("error: bad screenshot buffer"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if elapsed > Duration::from_secs(30) {
            eprintln!("error: no screenshot after 30 s");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// The summary string from `guard explain --json` stdout, or a readable error.
fn parse_explain(out: Result<Captured, String>) -> Result<String, String> {
    let captured = out?;
    let text = captured.stdout.trim();
    if text.is_empty() {
        return Err(error_lines(&captured));
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("bad explain output: {e}"))?;
    value["summary"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "explain output had no summary".to_string())
}

/// The file `guard explain --format <md|xlsx> --out <path>` wrote, or a
/// readable error: it must exit 0 and leave the file behind (`exists`).
fn parse_export(
    out: Result<Captured, String>,
    path: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    let captured = out?;
    if captured.code != Some(0) {
        return Err(error_lines(&captured));
    }
    if !exists(path) {
        return Err(format!("guard explain wrote no {}", path.display()));
    }
    Ok(path.to_path_buf())
}

/// The suspects from `guard suspects --json`, or a readable error.
fn parse_suspects(out: Result<Captured, String>) -> Result<SuspectsDoc, String> {
    let captured = out?;
    if captured.code != Some(0) || captured.stdout.trim().is_empty() {
        return Err(error_lines(&captured));
    }
    crate::suspects::parse(&captured.stdout)
}

/// The models from `guard explain --list-models --json`, or a readable error.
fn parse_models(out: Result<Captured, String>) -> Result<Vec<models::ModelInfo>, String> {
    let captured = out?;
    if captured.code != Some(0) || captured.stdout.trim().is_empty() {
        return Err(error_lines(&captured));
    }
    models::parse_list(&captured.stdout)
}

/// The `error:`/`hint:` lines of a failed probe, or its whole stderr.
fn error_lines(captured: &Captured) -> String {
    let lines: Vec<&str> = captured
        .stderr
        .lines()
        .filter(|l| l.starts_with("error:") || l.starts_with("hint:"))
        .collect();
    let detail = if lines.is_empty() {
        captured.stderr.trim().to_string()
    } else {
        lines.join("\n")
    };
    match captured.code {
        Some(code) => format!("guard exited {code}: {detail}"),
        None => format!("guard was killed: {detail}"),
    }
}

impl eframe::App for GuardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain(ctx);
        self.poll_integrations(ctx);
        self.toolbar(ctx);
        self.status_bar(ctx);
        self.stripe(ctx);
        if self.project.is_none() {
            self.welcome(ctx);
        } else {
            if self.bottom_open {
                self.tool_window(ctx);
            }
            match self.view {
                View::Tests => self.tests_screen(ctx),
                View::Run => self.run_screen(ctx),
                View::Flow => self.flow_screen(ctx),
            }
        }
        self.dialogs(ctx);
        self.ai_settings_window(ctx);
        self.sync_key_saved();
        self.screenshot(ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.shot.is_some() {
            return;
        }
        eframe::set_value(storage, RECENTS_KEY, &self.recents);
        let layout = Layout {
            view: self.view,
            bottom_open: self.bottom_open,
            bottom_tab: self.bottom_tab,
            up_timeout: self.up_timeout,
            run_mode: self.run_mode,
        };
        eframe::set_value(storage, LAYOUT_KEY, &layout);
        eframe::set_value(storage, AI_SETTINGS_KEY, &self.ai);
        eframe::set_value(storage, INTEGRATIONS_KEY, &self.integration_settings);
    }

    fn persist_egui_memory(&self) -> bool {
        self.shot.is_none()
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::GROUND.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Never leave Playwright and its browsers behind.
        if let Some(active) = &self.run {
            active.run.cancel();
        }
    }
}

/// Whether the Credential Manager holds a non-blank key for `host`.
fn key_saved_for(host: &str) -> bool {
    matches!(crate::credentials::load(host), Ok(Some(k)) if !k.trim().is_empty())
}
