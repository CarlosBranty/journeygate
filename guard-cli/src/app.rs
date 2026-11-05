//! `guard up` / `guard down` procedures, shared with `guard gate shard`.
//!
//! `[app].start` and every `[[services]]` entry run detached in their own
//! process group; each records its PID and start identity under `.guard/`
//! so `guard down` only ever terminates what `guard up` started.

use crate::context::Context;
use crate::error::{CliError, CliResult, warning};
use crate::http;
use crate::process;
use guard_core::config::{Service, TargetConfig};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

pub struct UpOutcome {
    pub already_ready: bool,
    /// True when this call spawned `[app].start` or a service.
    pub started: bool,
    /// True when this call spawned `[app].start`.
    pub started_app: bool,
    pub elapsed: Duration,
    /// What happened to each `[[services]]` entry, in file order.
    pub services: Vec<ServiceUp>,
}

pub struct ServiceUp {
    pub name: String,
    pub state: ServiceState,
}

pub enum ServiceState {
    /// Its `ready_url` already answered 200; it was not started.
    AlreadyReady,
    /// Started and ready at `url` after `elapsed`.
    Ready { url: String, elapsed: Duration },
    /// Started; it has no `ready_url` to wait for.
    Started,
    /// The process an earlier `guard up` recorded for it is still running
    /// (and its `ready_url`, if any, answered); it was not started again.
    Running,
}

impl ServiceState {
    /// True when this call spawned the service.
    pub fn spawned(&self) -> bool {
        matches!(self, ServiceState::Ready { .. } | ServiceState::Started)
    }
}

/// Where one process `guard up` started is recorded: `<dir>/<stem>.{pid,identity,log}`.
struct Slot {
    dir: PathBuf,
    stem: String,
    /// `.guard/app` or `.guard/services/<name>`, for messages.
    shown: String,
}

impl Slot {
    fn app(ctx: &Context) -> Self {
        Slot {
            dir: ctx.guard_dir(),
            stem: "app".into(),
            shown: ".guard/app".into(),
        }
    }

    fn service(ctx: &Context, name: &str) -> Self {
        Slot {
            dir: ctx.guard_dir().join("services"),
            stem: name.into(),
            shown: format!(".guard/services/{name}"),
        }
    }

    fn path(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.stem))
    }

    fn shown_dir(&self) -> &str {
        self.shown
            .rsplit_once('/')
            .map_or(self.shown.as_str(), |(dir, _)| dir)
    }

    /// Spawns `command` from `cwd` through the platform shell, detached, with
    /// output appended to the slot's log, and records its PID.
    fn spawn(&self, command: &str, cwd: &Path, what: &str) -> CliResult<Child> {
        fs::create_dir_all(&self.dir)
            .map_err(|e| CliError::env(format!("cannot create {}/: {e}", self.shown_dir())))?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("log"))
            .map_err(|e| CliError::env(format!("cannot open {}.log: {e}", self.shown)))?;
        let log_err = log.try_clone().map_err(|e| CliError::env(e.to_string()))?;
        let mut cmd = process::shell(command);
        cmd.current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);
        process::detach(&mut cmd);
        hide_console(&mut cmd);
        seal_std_handles();
        let spawned = cmd
            .spawn()
            .map_err(|e| CliError::env(format!("cannot start {what}: {e}")))?;
        self.write_pid(spawned.id())?;
        Ok(spawned)
    }

    /// `<stem>.pid` (decimal PID) plus `<stem>.identity`, the process start
    /// time `guard down` checks before it terminates that PID.
    fn write_pid(&self, pid: u32) -> CliResult<()> {
        let identity_path = self.path("identity");
        let _ = fs::remove_file(&identity_path);
        fs::write(self.path("pid"), pid.to_string())
            .map_err(|e| CliError::env(format!("cannot write {}.pid: {e}", self.shown)))?;
        if let Some(identity) = process::start_identity(pid) {
            fs::write(&identity_path, identity)
                .map_err(|e| CliError::env(format!("cannot write {}.identity: {e}", self.shown)))?;
        }
        Ok(())
    }

    /// True when the recorded PID is still the process `guard up` started
    /// (same identity rule as `terminate_recorded`).
    fn recorded_alive(&self) -> bool {
        let Some(pid) = fs::read_to_string(self.path("pid"))
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
        else {
            return false;
        };
        let recorded = fs::read_to_string(self.path("identity")).ok();
        let current = process::start_identity(pid);
        current.is_some()
            && process::same_process(recorded.as_deref().map(str::trim), current.as_deref())
    }

    fn clear(&self) {
        let _ = fs::remove_file(self.path("pid"));
        let _ = fs::remove_file(self.path("identity"));
    }

    fn log_tail(&self) -> Vec<String> {
        fs::read_to_string(self.path("log"))
            .map(|t| process::tail(&t, 20))
            .unwrap_or_default()
    }

    /// Terminates the recorded PID when its identity still matches, then
    /// deletes the PID files. True when a PID file existed.
    fn terminate_recorded(&self) -> bool {
        let Ok(text) = fs::read_to_string(self.path("pid")) else {
            return false;
        };
        if let Ok(pid) = text.trim().parse::<u32>() {
            let recorded = fs::read_to_string(self.path("identity")).ok();
            let recorded = recorded.as_deref().map(str::trim);
            if process::same_process(recorded, process::start_identity(pid).as_deref()) {
                process::kill_tree(pid);
            } else {
                warning(format!(
                    "PID {pid} in {}.pid is no longer the process guard up started; not terminating it",
                    self.shown
                ));
            }
        }
        self.clear();
        true
    }
}

/// A process this `guard up` call spawned and still owns.
struct Spawned {
    slot: Slot,
    child: Option<Child>,
}

impl Spawned {
    /// The exit status once the process has exited; its PID files are then
    /// cleared, because a later `guard down` must never kill whatever reuses it.
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.as_mut()?.try_wait().ok()??;
        self.child = None;
        self.slot.clear();
        Some(status)
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            process::kill_tree(child.id());
            let _ = child.wait();
            self.slot.clear();
        }
    }
}

/// Terminates what this call spawned, newest first.
fn abort(services: &mut [Spawned], app: &mut Spawned) {
    for spawned in services.iter_mut().rev() {
        spawned.terminate();
    }
    app.terminate();
}

fn probe(url: &str) -> bool {
    http::is_ready(url, Duration::from_secs(2)).is_ok()
}

pub fn up(ctx: &Context, config: &TargetConfig, timeout: Duration) -> CliResult<UpOutcome> {
    let url = config.ready_url();
    let begin = Instant::now();
    let services_ready = || {
        config
            .services
            .iter()
            .all(|s| s.ready_url.as_deref().is_none_or(probe))
    };
    if probe(&url) && services_ready() {
        return Ok(UpOutcome {
            already_ready: true,
            started: false,
            started_app: false,
            elapsed: begin.elapsed(),
            services: Vec::new(),
        });
    }

    let mut app = Spawned {
        slot: Slot::app(ctx),
        child: None,
    };
    // With [[services]], `[app].start` is skipped while the base URL is
    // served or the process an earlier call recorded still runs: spawning it
    // again would orphan that process. Without services: unchanged.
    let app_running = !config.services.is_empty() && (probe(&url) || app.slot.recorded_alive());
    if let Some(start) = config.app.start.as_ref().filter(|_| !app_running) {
        app.child = Some(app.slot.spawn(start, &ctx.root, "[app].start")?);
    }
    let started_app = app.child.is_some();
    let mut spawned: Vec<Spawned> = Vec::new();
    let mut outcomes = Vec::new();
    if !config.services.is_empty() {
        if app.child.is_some() {
            if let Err(e) = wait_for_launcher(&mut app, &url, timeout) {
                abort(&mut spawned, &mut app);
                return Err(e);
            }
        }
        for service in &config.services {
            match start_service(ctx, service, timeout, &mut spawned) {
                Ok(state) => outcomes.push(ServiceUp {
                    name: service.name.clone(),
                    state,
                }),
                Err(e) => {
                    abort(&mut spawned, &mut app);
                    return Err(e);
                }
            }
        }
    }

    let result = http::wait_ready(&url, Instant::now() + timeout, || {
        // A launcher such as `docker compose up -d --wait` may exit 0 before readiness.
        let status = app.exited()?;
        if status.success() {
            None
        } else {
            Some(format!("[app].start exited with {status}"))
        }
    });
    match result {
        Ok(()) => Ok(UpOutcome {
            already_ready: false,
            started: started_app || !spawned.is_empty(),
            started_app,
            elapsed: begin.elapsed(),
            services: outcomes,
        }),
        Err(reason) => {
            abort(&mut spawned, &mut app);
            let message = if reason.starts_with("[app].start") {
                reason
            } else {
                format!(
                    "app not ready at {url} after {}s: {reason}",
                    timeout.as_secs()
                )
            };
            Err(CliError::env(message).with_messages(app.slot.log_tail()))
        }
    }
}

/// With `[[services]]`, `[app].start` (typically the compose infrastructure)
/// must exit 0 or serve the base URL before the first service starts.
fn wait_for_launcher(app: &mut Spawned, url: &str, timeout: Duration) -> CliResult<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = app.exited() {
            if status.success() {
                return Ok(());
            }
            return Err(CliError::env(format!("[app].start exited with {status}"))
                .with_messages(app.slot.log_tail()));
        }
        if probe(url) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(CliError::env(format!(
                "[app].start did not finish within {}s; with [[services]] it must exit (like `docker compose up -d --wait`) or serve {url}",
                timeout.as_secs()
            ))
            .with_messages(app.slot.log_tail()));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn start_service(
    ctx: &Context,
    service: &Service,
    timeout: Duration,
    spawned: &mut Vec<Spawned>,
) -> CliResult<ServiceState> {
    let name = &service.name;
    if service.ready_url.as_deref().is_some_and(probe) {
        return Ok(ServiceState::AlreadyReady);
    }
    let cwd = ctx.root.join(&service.cwd);
    if !cwd.is_dir() {
        return Err(CliError::env(format!(
            "service {name}: cwd {} is not a directory",
            service.cwd
        )));
    }
    let slot = Slot::service(ctx, name);
    if slot.recorded_alive() {
        // Spawning it again would orphan the recorded process.
        let Some(url) = &service.ready_url else {
            return Ok(ServiceState::Running);
        };
        return match http::wait_ready(url, Instant::now() + timeout, || None) {
            Ok(()) => Ok(ServiceState::Running),
            Err(reason) => Err(CliError::env(format!(
                "service {name} is still running from an earlier guard up (.guard/services/{name}.pid) but not ready at {url} after {}s: {reason}; run guard down first",
                timeout.as_secs()
            ))
            .with_messages(slot.log_tail())),
        };
    }
    let begin = Instant::now();
    let child = slot.spawn(&service.start, &cwd, &format!("service {name}"))?;
    spawned.push(Spawned {
        slot,
        child: Some(child),
    });
    let Some(url) = &service.ready_url else {
        return Ok(ServiceState::Started);
    };
    let current = spawned.last_mut().expect("just pushed");
    let result = http::wait_ready(url, Instant::now() + timeout, || {
        let status = current.exited()?;
        (!status.success()).then(|| format!("service {name} start exited with {status}"))
    });
    match result {
        Ok(()) => Ok(ServiceState::Ready {
            url: url.clone(),
            elapsed: begin.elapsed(),
        }),
        Err(reason) => {
            let message = if reason.starts_with("service ") {
                reason
            } else {
                format!(
                    "service {name} not ready at {url} after {}s: {reason}",
                    timeout.as_secs()
                )
            };
            Err(CliError::env(message).with_messages(current.slot.log_tail()))
        }
    }
}

/// Windows: the app gets a hidden console of its own (`CREATE_NO_WINDOW`)
/// instead of sharing guard's, so closing guard's terminal does not take it
/// down and it never pops a window; it stays its own process group.
fn hide_console(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Makes guard's own stdin/stdout/stderr handles non-inheritable (once per
/// process; a no-op off Windows).
///
/// std spawns every Windows child with `bInheritHandles = TRUE`, so each
/// child, and the detached app's whole tree, also received guard's stdout
/// pipe even with its stdio redirected: `guard up | tail` (or a test reading
/// guard's output) then never saw EOF while the app ran. Children still get
/// the stdio std hands them explicitly: std duplicates those handles,
/// inheritable, for each spawn (`Stdio::inherit()` included).
pub(crate) fn seal_std_handles() {
    #[cfg(windows)]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            type Handle = *mut std::ffi::c_void;
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn GetStdHandle(std_handle: u32) -> Handle;
                fn SetHandleInformation(handle: Handle, mask: u32, flags: u32) -> i32;
            }
            const STD_INPUT_HANDLE: u32 = -10i32 as u32;
            const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
            const STD_ERROR_HANDLE: u32 = -12i32 as u32;
            const HANDLE_FLAG_INHERIT: u32 = 0x1;
            for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: GetStdHandle has no preconditions; SetHandleInformation
                // only flips the inherit flag of a handle this process owns, and
                // tolerates null/invalid handles by failing, which is ignored.
                unsafe {
                    let handle = GetStdHandle(id);
                    if !handle.is_null() && handle as isize != -1 {
                        SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                    }
                }
            }
        });
    }
}

pub struct DownOutcome {
    pub stopped: bool,
}

pub fn down(ctx: &Context, config: &TargetConfig) -> CliResult<DownOutcome> {
    down_selected(ctx, config, |_| true, true)
}

/// Stops only what `outcome` says that `up` call spawned (used by
/// `guard gate shard`, which must leave already-running processes alone).
pub fn down_started(
    ctx: &Context,
    config: &TargetConfig,
    outcome: &UpOutcome,
) -> CliResult<DownOutcome> {
    let spawned: Vec<&str> = outcome
        .services
        .iter()
        .filter(|s| s.state.spawned())
        .map(|s| s.name.as_str())
        .collect();
    down_selected(
        ctx,
        config,
        |name| spawned.contains(&name),
        outcome.started_app,
    )
}

fn down_selected(
    ctx: &Context,
    config: &TargetConfig,
    service_selected: impl Fn(&str) -> bool,
    app_selected: bool,
) -> CliResult<DownOutcome> {
    let mut stopped = false;
    let mut errors: Vec<CliError> = Vec::new();
    for service in config
        .services
        .iter()
        .rev()
        .filter(|s| service_selected(&s.name))
    {
        let name = &service.name;
        if let Some(stop) = &service.stop {
            let status = process::shell(stop)
                .current_dir(ctx.root.join(&service.cwd))
                .status();
            match status {
                Ok(status) if status.success() => stopped = true,
                Ok(status) => errors.push(CliError::env(format!(
                    "service {name} stop exited with {status}"
                ))),
                Err(e) => errors.push(CliError::env(format!(
                    "cannot run service {name} stop: {e}"
                ))),
            }
        }
        // Always reap what `guard up` started, even when `stop` failed.
        stopped |= Slot::service(ctx, name).terminate_recorded();
    }
    if !app_selected {
        let mut errors = errors.into_iter();
        return match errors.next() {
            Some(first) => Err(first.with_messages(errors.flat_map(|e| e.messages))),
            None => Ok(DownOutcome { stopped }),
        };
    }
    if let Some(stop) = &config.app.stop {
        let status = process::shell(stop)
            .current_dir(&ctx.root)
            .status()
            .map_err(|e| CliError::env(format!("cannot run [app].stop: {e}")));
        match status {
            Ok(status) if status.success() => stopped = true,
            Ok(status) => errors.push(CliError::env(format!("[app].stop exited with {status}"))),
            Err(e) => errors.push(e),
        }
    }
    // Always reap what `guard up` started, even when `[app].stop` failed.
    stopped |= Slot::app(ctx).terminate_recorded();
    let mut errors = errors.into_iter();
    match errors.next() {
        Some(first) => Err(first.with_messages(errors.flat_map(|e| e.messages))),
        None => Ok(DownOutcome { stopped }),
    }
}
