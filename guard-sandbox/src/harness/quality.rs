//! Deterministic test-quality checks (qa-coder-protocol.md step 3).
//!
//! [`NodeSpecQuality`] runs `assets/spec-quality.mjs` (TypeScript compiler
//! API, syntax only) with the spec on stdin and parses its JSON verdict. The
//! check ids are stable and reported in `agent_runs.result.quality`.

use super::HarnessError;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Ids `spec-quality.mjs` can report (the contract table plus `parse_error`).
pub const QUALITY_CHECK_IDS: &[&str] = &[
    "no_focus",
    "no_skip",
    "no_empty_test",
    "has_expect",
    "no_trivial_expect",
    "no_wait_for_timeout",
    "no_self_mock",
    "no_swallowed_assertions",
    "no_host_access",
    "ticket_tag",
    "journey_tag_format",
    "parse_error",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityViolation {
    pub id: String,
    /// 1-based.
    pub line: u32,
    pub message: String,
}

#[async_trait::async_trait]
pub trait SpecQuality: Send + Sync {
    /// Violations of `bytes` (the spec at repo-relative `spec_path`) for `ticket`.
    async fn check(
        &self,
        ticket: &str,
        spec_path: &str,
        bytes: &[u8],
    ) -> Result<Vec<QualityViolation>, HarnessError>;
}

/// `node assets/spec-quality.mjs` on the orchestrator host.
#[derive(Debug, Clone)]
pub struct NodeSpecQuality {
    pub node: String,
    pub script: PathBuf,
    /// `[app].base_url` and API prefixes `no_self_mock` protects (default `/api/`).
    pub api_bases: Vec<String>,
    pub timeout: Duration,
}

impl NodeSpecQuality {
    /// The script shipped in this crate (`assets/`, with its own
    /// `node_modules/typescript` after `pnpm install` there).
    pub fn bundled(api_bases: Vec<String>) -> Self {
        Self {
            node: "node".into(),
            script: Self::bundled_script(),
            api_bases,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn bundled_script() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/spec-quality.mjs")
    }
}

#[derive(Deserialize)]
struct Report {
    violations: Vec<QualityViolation>,
}

#[async_trait::async_trait]
impl SpecQuality for NodeSpecQuality {
    async fn check(
        &self,
        ticket: &str,
        spec_path: &str,
        bytes: &[u8],
    ) -> Result<Vec<QualityViolation>, HarnessError> {
        let mut cmd = tokio::process::Command::new(&self.node);
        cmd.arg(&self.script)
            .args(["--ticket", ticket, "--file", spec_path]);
        for base in &self.api_bases {
            cmd.args(["--api-base", base]);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| HarnessError::Quality(format!("cannot start {}: {e}", self.node)))?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let input = bytes.to_vec();
        tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        });
        let out = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| HarnessError::Quality("spec-quality.mjs timed out".into()))?
            .map_err(|e| HarnessError::Quality(e.to_string()))?;
        if !out.status.success() {
            return Err(HarnessError::Quality(format!(
                "spec-quality.mjs exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let report: Report = serde_json::from_slice(&out.stdout)
            .map_err(|e| HarnessError::Quality(format!("spec-quality.mjs output: {e}")))?;
        if let Some(v) = report
            .violations
            .iter()
            .find(|v| !QUALITY_CHECK_IDS.contains(&v.id.as_str()))
        {
            return Err(HarnessError::Quality(format!("unknown check id {}", v.id)));
        }
        Ok(report.violations)
    }
}
