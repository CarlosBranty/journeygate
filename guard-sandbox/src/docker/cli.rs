//! The seam between [`super::DockerSandbox`] and the `docker` binary.
//!
//! Every Docker operation is one `docker <args>` process. Secrets never appear
//! in `args`: they travel as environment variables of the `docker` process and
//! are forwarded by name (`docker exec -e NAME`), so they are not visible in
//! the host process list, in `docker inspect`, or on disk.

use guard_core::ports::{SandboxError, Secret};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// One `docker` process.
#[derive(Debug, Clone, Default)]
pub struct Invocation {
    pub args: Vec<String>,
    /// Set on the `docker` process environment only (never in `args`).
    pub secret_env: Vec<(String, Secret)>,
    pub stdin: Option<Vec<u8>>,
    /// Kill the `docker` process after this long.
    pub timeout: Option<Duration>,
}

impl Invocation {
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: args.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// `args` joined by spaces (diagnostics and test assertions).
    pub fn line(&self) -> String {
        self.args.join(" ")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOutput {
    /// `None` when the process was killed (timeout or signal).
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

impl CliOutput {
    pub fn ok(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            status: Some(0),
            stdout: stdout.into(),
            ..Self::default()
        }
    }

    pub fn failed(status: i32, stderr: impl Into<Vec<u8>>) -> Self {
        Self {
            status: Some(status),
            stderr: stderr.into(),
            ..Self::default()
        }
    }

    pub fn success(&self) -> bool {
        self.status == Some(0) && !self.timed_out
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[async_trait::async_trait]
pub trait DockerCli: Send + Sync {
    /// Runs one `docker` process. `Err` only when it could not be started.
    async fn run(&self, invocation: Invocation) -> Result<CliOutput, SandboxError>;
}

/// The real `docker` binary (`docker` on `PATH` by default).
#[derive(Debug, Clone)]
pub struct ProcessDockerCli {
    bin: String,
}

impl ProcessDockerCli {
    pub fn new(bin: impl Into<String>) -> Self {
        Self { bin: bin.into() }
    }
}

impl Default for ProcessDockerCli {
    fn default() -> Self {
        Self::new("docker")
    }
}

#[async_trait::async_trait]
impl DockerCli for ProcessDockerCli {
    async fn run(&self, invocation: Invocation) -> Result<CliOutput, SandboxError> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.args(&invocation.args)
            .stdin(if invocation.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (name, value) in &invocation.secret_env {
            cmd.env(name, value.expose());
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::Unavailable(format!("cannot start {}: {e}", self.bin)))?;
        if let Some(bytes) = invocation.stdin {
            let mut stdin = child.stdin.take().expect("stdin is piped");
            // Written concurrently with the output reads so a large payload
            // cannot deadlock against a full stdout pipe.
            tokio::spawn(async move {
                let _ = stdin.write_all(&bytes).await;
                let _ = stdin.shutdown().await;
            });
        }
        let wait = child.wait_with_output();
        let output = match invocation.timeout {
            Some(limit) => match tokio::time::timeout(limit, wait).await {
                Ok(r) => r,
                // Dropping the future drops the child, which kill_on_drop kills.
                Err(_) => {
                    return Ok(CliOutput {
                        status: None,
                        timed_out: true,
                        ..CliOutput::default()
                    });
                }
            },
            None => wait.await,
        }
        .map_err(|e| SandboxError::Io(format!("{} failed: {e}", self.bin)))?;
        Ok(CliOutput {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
            timed_out: false,
        })
    }
}
