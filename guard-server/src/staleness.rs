//! The memory staleness job in `guard serve` (memory.md, "Staleness"):
//! `guard_store::memory_staleness::run_staleness` on a timer
//! ([`DEFAULT_STALENESS_INTERVAL`], `GUARD_STALENESS_INTERVAL`), with the
//! production sources:
//!
//! - [`GitRepo`]: `TouchedFiles` over the serve checkout — the files of the
//!   commits on the default branch after a fact's `commit_sha`
//!   (`git log --name-only <sha>..<branch>`, newest commit per path) — and
//!   the default branch head for the fact writers ([`BranchHead`]).
//! - [`ImpactFanOut`]: `FanOutReach` over `guard-impact`'s import graph
//!   (`ImpactIndex::importers_of`), rebuilt from the checkout at every run
//!   ([`FanOutSource::rebuild`]) so new commits' imports count.
//! - `GateHistoryRuns` over the gate history for the fingerprint rule.
//!
//! The checkout is not fetched here: whatever keeps it current (a CI
//! deploy, a `git pull` cron) makes new default-branch commits visible.

use crate::clock::{Clock, SystemClock};
use crate::jobs::PeriodicJob;
use crate::memory::BranchHead;
use guard_core::config::TargetConfig;
use guard_core::ports::{GateHistory, ImpactError, MemoryFactsWriter, StoreError};
use guard_impact::ImpactIndex;
use guard_store::memory_staleness::{
    DEFAULT_FINGERPRINT_WINDOW, FanOutReach, GateHistoryRuns, StalenessConfig, StalenessReport,
    StalenessSources, TouchedFile, TouchedFiles, run_staleness,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Default interval of the staleness job.
pub const DEFAULT_STALENESS_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Env var the integrator reads for the interval ([`parse_interval`]).
pub const STALENESS_INTERVAL_ENV: &str = "GUARD_STALENESS_INTERVAL";
/// Deadline of one `git` call.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// `900`, `900s`, `15m` or `2h`; positive.
pub fn parse_interval(raw: &str) -> Result<Duration, String> {
    let s = raw.trim();
    let (digits, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => s.split_at(i),
        None => (s, "s"),
    };
    let n: u64 = digits
        .parse()
        .map_err(|_| format!("invalid interval {raw:?} (use 900, 900s, 15m or 2h)"))?;
    let secs = match unit {
        "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        _ => return Err(format!("invalid interval unit in {raw:?} (s, m or h)")),
    };
    if secs == 0 {
        return Err(format!("interval {raw:?} must be positive"));
    }
    Ok(Duration::from_secs(secs))
}

// ---------------------------------------------------------------- git

/// The serve checkout and its default branch (`main`, `origin/main`, `HEAD`).
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
    branch: String,
    program: String,
}

fn is_commit_id(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl GitRepo {
    pub fn new(root: &Path, branch: impl Into<String>) -> Self {
        Self {
            root: root.to_path_buf(),
            branch: branch.into(),
            program: "git".into(),
        }
    }

    async fn git(&self, args: &[&str]) -> Result<std::process::Output, StoreError> {
        let run = tokio::process::Command::new(&self.program)
            .arg("-C")
            .arg(&self.root)
            .args(["-c", "core.quotePath=false"])
            .args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        tokio::time::timeout(GIT_TIMEOUT, run)
            .await
            .map_err(|_| StoreError::Unavailable(format!("git {} timed out", args[0])))?
            .map_err(|e| StoreError::Unavailable(format!("cannot run git: {e}")))
    }

    async fn git_ok(&self, args: &[&str]) -> Result<String, StoreError> {
        let out = self.git(args).await?;
        if !out.status.success() {
            return Err(StoreError::Unavailable(format!(
                "git {} failed: {}",
                args[0],
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn branch_ref(&self) -> Result<&str, StoreError> {
        if self.branch.is_empty() || self.branch.starts_with('-') {
            return Err(StoreError::Invalid(format!(
                "invalid default branch {:?}",
                self.branch
            )));
        }
        Ok(&self.branch)
    }
}

/// Paths of `git log --name-only --format=commit %H`, newest commit per
/// path, sorted by path.
fn parse_log(stdout: &str) -> Vec<TouchedFile> {
    let mut newest: BTreeMap<String, String> = BTreeMap::new();
    let mut commit = None;
    for line in stdout.lines().map(|l| l.trim_end_matches('\r')) {
        if let Some(sha) = line.strip_prefix("commit ") {
            commit = Some(sha.trim().to_string());
        } else if !line.trim().is_empty() {
            if let Some(c) = &commit {
                newest.entry(line.to_string()).or_insert_with(|| c.clone());
            }
        }
    }
    newest
        .into_iter()
        .map(|(path, commit)| TouchedFile { path, commit })
        .collect()
}

#[async_trait::async_trait]
impl TouchedFiles for GitRepo {
    /// Commits on the default branch that `since` does not contain. A
    /// commit the checkout does not have yields nothing (it cannot be
    /// compared; the other staleness rules still apply).
    async fn touched_since(
        &self,
        _repo: &str,
        since: &str,
    ) -> Result<Vec<TouchedFile>, StoreError> {
        if !is_commit_id(since) {
            return Err(StoreError::Invalid(format!("{since:?} is not a commit id")));
        }
        let branch = self.branch_ref()?;
        let probe = format!("{since}^{{commit}}");
        if !self
            .git(&["cat-file", "-e", &probe])
            .await?
            .status
            .success()
        {
            tracing::debug!(commit = since, "staleness: commit not in the checkout");
            return Ok(vec![]);
        }
        let range = format!("{since}..{branch}");
        let out = self
            .git_ok(&[
                "log",
                "--name-only",
                "--no-renames",
                "--format=commit %H",
                &range,
                "--",
            ])
            .await?;
        Ok(parse_log(&out))
    }
}

#[async_trait::async_trait]
impl BranchHead for GitRepo {
    async fn head(&self) -> Result<String, String> {
        let branch = self.branch_ref().map_err(|e| e.to_string())?;
        let spec = format!("{branch}^{{commit}}");
        let out = self
            .git_ok(&["rev-parse", "--verify", "--end-of-options", &spec])
            .await
            .map_err(|e| e.to_string())?;
        Ok(out.trim().to_string())
    }
}

// ---------------------------------------------------------------- fan-out

/// `guard-impact`'s import graph as the staleness job's fan-out.
pub struct ImpactFanOut(ImpactIndex);

impl ImpactFanOut {
    pub fn new(index: ImpactIndex) -> Self {
        Self(index)
    }

    pub fn build(root: &Path, config: &TargetConfig) -> Result<Self, ImpactError> {
        ImpactIndex::build(root, config).map(Self)
    }
}

impl FanOutReach for ImpactFanOut {
    fn dependents(&self, path: &str) -> Vec<String> {
        self.0
            .importers_of(path)
            .into_iter()
            .map(str::to_string)
            .collect()
    }
}

/// Where a run gets its fan-out graph.
#[derive(Clone)]
pub enum FanOutSource {
    Fixed(Arc<dyn FanOutReach>),
    /// [`ImpactFanOut::build`] of the checkout at every run.
    Rebuild {
        root: PathBuf,
        config: Box<TargetConfig>,
    },
}

impl FanOutSource {
    pub fn fixed(graph: Arc<dyn FanOutReach>) -> Self {
        Self::Fixed(graph)
    }

    pub fn rebuild(root: &Path, config: TargetConfig) -> Self {
        Self::Rebuild {
            root: root.to_path_buf(),
            config: Box::new(config),
        }
    }

    pub async fn current(&self) -> Result<Arc<dyn FanOutReach>, String> {
        match self {
            Self::Fixed(g) => Ok(g.clone()),
            Self::Rebuild { root, config } => {
                let (root, config) = (root.clone(), config.as_ref().clone());
                tokio::task::spawn_blocking(move || ImpactFanOut::build(&root, &config))
                    .await
                    .map_err(|e| format!("impact index task failed: {e}"))?
                    .map(|g| Arc::new(g) as Arc<dyn FanOutReach>)
                    .map_err(|e| e.to_string())
            }
        }
    }
}

// ---------------------------------------------------------------- job

#[derive(Clone)]
pub struct StalenessJob {
    facts: Arc<dyn MemoryFactsWriter>,
    repo: String,
    touched: Arc<dyn TouchedFiles>,
    fan_out: Option<FanOutSource>,
    gates: Option<Arc<dyn GateHistory>>,
    clock: Arc<dyn Clock>,
    fingerprint_window: u32,
}

impl StalenessJob {
    /// Rule 1 through direct changes only, rule 2, no rule 3 until
    /// [`with_gates`](Self::with_gates).
    pub fn new(
        facts: Arc<dyn MemoryFactsWriter>,
        repo: impl Into<String>,
        touched: Arc<dyn TouchedFiles>,
    ) -> Self {
        Self {
            facts,
            repo: repo.into(),
            touched,
            fan_out: None,
            gates: None,
            clock: Arc::new(SystemClock),
            fingerprint_window: DEFAULT_FINGERPRINT_WINDOW,
        }
    }

    pub fn with_fan_out(mut self, source: FanOutSource) -> Self {
        self.fan_out = Some(source);
        self
    }

    /// Enables rule 3 (fingerprints absent from the last N runs).
    pub fn with_gates(mut self, gates: Arc<dyn GateHistory>) -> Self {
        self.gates = Some(gates);
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_fingerprint_window(mut self, n: u32) -> Self {
        self.fingerprint_window = n.max(1);
        self
    }

    /// One pass. A fan-out graph that cannot be built degrades to direct
    /// changes (logged); store and git failures fail the pass.
    pub async fn run_once(&self) -> Result<StalenessReport, StoreError> {
        let graph = match &self.fan_out {
            Some(source) => match source.current().await {
                Ok(g) => Some(g),
                Err(e) => {
                    tracing::warn!(error = %e, "staleness: no fan-out graph; direct changes only");
                    None
                }
            },
            None => None,
        };
        let runs = self.gates.as_deref().map(GateHistoryRuns::new);
        let sources = StalenessSources {
            touched: self.touched.as_ref(),
            fan_out: graph.as_deref(),
            fingerprints: runs.as_ref().map(|r| r as _),
        };
        let config = StalenessConfig {
            now: self.clock.now_rfc3339(),
            fingerprint_window: self.fingerprint_window,
        };
        run_staleness(self.facts.as_ref(), &self.repo, &sources, &config).await
    }

    /// The job on a timer (`"memory staleness"`).
    pub fn periodic(self, interval: Duration) -> PeriodicJob {
        let job = Arc::new(self);
        PeriodicJob::new("memory staleness", interval, move || {
            let job = job.clone();
            async move {
                let r = job.run_once().await.map_err(|e| e.to_string())?;
                Ok(format!(
                    "{}: checked {}, marked {} needs_review",
                    job.repo,
                    r.checked,
                    r.marked.len()
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_log_keeps_the_newest_commit_per_path() {
        let out = "commit 3333\n\nsrc/a.ts\nsrc/c.ts\ncommit 2222\n\nsrc/a.ts\r\nsrc/b.ts\n";
        let got = parse_log(out);
        let pairs: Vec<_> = got
            .iter()
            .map(|t| (t.path.as_str(), t.commit.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("src/a.ts", "3333"),
                ("src/b.ts", "2222"),
                ("src/c.ts", "3333")
            ]
        );
    }

    #[test]
    fn only_hex_commit_ids_reach_git() {
        assert!(is_commit_id("abc1234"));
        assert!(!is_commit_id("--all"));
        assert!(!is_commit_id("HEAD"));
        assert!(!is_commit_id("abc"));
    }
}
