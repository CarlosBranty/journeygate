//! The exact process an adapter starts inside the sandbox.

use guard_core::ports::sandbox::{ExecRequest, WORKSPACE_DIR};
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// A file the run loop writes into a writable mount (`/task/...`) before start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SandboxFile {
    pub path: String,
    pub contents: String,
}

/// argv (never through a shell), non-secret env, working directory, stdin and
/// config files. Secrets are never part of an invocation: the virtual key is
/// injected by the sandbox as `task.gateway.key_env` (sandbox.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Invocation {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub workdir: String,
    pub stdin: Option<String>,
    pub files: Vec<SandboxFile>,
}

impl Invocation {
    pub(crate) fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            env: BTreeMap::new(),
            workdir: WORKSPACE_DIR.to_string(),
            stdin: None,
            files: vec![],
        }
    }

    pub(crate) fn with_env(mut self, k: &str, v: impl Into<String>) -> Self {
        self.env.insert(k.to_string(), v.into());
        self
    }

    pub(crate) fn with_file(mut self, path: &str, contents: impl Into<String>) -> Self {
        self.files.push(SandboxFile {
            path: path.to_string(),
            contents: contents.into(),
        });
        self
    }

    /// Contents of the config file written at `path`.
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.contents.as_str())
    }

    pub fn to_exec(&self, timeout: Duration) -> ExecRequest {
        ExecRequest {
            argv: self.argv.clone(),
            env: self.env.clone(),
            workdir: Some(self.workdir.clone()),
            timeout: Some(timeout),
            stdin: self.stdin.as_ref().map(|s| s.as_bytes().to_vec()),
        }
    }

    /// Pretty JSON with a trailing newline, for golden files.
    pub fn to_golden(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("invocation serializes");
        s.push('\n');
        s
    }
}
