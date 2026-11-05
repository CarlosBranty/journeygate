//! Running `guard` as a child process.
//!
//! [`Run`] streams stdout and stderr lines through a channel while the process
//! runs and can be cancelled (the whole process tree: Playwright and its
//! browsers too). [`capture`] runs a short command to completion. On Windows
//! neither opens a console window.

use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests;

/// How long the exit waits for the last buffered lines. A detached grandchild
/// (for example a service started by `guard up`) may keep a pipe open forever.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// How long a probe (`guard status --json`, `guard affected --json`, `git
/// rev-parse`) may take before it is killed and reported.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    /// Ended by a signal, without an exit code.
    Killed,
    /// Stopped by the user's Cancel, whatever code the kill left behind
    /// (`taskkill /F` makes it exit 1 on Windows).
    Cancelled,
}

impl Exit {
    pub fn success(self) -> bool {
        self == Self::Code(0)
    }

    /// The capitalised form the window shows.
    pub fn label(self) -> String {
        match self {
            Self::Code(code) => format!("Exit {code}"),
            Self::Killed => "Killed".into(),
            Self::Cancelled => "Cancelled".into(),
        }
    }
}

/// The exit of a finished child: a Cancel wins over its exit code.
pub fn classify(code: Option<i32>, cancelled: bool) -> Exit {
    match (cancelled, code) {
        (true, _) => Exit::Cancelled,
        (false, Some(code)) => Exit::Code(code),
        (false, None) => Exit::Killed,
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(code) => write!(f, "exit {code}"),
            Self::Killed => write!(f, "killed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Line(String),
    /// Always the last event.
    Exited(Exit),
}

/// One running child process.
pub struct Run {
    pid: u32,
    events: Receiver<RunEvent>,
    finished: Arc<Mutex<bool>>,
    cancelled: Arc<AtomicBool>,
}

impl Run {
    /// Spawns `program args` in `cwd`; `notify` is called after every event
    /// (the GUI passes a repaint request, so it repaints only on new output).
    pub fn spawn(
        program: &Path,
        args: &[String],
        cwd: &Path,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let mut cmd = command(program, args, cwd);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        new_process_group(&mut cmd);
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let (tx, events) = mpsc::channel();
        let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(notify);
        let (drained_tx, drained) = mpsc::channel();
        if let Some(out) = child.stdout.take() {
            pump(out, tx.clone(), notify.clone(), drained_tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, tx.clone(), notify.clone(), drained_tx);
        }
        let finished = Arc::new(Mutex::new(false));
        let done = finished.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let was_cancelled = cancelled.clone();
        thread::spawn(move || {
            let code = child.wait().ok().and_then(|status| status.code());
            let exit = classify(code, was_cancelled.load(Ordering::SeqCst));
            // Both pumps report once; a pipe held by a grandchild never closes.
            for _ in 0..2 {
                if drained.recv_timeout(DRAIN_GRACE).is_err() {
                    break;
                }
            }
            if let Ok(mut done) = done.lock() {
                *done = true;
            }
            let _ = tx.send(RunEvent::Exited(exit));
            notify();
        });
        Ok(Self {
            pid,
            events,
            finished,
            cancelled,
        })
    }

    pub fn events(&self) -> &Receiver<RunEvent> {
        &self.events
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Kills the process tree. A no-op once the process has exited, so a
    /// reused PID is never touched.
    pub fn cancel(&self) {
        let finished = self.finished.lock().map(|f| *f).unwrap_or(true);
        if !finished {
            self.cancelled.store(true, Ordering::SeqCst);
            kill_tree(self.pid);
        }
    }
}

fn pump(
    stream: impl Read + Send + 'static,
    tx: Sender<RunEvent>,
    notify: Arc<dyn Fn() + Send + Sync>,
    drained: Sender<()>,
) {
    thread::spawn(move || {
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end_matches(['\n', '\r']).to_string();
                    if tx.send(RunEvent::Line(line)).is_err() {
                        break;
                    }
                    notify();
                }
            }
        }
        let _ = drained.send(());
    });
}

/// Output of a command run to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("timed out after {} s and was stopped", .0.as_secs())]
    TimedOut(Duration),
}

/// Runs `program args` in `cwd` to completion (for short read-only commands
/// such as `guard status --json`). Past `timeout` the process tree is killed
/// and [`CaptureError::TimedOut`] is returned.
pub fn capture(
    program: &Path,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<Captured, CaptureError> {
    capture_with_env(program, args, &[], cwd, timeout)
}

/// [`capture`] with extra variables set on the child only (on top of the
/// inherited environment). Secrets such as the explain API key travel this
/// way: never in `args`, which other processes can read.
pub fn capture_with_env(
    program: &Path,
    args: &[String],
    env: &[(String, String)],
    cwd: &Path,
    timeout: Duration,
) -> Result<Captured, CaptureError> {
    let mut cmd = command(program, args, cwd);
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    new_process_group(&mut cmd);
    let mut child = cmd.spawn()?;
    let stdout = read_all(child.stdout.take());
    let stderr = read_all(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let now = Instant::now();
        if now >= deadline {
            kill_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(25)));
    };
    let Some(status) = status else {
        return Err(CaptureError::TimedOut(timeout));
    };
    // A grandchild holding a pipe must not hang the probe either.
    let text = |rx: Receiver<Vec<u8>>| {
        let bytes = rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(Captured {
        code: status.code(),
        stdout: text(stdout),
        stderr: text(stderr),
    })
}

/// Reads a whole pipe on its own thread.
fn read_all(stream: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut bytes);
        }
        let _ = tx.send(bytes);
    });
    rx
}

fn command(program: &Path, args: &[String], cwd: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        // Plain text in the log (cli.md: no colour with NO_COLOR).
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .stdin(Stdio::null());
    no_window(&mut cmd);
    cmd
}

/// No console window for the child on Windows (`CREATE_NO_WINDOW`).
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Its own process group on Unix, so Cancel can signal the whole tree.
fn new_process_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Terminates `pid` and its descendants: `taskkill /T /F` on Windows, the
/// process group (led by `pid`, see [`new_process_group`]) elsewhere.
fn kill_tree(pid: u32) {
    let mut cmd = if cfg!(windows) {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"]);
        cmd
    } else {
        let mut cmd = Command::new("kill");
        cmd.args(["-TERM", "--", &format!("-{pid}")]);
        cmd
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let _ = cmd.status();
}
