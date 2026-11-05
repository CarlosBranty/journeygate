//! Check sandboxes: the orchestrator's deterministic Playwright checks of
//! the agent lane (red-first, gate; qa-coder-protocol.md steps 4 and 8) run
//! agent-written code (the app under test, the specs), so they run in a
//! sandbox container too, never on the host.
//!
//! One check = one fresh container from the sandbox image
//! ([`CheckSandboxProvider::create_check`]) with the same hardening as an
//! agent sandbox (`--read-only`, `--cap-drop ALL`, `no-new-privileges`,
//! non-root, limits, `runsc` when available) on its own internal network
//! with **no** egress rule and `--dns 127.0.0.1`: the app under test listens
//! inside the container, and nothing goes out. No host directory is mounted:
//! the candidate tree is copied in as a tar on `tar -x` stdin into a tmpfs
//! [`CHECK_WORKSPACE`]. The container environment holds only the image's
//! variables plus what the check passes ([`PlaywrightCheck::env`]; secrets
//! such as the testability token are forwarded by name, never in argv).
//!
//! [`run_playwright_check`] copies the tree in, runs [`DRIVER_SCRIPT`]
//! (links the image's `@playwright/test` into the e2e dir, starts the app,
//! waits for its ready path, runs Playwright with the JSON reporter), reads
//! the report, copies the results (report, app log, Playwright stderr,
//! screenshots) out to a host directory, and always destroys the sandbox.

pub mod tar;

use crate::docker::is_env_name;
use guard_core::ports::sandbox::ResourceLimits;
use guard_core::ports::{ExecRequest, Role, Sandbox, SandboxError, SandboxProvider, Secret};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

/// Where the candidate tree is extracted (a tmpfs; the rootfs is read-only).
pub const CHECK_WORKSPACE: &str = "/workspace";
/// The port the app under test listens on inside the check container.
pub const CHECK_APP_PORT: u16 = 3000;
/// Driver outputs inside the container.
pub const CHECK_OUT_DIR: &str = "/workspace/.guard-check";
pub const REPORT_PATH: &str = "/workspace/.guard-check/report.json";
pub const STDERR_PATH: &str = "/workspace/.guard-check/playwright.stderr";
pub const APP_LOG_PATH: &str = "/workspace/.guard-check/app.log";

/// `$0` of [`DRIVER_SCRIPT`].
pub const DRIVER_NAME: &str = "guard-check";
/// `$0` of [`COPY_OUT_SCRIPT`].
pub const COPY_OUT_NAME: &str = "guard-check-results";

/// Exit code of the driver when the app never answered its ready path.
pub const EXIT_APP_NOT_READY: i32 = 90;
/// Exit code of the driver when the check itself cannot be set up.
pub const EXIT_SETUP: i32 = 91;

/// Most screenshots copied out per check, and the byte cap of the copy.
pub const MAX_SCREENSHOTS: usize = 50;
pub const MAX_RESULTS_BYTES: usize = 64 << 20;
const LOG_TAIL_BYTES: usize = 2048;

/// `sh -c DRIVER_SCRIPT guard-check <e2e dir> <playwright args...>`, cwd
/// [`CHECK_WORKSPACE`]. Env: `GUARD_CHECK_APP_START`, `GUARD_CHECK_READY_PATH`,
/// `GUARD_CHECK_READY_SECS`, `GUARD_CHECK_RUN_SECS`, `PORT`, and the runner
/// variables. Exit 0 once Playwright ran (its own exit code is printed as
/// `guard-check: playwright-exit=<n>`; 124/137 = killed by `timeout`),
/// [`EXIT_APP_NOT_READY`] with the app log tail on stdout, or [`EXIT_SETUP`].
pub const DRIVER_SCRIPT: &str = r#"set -u
e2e=$1
shift
out=/workspace/.guard-check
mkdir -p "$out" || exit 91
cd /workspace || exit 91
pw=
for root in /usr/local/lib/node_modules /usr/lib/node_modules; do
  if [ -f "$root/@playwright/test/cli.js" ]; then pw=$root/@playwright/test; break; fi
done
if [ -z "$pw" ]; then
  echo "guard-check: the image has no global @playwright/test" >&2
  exit 91
fi
rm -rf "$e2e/node_modules/@playwright/test"
mkdir -p "$e2e/node_modules/@playwright" || exit 91
ln -s "$pw" "$e2e/node_modules/@playwright/test" || exit 91
sh -c "$GUARD_CHECK_APP_START" >"$out/app.log" 2>&1 </dev/null &
end=$(( $(date +%s) + GUARD_CHECK_READY_SECS ))
until node -e 'fetch(process.argv[1],{signal:AbortSignal.timeout(2000)}).then(r=>process.exit(r.status===200?0:1),()=>process.exit(1))' "http://127.0.0.1:$PORT$GUARD_CHECK_READY_PATH" 2>/dev/null; do
  if [ "$(date +%s)" -ge "$end" ]; then
    echo "guard-check: app-not-ready"
    tail -c 2048 "$out/app.log"
    exit 90
  fi
  sleep 0.25
done
cd "$e2e" || exit 91
PLAYWRIGHT_JSON_OUTPUT_NAME="$out/report.json" timeout -k 10 "$GUARD_CHECK_RUN_SECS" \
  node node_modules/@playwright/test/cli.js test "$@" --reporter=json >/dev/null 2>"$out/playwright.stderr"
echo "guard-check: playwright-exit=$?"
exit 0"#;

/// `sh -c COPY_OUT_SCRIPT guard-check-results <e2e dir>`: a tar of the
/// screenshots under `<e2e dir>/test-results` on stdout (empty when none).
pub const COPY_OUT_SCRIPT: &str = r#"cd "/workspace/$1" 2>/dev/null || exit 0
[ -d test-results ] || exit 0
find test-results -type f \( -name '*.png' -o -name '*.jpg' -o -name '*.jpeg' \) -size -5120k \
  | head -n 50 | tar -c -f - --no-recursion -T -"#;

/// Which check a container runs (its `guard.check` label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    /// The QA's spec alone on the base commit.
    RedFirst,
    /// The Coder's diff plus the sealed spec (and the smoke suite).
    Gate,
}

impl CheckKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckKind::RedFirst => "red-first",
            CheckKind::Gate => "gate",
        }
    }

    /// The role whose output is under check.
    pub fn role(self) -> Role {
        match self {
            CheckKind::RedFirst => Role::Qa,
            CheckKind::Gate => Role::Coder,
        }
    }
}

/// Everything a provider needs to create one check sandbox.
#[derive(Debug, Clone)]
pub struct CheckSpec {
    pub run_id: String,
    pub ticket: String,
    pub kind: CheckKind,
    /// `<repo>@sha256:<64 hex>` (the agent sandbox image).
    pub image: String,
    pub limits: ResourceLimits,
    /// Size of the tmpfs at [`CHECK_WORKSPACE`].
    pub workspace_size_mb: u32,
    /// Forwarded by name to every process of the check.
    pub secrets: BTreeMap<String, Secret>,
}

impl CheckSpec {
    pub fn new(run_id: &str, ticket: &str, kind: CheckKind, image: &str) -> Self {
        Self {
            run_id: run_id.into(),
            ticket: ticket.into(),
            kind,
            image: image.into(),
            limits: ResourceLimits::default(),
            workspace_size_mb: 1024,
            secrets: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let digest_ok = self.image.split_once("@sha256:").is_some_and(|(repo, d)| {
            !repo.is_empty()
                && d.len() == 64
                && d.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if !digest_ok {
            return Err(format!(
                "check image {:?} must be pinned by digest (@sha256:...)",
                self.image
            ));
        }
        for (what, v) in [("run_id", &self.run_id), ("ticket", &self.ticket)] {
            let plain = !v.is_empty()
                && v.len() <= 128
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
            if !plain {
                return Err(format!("check {what} {v:?} is not [A-Za-z0-9._-]"));
            }
        }
        if let Some(name) = self.secrets.keys().find(|n| !is_env_name(n)) {
            return Err(format!("secrets name {name:?} is not an env var name"));
        }
        if self.workspace_size_mb == 0 || self.limits.timeout.is_zero() {
            return Err("check workspace size and timeout must be positive".into());
        }
        Ok(())
    }
}

/// [`SandboxProvider`] plus check sandboxes (same daemon, same hardening).
#[async_trait::async_trait]
pub trait CheckSandboxProvider: SandboxProvider {
    /// Validates `spec` and starts a fresh check container: no mounts, no
    /// egress, a tmpfs [`CHECK_WORKSPACE`]. Use `exec`, `read_file` and
    /// `destroy` of [`SandboxProvider`] on the returned handle.
    async fn create_check(&self, spec: &CheckSpec) -> Result<Sandbox, SandboxError>;
}

/// One Playwright run against the candidate tree.
#[derive(Debug, Clone)]
pub struct PlaywrightCheck {
    /// Tar of the candidate tree ([`tar::write`]), extracted at [`CHECK_WORKSPACE`].
    pub tree: Vec<u8>,
    /// The Playwright project dir, relative to the tree root.
    pub e2e_dir: String,
    /// `[app].start`, run by `sh -c` from the tree root with `PORT`.
    pub app_start: String,
    pub ready_path: String,
    /// Playwright args after `test` (specs, `--project`, `--retries`).
    pub args: Vec<String>,
    /// Non-secret variables of the app and Playwright (runner env).
    pub env: BTreeMap<String, String>,
    pub app_ready_timeout: Duration,
    pub run_timeout: Duration,
    /// Host directory for the copied-out results, when kept.
    pub results_dir: Option<PathBuf>,
}

impl PlaywrightCheck {
    /// The wall clock a check sandbox needs for this run.
    pub fn sandbox_timeout(&self) -> Duration {
        self.app_ready_timeout + self.run_timeout + Duration::from_secs(180)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    /// Playwright ran and wrote its JSON report.
    Report { json: Vec<u8>, timed_out: bool },
    /// The app did not answer its ready path in time (with its log tail).
    AppNotReady(String),
    /// Playwright wrote no report (with its stderr tail).
    NoReport(String),
}

#[derive(Debug, Clone)]
pub struct CheckRun {
    pub outcome: CheckOutcome,
    /// Host files written under [`PlaywrightCheck::results_dir`].
    pub results: Vec<PathBuf>,
    /// The sandbox could not be removed (its labels let the reaper do it).
    pub cleanup_error: Option<String>,
}

fn tail(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim();
    let mut start = s.len().saturating_sub(max);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

/// Runs `check` in a fresh check sandbox of `provider` and destroys it.
/// `Err` = the sandbox could not run the check (an infrastructure error);
/// a candidate that cannot be judged is an `Ok` [`CheckOutcome`].
pub async fn run_playwright_check(
    provider: &dyn CheckSandboxProvider,
    spec: &CheckSpec,
    check: &PlaywrightCheck,
) -> Result<CheckRun, SandboxError> {
    let sandbox = provider.create_check(spec).await?;
    let result = run_in(provider, &sandbox, check).await;
    let cleanup_error = provider
        .destroy(&sandbox)
        .await
        .err()
        .map(|e| e.to_string());
    let (outcome, results) = result?;
    Ok(CheckRun {
        outcome,
        results,
        cleanup_error,
    })
}

async fn run_in(
    provider: &dyn CheckSandboxProvider,
    sandbox: &Sandbox,
    check: &PlaywrightCheck,
) -> Result<(CheckOutcome, Vec<PathBuf>), SandboxError> {
    let copy_in = provider
        .exec(
            sandbox,
            &ExecRequest {
                argv: ["tar", "-x", "-f", "-", "-C", CHECK_WORKSPACE]
                    .map(String::from)
                    .to_vec(),
                workdir: Some(CHECK_WORKSPACE.into()),
                timeout: Some(Duration::from_secs(300)),
                stdin: Some(check.tree.clone()),
                ..ExecRequest::default()
            },
        )
        .await?;
    if copy_in.exit_code != 0 || copy_in.timed_out {
        return Err(SandboxError::Exec(format!(
            "copy the candidate tree in: {}",
            tail(&copy_in.stderr, LOG_TAIL_BYTES)
        )));
    }

    let mut env = check.env.clone();
    env.insert("GUARD_CHECK_APP_START".into(), check.app_start.clone());
    env.insert("GUARD_CHECK_READY_PATH".into(), check.ready_path.clone());
    env.insert(
        "GUARD_CHECK_READY_SECS".into(),
        check.app_ready_timeout.as_secs().max(1).to_string(),
    );
    env.insert(
        "GUARD_CHECK_RUN_SECS".into(),
        check.run_timeout.as_secs().max(1).to_string(),
    );
    env.insert("PORT".into(), CHECK_APP_PORT.to_string());
    env.insert(
        "TARGET_BASE_URL".into(),
        format!("http://127.0.0.1:{CHECK_APP_PORT}"),
    );
    let mut argv: Vec<String> = vec![
        "sh".into(),
        "-c".into(),
        DRIVER_SCRIPT.into(),
        DRIVER_NAME.into(),
        check.e2e_dir.clone(),
    ];
    argv.extend(check.args.iter().cloned());
    let driver = provider
        .exec(
            sandbox,
            &ExecRequest {
                argv,
                env,
                workdir: Some(CHECK_WORKSPACE.into()),
                timeout: Some(
                    check.app_ready_timeout + check.run_timeout + Duration::from_secs(60),
                ),
                stdin: None,
            },
        )
        .await?;
    let stdout = String::from_utf8_lossy(&driver.stdout).into_owned();
    if driver.exit_code == EXIT_SETUP {
        return Err(SandboxError::Unavailable(format!(
            "check setup failed: {}",
            tail(&driver.stderr, LOG_TAIL_BYTES)
        )));
    }
    let mut results = Vec::new();
    if driver.exit_code == EXIT_APP_NOT_READY {
        let log = stdout
            .split_once("guard-check: app-not-ready")
            .map_or(stdout.as_str(), |(_, rest)| rest);
        let outcome = CheckOutcome::AppNotReady(format!(
            "the app did not answer {} within {}s: {}",
            check.ready_path,
            check.app_ready_timeout.as_secs(),
            tail(log.as_bytes(), LOG_TAIL_BYTES)
        ));
        if let Some(dir) = &check.results_dir {
            results.extend(copy_logs(provider, sandbox, dir).await);
        }
        return Ok((outcome, results));
    }
    let pw_exit = stdout
        .lines()
        .find_map(|l| l.strip_prefix("guard-check: playwright-exit="))
        .and_then(|n| n.trim().parse::<i32>().ok());
    let timed_out = driver.timed_out || matches!(pw_exit, Some(124 | 137));
    let outcome = match provider.read_file(sandbox, REPORT_PATH).await {
        Ok(json) => CheckOutcome::Report { json, timed_out },
        Err(SandboxError::NotFound(_)) => {
            let stderr = provider
                .read_file(sandbox, STDERR_PATH)
                .await
                .unwrap_or_default();
            CheckOutcome::NoReport(format!(
                "playwright wrote no JSON report{}: {}",
                if timed_out { " (timed out)" } else { "" },
                tail(&stderr, LOG_TAIL_BYTES)
            ))
        }
        Err(e) => return Err(e),
    };
    if let Some(dir) = &check.results_dir {
        if let CheckOutcome::Report { json, .. } = &outcome {
            if let Some(p) = write_result(dir, std::path::Path::new("report.json"), json) {
                results.push(p);
            }
        }
        results.extend(copy_logs(provider, sandbox, dir).await);
        results.extend(copy_screenshots(provider, sandbox, &check.e2e_dir, dir).await);
    }
    Ok((outcome, results))
}

fn write_result(dir: &std::path::Path, rel: &std::path::Path, bytes: &[u8]) -> Option<PathBuf> {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent()?).ok()?;
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

async fn copy_logs(
    provider: &dyn CheckSandboxProvider,
    sandbox: &Sandbox,
    dir: &std::path::Path,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (path, name) in [
        (APP_LOG_PATH, "app.log"),
        (STDERR_PATH, "playwright.stderr"),
    ] {
        if let Ok(bytes) = provider.read_file(sandbox, path).await {
            let bytes = &bytes[bytes.len().saturating_sub(1 << 20)..];
            out.extend(write_result(dir, std::path::Path::new(name), bytes));
        }
    }
    out
}

/// Best effort: the screenshots under `<e2e>/test-results`, written under
/// `dir` only through [`tar::safe_relative`] paths.
async fn copy_screenshots(
    provider: &dyn CheckSandboxProvider,
    sandbox: &Sandbox,
    e2e_dir: &str,
    dir: &std::path::Path,
) -> Vec<PathBuf> {
    let request = ExecRequest {
        argv: vec![
            "sh".into(),
            "-c".into(),
            COPY_OUT_SCRIPT.into(),
            COPY_OUT_NAME.into(),
            e2e_dir.into(),
        ],
        workdir: Some(CHECK_WORKSPACE.into()),
        timeout: Some(Duration::from_secs(120)),
        ..ExecRequest::default()
    };
    let Ok(out) = provider.exec(sandbox, &request).await else {
        return Vec::new();
    };
    if out.exit_code != 0 || out.stdout.is_empty() {
        return Vec::new();
    }
    let Ok(files) = tar::read(&out.stdout, MAX_RESULTS_BYTES) else {
        return Vec::new();
    };
    files
        .into_iter()
        .filter(|f| {
            let lower = f.path.to_ascii_lowercase();
            lower.starts_with("test-results/")
                && [".png", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e))
        })
        .take(MAX_SCREENSHOTS)
        .filter_map(|f| {
            let rel = tar::safe_relative(&f.path)?;
            write_result(dir, &rel, &f.bytes)
        })
        .collect()
}

#[cfg(test)]
mod tests;
