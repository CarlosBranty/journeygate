//! [`HostChecks`]: the orchestrator's deterministic checks of the agent lane
//! (qa-coder-protocol.md steps 3, 4, 7 and 8).
//!
//! - **Test quality**: `guard-sandbox`'s `assets/spec-quality.mjs`
//!   (TypeScript compiler API) through `NodeSpecQuality`, on the host (it
//!   parses the spec; it never runs it).
//! - **Diff scanner**: `guard_agents::scanner` (section 4), on the host.
//! - **Red-first** and **gate** run agent-written code (the app under test
//!   and the Playwright specs), so by default they run in a **check
//!   sandbox** ([`CheckIsolation::Sandbox`]): a fresh container per check
//!   from the agent sandbox image, created by the same `DockerSandbox`
//!   (`guard_sandbox::check`). Nothing of the host is mounted: the
//!   candidate tree is built on the host with git ([`candidate_tree`]: the
//!   `base_commit` tree in a temporary index, `git apply --cached` of the
//!   Coder diff for the gate, the spec bytes written over its path) and
//!   copied in as a tar. The container is on its own internal network with
//!   no egress rule (the app listens inside it; nothing goes out), its
//!   environment holds only the image's variables and the runner variables
//!   (the testability token is forwarded by name), and the Playwright JSON
//!   report, the app log and the screenshots are copied out to
//!   `<work_root>/check-results/<check>/`. The red-first run is the spec
//!   alone; the gate runs the sealed spec plus the smoke suite.
//! - `GUARD_UNSAFE_HOST_CHECKS=1` (dev only, warned loudly) keeps the old
//!   host path ([`CheckIsolation::UnsafeHost`]): a clean `git worktree` of
//!   `base_commit` with the orchestrator's `node_modules` linked in (a
//!   junction on Windows), the app started on a free port, Playwright on
//!   the host. Its processes get a cleared environment ([`scrub_env`]) but
//!   **full network and host filesystem access**.
//!
//! Every check gets its own name (`<run>-<ticket>-<what>-<pid>-<n>`), so
//! best-of-N gates never share a worktree, an index or a results dir.
//!
//! A candidate that cannot be judged (the diff does not apply, the app does
//! not start, Playwright writes no report) is a red gate with that reason as
//! the failure context, not an infrastructure error.

use super::host::{blocking, git, slash};
use guard_agents::pipeline::seams::{
    GateResult, QualityReport, RedFirst, SeamError, TicketChecks, TicketInput,
};
use guard_core::config::TargetConfig;
use guard_core::ports::{ExtractedDiff, SealedSpec, Secret};
use guard_sandbox::check::{
    CheckKind, CheckOutcome, CheckSandboxProvider, CheckSpec, PlaywrightCheck, tar,
};
use guard_sandbox::harness::quality::{NodeSpecQuality, SpecQuality};
use guard_sandbox::harness::red_first::{self, SpecRun};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Failure context handed to the next Coder attempt (bytes).
pub const FAILURE_CONTEXT_MAX_BYTES: usize = 4096;

/// `1`: run red-first and the gate on the host instead of a check sandbox.
pub const UNSAFE_HOST_CHECKS_ENV: &str = "GUARD_UNSAFE_HOST_CHECKS";

/// Printed when the lane starts with [`UNSAFE_HOST_CHECKS_ENV`], and before
/// every host check.
pub const UNSAFE_HOST_CHECKS_WARNING: &str = "guard: WARNING: UNSAFE: GUARD_UNSAFE_HOST_CHECKS=1: \
red-first and the gate run agent-written code (the app under test and Playwright specs) \
ON THE HOST, with full network and host filesystem access. Development only: never set it \
on a machine that holds credentials or serves other work.";

/// Whether `value` (of [`UNSAFE_HOST_CHECKS_ENV`]) asks for host checks:
/// exactly `1`, nothing else.
pub fn unsafe_host_checks_requested(value: Option<&str>) -> bool {
    value.map(str::trim) == Some("1")
}

/// Where red-first and the gate run.
#[derive(Clone)]
pub enum CheckIsolation {
    /// A fresh check sandbox per check (the default).
    Sandbox {
        provider: Arc<dyn CheckSandboxProvider>,
        /// The agent sandbox image (`repo@sha256:<64 hex>`).
        image: String,
    },
    /// On the host ([`UNSAFE_HOST_CHECKS_ENV`] only).
    UnsafeHost,
}

/// Variables of the orchestrator environment that agent-written code may
/// see on the [`CheckIsolation::UnsafeHost`] path: OS and toolchain only.
pub const HOST_CHECK_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "TEMP",
    "TMP",
    "TMPDIR",
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "COMMONPROGRAMFILES",
    "COMMONPROGRAMFILES(X86)",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "OS",
    "LANG",
    "LC_ALL",
    "TZ",
    "TERM",
    "DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_CACHE_HOME",
    "PLAYWRIGHT_BROWSERS_PATH",
];

/// Clears `cmd`'s environment and keeps only [`HOST_CHECK_ENV_ALLOWLIST`].
pub fn scrub_env(cmd: &mut Command) -> &mut Command {
    cmd.env_clear();
    for name in HOST_CHECK_ENV_ALLOWLIST {
        if let Some(v) = std::env::var_os(name) {
            cmd.env(name, v);
        }
    }
    cmd
}

/// Why a check produced no verdict.
#[derive(Debug)]
enum CheckError {
    /// The host or the sandbox could not run the check: retried by the worker.
    Infra(SeamError),
    /// The app did not become ready.
    AppNotReady(String),
    /// The candidate under check is broken (patch does not apply, no report).
    Candidate(String),
}

impl From<SeamError> for CheckError {
    fn from(e: SeamError) -> Self {
        CheckError::Infra(e)
    }
}

/// One check: the candidate (base, optional diff, one file written over
/// the tree) and the Playwright selection.
struct CheckJob {
    kind: CheckKind,
    name: String,
    run_id: String,
    ticket: String,
    base: String,
    diff: Option<String>,
    file_path: String,
    file_bytes: Vec<u8>,
    /// Relative to the Playwright dir.
    specs: Vec<String>,
}

#[derive(Clone)]
pub struct HostChecks {
    pub repo_root: PathBuf,
    pub work_root: PathBuf,
    /// The Playwright project dir, repo-relative (`e2e-playwright`).
    pub e2e_rel: String,
    pub config: TargetConfig,
    /// `[testability].token_env` value for the app and Playwright.
    pub token: String,
    pub quality: NodeSpecQuality,
    /// Extra spec paths (relative to the Playwright dir) the gate also runs.
    pub gate_extra: Vec<String>,
    pub app_ready_timeout: Duration,
    pub run_timeout: Duration,
    pub isolation: CheckIsolation,
}

impl HostChecks {
    pub fn new(
        repo_root: &Path,
        e2e_rel: &str,
        config: TargetConfig,
        token: String,
        isolation: CheckIsolation,
    ) -> Self {
        let api_bases = vec![config.app.base_url.clone(), "/api/".to_string()];
        Self {
            repo_root: repo_root.to_path_buf(),
            work_root: repo_root.join(".guard").join("agents").join("work"),
            e2e_rel: e2e_rel.trim_end_matches('/').to_string(),
            config,
            token,
            quality: NodeSpecQuality::bundled(api_bases),
            gate_extra: vec!["tests/smoke".into()],
            app_ready_timeout: Duration::from_secs(60),
            run_timeout: Duration::from_secs(600),
            isolation,
        }
    }

    /// `spec_path` (repo-relative) as Playwright sees it from its project dir.
    fn in_e2e<'a>(&self, spec_path: &'a str) -> Result<&'a str, SeamError> {
        spec_path
            .strip_prefix(&format!("{}/", self.e2e_rel))
            .ok_or_else(|| {
                SeamError(format!(
                    "{spec_path} is not under the Playwright dir {}",
                    self.e2e_rel
                ))
            })
    }

    /// Runs `job` where [`HostChecks::isolation`] says.
    async fn run_check(&self, job: CheckJob) -> Result<SpecRun, CheckError> {
        match &self.isolation {
            CheckIsolation::Sandbox { provider, image } => {
                self.run_sandboxed(provider.as_ref(), image, job).await
            }
            CheckIsolation::UnsafeHost => {
                eprintln!(
                    "{UNSAFE_HOST_CHECKS_WARNING} ({} {})",
                    job.kind.as_str(),
                    job.name
                );
                let this = self.clone();
                blocking(move || Ok(this.run_on_host(job))).await?
            }
        }
    }

    /// The runner variables of a check (`GUARD_TEST_TOKEN` excluded: the
    /// token is a secret) and the secrets.
    fn check_env(&self, run_id: &str) -> (BTreeMap<String, String>, BTreeMap<String, Secret>) {
        let env = guard_core::runner_env::runner_env(&guard_core::runner_env::RunnerEnvInput {
            config: &self.config,
            e2e_tags: "",
            run_id,
            token: None,
            docker: false,
        });
        let env: BTreeMap<String, String> = env
            .into_iter()
            .filter(|(k, _)| k != "TARGET_BASE_URL")
            .collect();
        let mut secrets = BTreeMap::new();
        for name in [
            "GUARD_TEST_TOKEN",
            self.config.testability.token_env.as_str(),
        ] {
            secrets.insert(name.to_string(), Secret::new(self.token.clone()));
        }
        (env, secrets)
    }

    async fn run_sandboxed(
        &self,
        provider: &dyn CheckSandboxProvider,
        image: &str,
        job: CheckJob,
    ) -> Result<SpecRun, CheckError> {
        let start = self
            .config
            .app
            .start
            .clone()
            .ok_or_else(|| SeamError("[app].start is not set; red-first needs it".into()))?;
        let (repo, scratch) = (self.repo_root.clone(), self.work_root.join("checks"));
        let (base, diff, path, bytes) = (
            job.base.clone(),
            job.diff.clone(),
            job.file_path.clone(),
            job.file_bytes.clone(),
        );
        let tree = blocking(move || {
            Ok(candidate_tree(
                &repo,
                &scratch,
                &base,
                diff.as_deref(),
                &path,
                &bytes,
            ))
        })
        .await??;
        let (env, secrets) = self.check_env(&format!("agent-check-{}", job.name));
        let mut args = job.specs.clone();
        args.extend(["--project=chromium".to_string(), "--retries=0".to_string()]);
        let check = PlaywrightCheck {
            tree,
            e2e_dir: self.e2e_rel.clone(),
            app_start: start,
            ready_path: self.config.app.ready_path.clone(),
            args,
            env,
            app_ready_timeout: self.app_ready_timeout,
            run_timeout: self.run_timeout,
            results_dir: Some(self.work_root.join("check-results").join(&job.name)),
        };
        let mut spec = CheckSpec::new(&job.run_id, &job.ticket, job.kind, image);
        spec.limits.timeout = check.sandbox_timeout();
        spec.secrets = secrets;
        let run = guard_sandbox::check::run_playwright_check(provider, &spec, &check)
            .await
            .map_err(|e| SeamError(format!("{} sandbox: {e}", job.kind.as_str())))?;
        if let Some(e) = &run.cleanup_error {
            eprintln!(
                "guard: warning: agents: {} sandbox not removed (the reaper will): {e}",
                job.kind.as_str()
            );
        }
        match run.outcome {
            CheckOutcome::Report { json, timed_out } => {
                let mut spec_run =
                    SpecRun::from_playwright_json(&json).map_err(CheckError::Candidate)?;
                spec_run.process_timed_out = timed_out;
                Ok(spec_run)
            }
            CheckOutcome::AppNotReady(why) => Err(CheckError::AppNotReady(why)),
            CheckOutcome::NoReport(why) => Err(CheckError::Candidate(why)),
        }
    }

    /// The [`CheckIsolation::UnsafeHost`] path: a worktree of the base with
    /// the candidate applied, the orchestrator's `node_modules` linked in.
    fn run_on_host(&self, job: CheckJob) -> Result<SpecRun, CheckError> {
        let dir = self.work_root.join("checks").join(&job.name);
        remove_worktree(&self.repo_root, &dir);
        std::fs::create_dir_all(dir.parent().expect("has a parent"))
            .map_err(|e| SeamError(format!("create checks dir: {e}")))?;
        git(
            &self.repo_root,
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                &slash(&dir),
                &job.base,
            ],
            None,
        )?;
        let result = (|| {
            if let Some(diff) = job.diff.as_deref().filter(|d| !d.trim().is_empty()) {
                git(
                    &dir,
                    &["apply", "--index", "--whitespace=nowarn", "-p1", "-"],
                    Some(diff.as_bytes()),
                )
                .map_err(|e| {
                    CheckError::Candidate(format!(
                        "the diff does not apply on the base commit: {e}"
                    ))
                })?;
            }
            let path = dir.join(&job.file_path);
            std::fs::create_dir_all(path.parent().expect("has a parent"))
                .map_err(|e| SeamError(e.to_string()))?;
            std::fs::write(path, &job.file_bytes).map_err(|e| SeamError(e.to_string()))?;
            for rel in ["node_modules", &format!("{}/node_modules", self.e2e_rel)] {
                link_dir(&self.repo_root.join(rel), &dir.join(rel))?;
            }
            let app = AppProcess::start(&self.config, &dir, &self.token, self.app_ready_timeout)?;
            let run = self.playwright(&dir, &job.specs, app.port);
            drop(app);
            run
        })();
        remove_worktree(&self.repo_root, &dir);
        result
    }

    fn playwright(&self, dir: &Path, specs: &[String], port: u16) -> Result<SpecRun, CheckError> {
        let e2e = dir.join(&self.e2e_rel);
        let report = dir.join(".guard-playwright.json");
        let run_id = format!("agent-check-{}", std::process::id());
        let env = guard_core::runner_env::runner_env(&guard_core::runner_env::RunnerEnvInput {
            config: &self.config,
            e2e_tags: "",
            run_id: &run_id,
            token: Some(&self.token),
            docker: false,
        });
        let mut cmd = Command::new(crate::playwright::npx_program());
        scrub_env(&mut cmd);
        cmd.args(["playwright", "test"])
            .args(specs)
            .args(["--reporter=json", "--project=chromium", "--retries=0"])
            .current_dir(&e2e)
            .envs(env)
            .env("TARGET_BASE_URL", format!("http://127.0.0.1:{port}"))
            .env("PLAYWRIGHT_JSON_OUTPUT_NAME", &report)
            .env_remove("CI")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let child = cmd
            .spawn()
            .map_err(|e| SeamError(format!("cannot start playwright: {e}")))?;
        let (timed_out, stderr) = wait_draining_stderr(child, self.run_timeout)?;
        let bytes = std::fs::read(&report).map_err(|e| {
            CheckError::Candidate(format!(
                "playwright wrote no JSON report ({e}): {}",
                tail(&stderr, FAILURE_CONTEXT_MAX_BYTES / 2)
            ))
        })?;
        let mut run = SpecRun::from_playwright_json(&bytes).map_err(CheckError::Candidate)?;
        run.process_timed_out = timed_out;
        Ok(run)
    }
}

/// `git` in `repo` with the temporary index `index`; raw stdout.
fn git_index(
    repo: &Path,
    index: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.autocrlf=false", "-c", "core.safecrlf=false"])
        .args(args)
        .current_dir(repo)
        .env("GIT_INDEX_FILE", index)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    // Feed stdin from a thread: `cat-file --batch` writes while it reads.
    let feeder = match (stdin, child.stdin.take()) {
        (Some(bytes), Some(mut pipe)) => {
            let bytes = bytes.to_vec();
            Some(std::thread::spawn(move || pipe.write_all(&bytes)))
        }
        _ => None,
    };
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    if let Some(f) = feeder {
        let _ = f.join();
    }
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// The candidate tree of a check as a tar: the `base` tree, `diff` applied
/// (`git apply --cached` on a temporary index under `scratch`), and
/// `file_bytes` at `file_path`. Only git objects are written on the host:
/// the checkout, its index and its worktree are never touched, and no
/// attribute (`export-ignore`, filters) of the candidate changes the bytes.
fn candidate_tree(
    repo: &Path,
    scratch: &Path,
    base: &str,
    diff: Option<&str>,
    file_path: &str,
    file_bytes: &[u8],
) -> Result<Vec<u8>, CheckError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::fs::create_dir_all(scratch)
        .map_err(|e| SeamError(format!("create {}: {e}", scratch.display())))?;
    let index = scratch.join(format!(
        "index-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = build_tree(repo, &index, base, diff, file_path, file_bytes);
    let _ = std::fs::remove_file(&index);
    let _ = std::fs::remove_file(index.with_extension("lock"));
    result
}

fn build_tree(
    repo: &Path,
    index: &Path,
    base: &str,
    diff: Option<&str>,
    file_path: &str,
    file_bytes: &[u8],
) -> Result<Vec<u8>, CheckError> {
    let infra = |e: String| CheckError::Infra(SeamError(format!("candidate tree: {e}")));
    let text = |b: Vec<u8>| String::from_utf8_lossy(&b).trim().to_string();
    git_index(repo, index, &["read-tree", base], None).map_err(infra)?;
    if let Some(diff) = diff.filter(|d| !d.trim().is_empty()) {
        git_index(
            repo,
            index,
            &["apply", "--cached", "--whitespace=nowarn", "-p1", "-"],
            Some(diff.as_bytes()),
        )
        .map_err(|e| {
            CheckError::Candidate(format!("the diff does not apply on the base commit: {e}"))
        })?;
    }
    let blob = text(
        git_index(
            repo,
            index,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            Some(file_bytes),
        )
        .map_err(infra)?,
    );
    git_index(
        repo,
        index,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},{file_path}"),
        ],
        None,
    )
    .map_err(infra)?;
    let tree = text(git_index(repo, index, &["write-tree"], None).map_err(infra)?);
    let listing = git_index(
        repo,
        index,
        &["ls-tree", "-r", "-z", "--full-tree", &tree],
        None,
    )
    .map_err(infra)?;
    // `<mode> <type> <sha>\t<path>\0`; gitlinks (submodules) are skipped.
    let mut items: Vec<(String, String, String)> = Vec::new();
    for rec in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = String::from_utf8_lossy(rec);
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let mut m = meta.split(' ');
        let (Some(mode), Some(kind), Some(sha)) = (m.next(), m.next(), m.next()) else {
            continue;
        };
        if kind == "blob" {
            items.push((mode.to_string(), sha.to_string(), path.to_string()));
        }
    }
    let shas: String = items.iter().map(|(_, sha, _)| format!("{sha}\n")).collect();
    let batch =
        git_index(repo, index, &["cat-file", "--batch"], Some(shas.as_bytes())).map_err(infra)?;
    let mut entries = Vec::with_capacity(items.len());
    let mut at = 0usize;
    for (mode, sha, path) in items {
        let nl = batch[at..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or_else(|| infra("truncated cat-file output".into()))?;
        let head = String::from_utf8_lossy(&batch[at..at + nl]).to_string();
        let size: usize = head
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|_| head.starts_with(&sha))
            .ok_or_else(|| infra(format!("unexpected cat-file header {head:?}")))?;
        let start = at + nl + 1;
        let bytes = batch
            .get(start..start + size)
            .ok_or_else(|| infra("truncated cat-file blob".into()))?
            .to_vec();
        at = start + size + 1;
        entries.push(match mode.as_str() {
            "120000" => tar::Entry::Symlink {
                path,
                target: String::from_utf8_lossy(&bytes).into_owned(),
            },
            "100755" => tar::Entry::File {
                path,
                mode: 0o755,
                bytes,
            },
            _ => tar::Entry::File {
                path,
                mode: 0o644,
                bytes,
            },
        });
    }
    tar::write(&entries).map_err(CheckError::Candidate)
}

/// Waits for `child` at most `timeout` (then kills its tree) while a thread
/// drains its stderr, so a chatty process never blocks on a full pipe.
/// Returns `(timed_out, stderr)`.
fn wait_draining_stderr(mut child: Child, timeout: Duration) -> Result<(bool, String), SeamError> {
    let reader = child.stderr.take().map(|mut s| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = s.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).into_owned()
        })
    });
    let started = Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > timeout => {
                crate::process::kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                timed_out = true;
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(SeamError(format!("wait: {e}"))),
        }
    }
    let stderr = reader
        .map(|r| r.join().unwrap_or_default())
        .unwrap_or_default();
    Ok((timed_out, stderr))
}

/// The last `max` bytes of `s` (on a char boundary).
fn tail(s: &str, max: usize) -> &str {
    let s = s.trim();
    let mut start = s.len().saturating_sub(max);
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Removes a check worktree. Every link inside it is removed first, as a
/// link: `git worktree remove --force` follows directory junctions and would
/// delete the linked `node_modules` of the orchestrator checkout.
fn remove_worktree(repo: &Path, dir: &Path) {
    if dir.exists() {
        unlink_all(dir, 3);
        let _ = git(repo, &["worktree", "remove", "--force", &slash(dir)], None);
        let _ = std::fs::remove_dir_all(dir);
    }
    let _ = git(repo, &["worktree", "prune"], None);
}

/// A symlink or (Windows) any reparse point, e.g. a directory junction.
fn is_link(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// Removes the links (not their targets) under `dir`, `depth` levels deep;
/// never descends into a link.
fn unlink_all(dir: &Path, depth: u32) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_link(&path) {
            // A junction is removed as an (empty) directory; a symlink as a file.
            if std::fs::remove_dir(&path).is_err() {
                let _ = std::fs::remove_file(&path);
            }
        } else if depth > 0 && path.is_dir() {
            unlink_all(&path, depth - 1);
        }
    }
}

/// Links `target` at `link` (a junction on Windows) when `target` exists.
fn link_dir(target: &Path, link: &Path) -> Result<(), SeamError> {
    if !target.is_dir() || link.exists() {
        return Ok(());
    }
    // Link to the real directory (the orchestrator checkout may itself hold
    // a junction); strip the verbatim prefix `mklink` does not accept.
    let real = std::fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let real = PathBuf::from(
        real.to_string_lossy()
            .trim_start_matches("\\\\?\\")
            .to_string(),
    );
    let target = real.as_path();
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SeamError(format!("link parent: {e}")))?;
    }
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(target.to_string_lossy().replace('/', "\\"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| SeamError(format!("mklink: {e}")))?;
        if !status.success() {
            return Err(SeamError(format!("mklink /J {} failed", link.display())));
        }
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(target, link).map_err(|e| SeamError(format!("symlink: {e}")))?;
    Ok(())
}

/// The app under test, started from a worktree; killed (whole tree) on drop.
struct AppProcess {
    pid: u32,
    port: u16,
}

impl AppProcess {
    fn start(
        config: &TargetConfig,
        dir: &Path,
        token: &str,
        timeout: Duration,
    ) -> Result<Self, CheckError> {
        let start = config
            .app
            .start
            .as_deref()
            .ok_or_else(|| SeamError("[app].start is not set; red-first needs it".into()))?;
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| SeamError(format!("no free port: {e}")))?
            .port();
        let log = std::fs::File::create(dir.join(".guard-app.log"))
            .map_err(|e| SeamError(format!("app log: {e}")))?;
        let mut cmd = crate::process::shell(start);
        scrub_env(&mut cmd);
        crate::process::detach(&mut cmd);
        let child = cmd
            .current_dir(dir)
            .env("PORT", port.to_string())
            .env(&config.testability.token_env, token)
            .stdin(Stdio::null())
            .stdout(log.try_clone().map_err(|e| SeamError(e.to_string()))?)
            .stderr(log)
            .spawn()
            .map_err(|e| SeamError(format!("cannot start the app ({start}): {e}")))?;
        let app = AppProcess {
            pid: child.id(),
            port,
        };
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if http_status(port, &config.app.ready_path) == Some(200) {
                return Ok(app);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let log = std::fs::read_to_string(dir.join(".guard-app.log")).unwrap_or_default();
        Err(CheckError::AppNotReady(format!(
            "the app did not answer {} within {}s: {}",
            config.app.ready_path,
            timeout.as_secs(),
            tail(&log, FAILURE_CONTEXT_MAX_BYTES / 2)
        )))
    }
}

impl Drop for AppProcess {
    fn drop(&mut self) {
        crate::process::kill_tree(self.pid);
    }
}

fn http_status(port: u16, path: &str) -> Option<u16> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut head = [0u8; 32];
    let n = s.read(&mut head).ok()?;
    let line = String::from_utf8_lossy(&head[..n]);
    line.split_whitespace().nth(1)?.parse().ok()
}

/// A worktree name no other check (another best-of-N candidate's gate, a
/// concurrent red-first) uses at the same time.
fn checkout_name(input: &TicketInput, what: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{}-{what}-{}-{n}",
        input.run_id,
        input.ticket,
        std::process::id()
    )
}

#[async_trait::async_trait]
impl TicketChecks for HostChecks {
    async fn test_quality(
        &self,
        input: &TicketInput,
        spec_path: &str,
        bytes: &[u8],
    ) -> Result<QualityReport, SeamError> {
        let violations = self
            .quality
            .check(&input.ticket, spec_path, bytes)
            .await
            .map_err(|e| SeamError(e.to_string()))?;
        let mut failed: Vec<String> = violations
            .into_iter()
            .map(|v| format!("{} (line {}: {})", v.id, v.line, v.message))
            .collect();
        failed.dedup();
        Ok(QualityReport { failed })
    }

    async fn red_first(
        &self,
        input: &TicketInput,
        spec_path: &str,
        bytes: &[u8],
    ) -> Result<RedFirst, SeamError> {
        let spec = self.in_e2e(spec_path)?.to_string();
        let job = CheckJob {
            kind: CheckKind::RedFirst,
            name: checkout_name(input, "red-first"),
            run_id: input.run_id.clone(),
            ticket: input.ticket.clone(),
            base: input.base_commit.clone(),
            diff: None,
            file_path: spec_path.to_string(),
            file_bytes: bytes.to_vec(),
            specs: vec![spec],
        };
        let run = match self.run_check(job).await {
            Ok(run) => run,
            Err(CheckError::Infra(e)) => return Err(e),
            // The spec alone cannot keep the base app from starting.
            Err(CheckError::AppNotReady(why)) => {
                return Err(SeamError(format!("red-first: {why}")));
            }
            Err(CheckError::Candidate(why)) => return Ok(RedFirst::Broken(why)),
        };
        Ok(match run.red_first() {
            red_first::RedFirst::Red => RedFirst::Failed,
            red_first::RedFirst::Passed => RedFirst::Passed,
            red_first::RedFirst::NotAnAssertionFailure(why) => RedFirst::Broken(why),
        })
    }

    async fn scan_diff(
        &self,
        _input: &TicketInput,
        diff: &ExtractedDiff,
    ) -> Result<Vec<String>, SeamError> {
        Ok(guard_agents::scanner::scan_diff(&diff.unified_diff))
    }

    async fn gate(
        &self,
        input: &TicketInput,
        diff: &ExtractedDiff,
        sealed: &SealedSpec,
        sealed_bytes: &[u8],
    ) -> Result<GateResult, SeamError> {
        let mut specs = vec![self.in_e2e(&sealed.path)?.to_string()];
        specs.extend(self.gate_extra.iter().cloned());
        let job = CheckJob {
            kind: CheckKind::Gate,
            name: checkout_name(input, "gate"),
            run_id: input.run_id.clone(),
            ticket: input.ticket.clone(),
            base: input.base_commit.clone(),
            diff: Some(diff.unified_diff.clone()),
            file_path: sealed.path.clone(),
            file_bytes: sealed_bytes.to_vec(),
            specs: specs.clone(),
        };
        let run = match self.run_check(job).await {
            Ok(run) => run,
            Err(CheckError::Infra(e)) => return Err(e),
            // The Coder's change broke the app or the run: a red gate
            // with the reason as feedback for the next attempt.
            Err(CheckError::AppNotReady(why) | CheckError::Candidate(why)) => {
                let why = tail(&why, FAILURE_CONTEXT_MAX_BYTES).to_string();
                return Ok(GateResult {
                    passed: false,
                    summary: format!(
                        "gate on {}: no verdict ({})",
                        short(&input.base_commit),
                        first_line(&why)
                    ),
                    failure_context: Some(why),
                });
            }
        };
        let passed = run.gate_passed();
        let summary = format!(
            "gate on {}: {} passed, {} failed, {} timed out ({})",
            short(&input.base_commit),
            run.passed,
            run.failed,
            run.timed_out,
            specs.join(", ")
        );
        Ok(GateResult {
            passed,
            summary,
            failure_context: (!passed).then(|| run.failure_context(FAILURE_CONTEXT_MAX_BYTES)),
        })
    }
}

fn short(sha: &str) -> &str {
    &sha[..12.min(sha.len())]
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::ports::{ExecOutput, ExecRequest, Sandbox, SandboxError, SandboxId};
    use guard_sandbox::check::{CheckSandboxProvider, CheckSpec, REPORT_PATH, tar};
    use std::sync::{Arc, Mutex};

    fn tconfig(start: &str) -> TargetConfig {
        guard_core::config::parse_target(&format!(
            "version = 1\n[app]\nname = \"shop\"\nlanguages = [\"javascript\"]\n\
             start = {start:?}\nbase_url = \"http://localhost:3000\"\n\
             [areas.catalog]\ndescription = \"c\"\npaths = [\"src/**\"]\n"
        ))
        .unwrap()
    }

    fn base_repo() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("e2e/tests")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.ts"), "export const a = 1;\n").unwrap();
        std::fs::write(repo.join("e2e/tests/x.spec.ts"), "x\n").unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"], None).unwrap();
        git(&repo, &["add", "-A"], None).unwrap();
        git(&repo, &["commit", "--quiet", "-m", "base"], None).unwrap();
        let base = git(&repo, &["rev-parse", "HEAD"], None).unwrap();
        (tmp, base)
    }

    fn input(base: &str) -> TicketInput {
        TicketInput {
            repo: "acme/shop".into(),
            ticket: "SHOP-1".into(),
            run_id: "run-1".into(),
            base_commit: base.into(),
            summary: "s".into(),
            acceptance_criteria: vec![],
            app_paths: vec!["src/**".into()],
            tests_dir: "e2e/tests".into(),
            labels: vec![],
            predicted_files: vec![],
            preview: None,
            package_mirror: None,
            experiment: None,
            areas: vec![],
            journeys: vec![],
            claim: 0,
        }
    }

    const GOOD_DIFF: &str = "diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-export const a = 1;\n+export const a = 2;\n";
    const BAD_DIFF: &str = "diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-nope\n+x\n";
    const SPEC_PATH: &str = "e2e/tests/features/SHOP-1.spec.ts";

    fn diff(unified: &str, base: &str) -> ExtractedDiff {
        ExtractedDiff {
            base_commit: base.into(),
            unified_diff: unified.into(),
            changed_paths: vec!["src/a.ts".into()],
        }
    }

    const NODE_PRINT_LEAK: &str =
        "node -e \"console.log('leak=' + (process.env.GUARD_TEST_LEAK_SECRET || 'none'))\"";

    #[test]
    fn checkout_names_are_unique_per_check() {
        let i = input("abc");
        let (a, b) = (checkout_name(&i, "gate"), checkout_name(&i, "gate"));
        assert_ne!(a, b);
        assert!(a.starts_with("run-1-SHOP-1-gate-"), "{a}");
    }

    #[test]
    fn host_checks_run_only_behind_the_explicit_escape_hatch() {
        for v in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("yes"),
            Some(" 2 "),
        ] {
            assert!(!unsafe_host_checks_requested(v), "{v:?}");
        }
        assert!(unsafe_host_checks_requested(Some("1")));
        assert!(unsafe_host_checks_requested(Some(" 1 ")));
        let w = UNSAFE_HOST_CHECKS_WARNING;
        assert!(w.contains(UNSAFE_HOST_CHECKS_ENV), "{w}");
        assert!(w.contains("network") && w.contains("filesystem"), "{w}");
        assert!(w.contains("UNSAFE"), "{w}");
    }

    #[test]
    fn the_candidate_tree_is_the_base_plus_the_diff_plus_the_spec() {
        let (tmp, base) = base_repo();
        let repo = tmp.path().join("repo");
        let scratch = tmp.path().join("scratch");
        let tree = candidate_tree(
            &repo,
            &scratch,
            &base,
            Some(GOOD_DIFF),
            SPEC_PATH,
            b"test('x')",
        )
        .unwrap();
        let files = tar::read(&tree, 1 << 20).unwrap();
        let get = |p: &str| {
            files
                .iter()
                .find(|f| f.path == p)
                .map(|f| f.bytes.clone())
                .unwrap_or_else(|| panic!("{p} missing: {files:?}"))
        };
        assert_eq!(get("src/a.ts"), b"export const a = 2;\n");
        assert_eq!(get(SPEC_PATH), b"test('x')");
        assert_eq!(get("e2e/tests/x.spec.ts"), b"x\n");
        assert!(!files.iter().any(|f| f.path.starts_with(".git")));
        // The host checkout is never touched, and the temporary index is gone.
        assert_eq!(
            git(&repo, &["status", "--porcelain"], None).unwrap(),
            "",
            "worktree and index unchanged"
        );
        assert!(!repo.join(SPEC_PATH).exists());
        let left: Vec<_> = std::fs::read_dir(&scratch)
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
        // The spec alone (red-first) on the base.
        let tree = candidate_tree(&repo, &scratch, &base, None, SPEC_PATH, b"s").unwrap();
        let files = tar::read(&tree, 1 << 20).unwrap();
        let a = files.iter().find(|f| f.path == "src/a.ts").unwrap();
        assert_eq!(a.bytes, b"export const a = 1;\n");
    }

    #[test]
    fn a_diff_that_does_not_apply_is_a_candidate_error() {
        let (tmp, base) = base_repo();
        let repo = tmp.path().join("repo");
        let err = candidate_tree(
            &repo,
            &tmp.path().join("scratch"),
            &base,
            Some(BAD_DIFF),
            SPEC_PATH,
            b"s",
        )
        .unwrap_err();
        let CheckError::Candidate(why) = err else {
            panic!("{err:?}")
        };
        assert!(why.contains("does not apply"), "{why}");
    }

    /// A check sandbox that answers with a scripted outcome.
    struct FakeChecks {
        report: Option<String>,
        app_not_ready: bool,
        created: Mutex<Vec<CheckSpec>>,
        execs: Mutex<Vec<ExecRequest>>,
        destroyed: Mutex<u32>,
    }

    impl FakeChecks {
        fn new(report: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                report: report.map(str::to_string),
                app_not_ready: false,
                created: Mutex::new(vec![]),
                execs: Mutex::new(vec![]),
                destroyed: Mutex::new(0),
            })
        }

        /// The files of the tree copied into the last check.
        fn last_tree(&self) -> Vec<tar::File> {
            let execs = self.execs.lock().unwrap();
            let put = execs
                .iter()
                .rev()
                .find(|e| e.argv.first().map(String::as_str) == Some("tar"))
                .expect("a tree was copied in");
            tar::read(put.stdin.as_deref().unwrap(), 1 << 20).unwrap()
        }

        fn driver(&self) -> ExecRequest {
            self.execs
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|e| e.argv.get(3).map(String::as_str) == Some("guard-check"))
                .cloned()
                .unwrap()
        }
    }

    #[async_trait::async_trait]
    impl guard_core::ports::SandboxProvider for FakeChecks {
        async fn create(
            &self,
            _: &guard_core::ports::SandboxSpec,
        ) -> Result<Sandbox, SandboxError> {
            unreachable!()
        }
        async fn put_file(&self, _: &Sandbox, _: &str, _: Vec<u8>) -> Result<(), SandboxError> {
            unreachable!()
        }
        async fn read_file(&self, _: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
            match (path, &self.report) {
                (REPORT_PATH, Some(r)) => Ok(r.as_bytes().to_vec()),
                _ => Err(SandboxError::NotFound(path.into())),
            }
        }
        async fn exec(&self, _: &Sandbox, r: &ExecRequest) -> Result<ExecOutput, SandboxError> {
            self.execs.lock().unwrap().push(r.clone());
            let driver = r.argv.get(3).map(String::as_str) == Some("guard-check");
            Ok(if driver && self.app_not_ready {
                ExecOutput {
                    exit_code: 90,
                    stdout: b"guard-check: app-not-ready\nboom\n".to_vec(),
                    ..ExecOutput::default()
                }
            } else if driver {
                ExecOutput {
                    stdout: b"guard-check: playwright-exit=1\n".to_vec(),
                    ..ExecOutput::default()
                }
            } else {
                ExecOutput::default()
            })
        }
        async fn extract_diff(&self, _: &Sandbox, _: &str) -> Result<ExtractedDiff, SandboxError> {
            unreachable!()
        }
        async fn destroy(&self, _: &Sandbox) -> Result<(), SandboxError> {
            *self.destroyed.lock().unwrap() += 1;
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl CheckSandboxProvider for FakeChecks {
        async fn create_check(&self, spec: &CheckSpec) -> Result<Sandbox, SandboxError> {
            self.created.lock().unwrap().push(spec.clone());
            Ok(Sandbox {
                id: SandboxId("check".into()),
                role: spec.kind.role(),
                run_id: spec.run_id.clone(),
                ticket: spec.ticket.clone(),
                attempt: 0,
            })
        }
    }

    const IMAGE: &str =
        "guard/sandbox@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn sandboxed(repo: &Path, work: &Path, fake: Arc<FakeChecks>) -> HostChecks {
        let mut checks = HostChecks::new(
            repo,
            "e2e",
            tconfig("node server.mjs"),
            "tok-1".into(),
            CheckIsolation::Sandbox {
                provider: fake,
                image: IMAGE.into(),
            },
        );
        checks.work_root = work.to_path_buf();
        checks
    }

    fn report(status: &str, message: &str) -> String {
        serde_json::json!({
            "errors": [],
            "suites": [{"title": "x.spec.ts", "specs": [{"title": "t", "tests": [
                {"results": [{"status": status, "errors": [{"message": message}]}]}
            ]}]}]
        })
        .to_string()
    }

    #[tokio::test]
    async fn red_first_runs_the_spec_alone_on_the_base_in_a_check_sandbox() {
        let (tmp, base) = base_repo();
        let repo = tmp.path().join("repo");
        let fake = FakeChecks::new(Some(&report(
            "failed",
            "Error: expect(received).toBe(expected)",
        )));
        let checks = sandboxed(&repo, &tmp.path().join("work"), fake.clone());
        let r = checks
            .red_first(&input(&base), SPEC_PATH, b"spec-bytes")
            .await
            .unwrap();
        assert_eq!(r, RedFirst::Failed);
        let created = fake.created.lock().unwrap().clone();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].image, IMAGE);
        assert_eq!(created[0].kind, guard_sandbox::check::CheckKind::RedFirst);
        // The testability token is a forwarded secret, under both names.
        assert_eq!(created[0].secrets["GUARD_TEST_TOKEN"].expose(), "tok-1");
        assert!(created[0].limits.timeout > checks.run_timeout);
        let tree = fake.last_tree();
        let spec = tree.iter().find(|f| f.path == SPEC_PATH).unwrap();
        assert_eq!(spec.bytes, b"spec-bytes");
        let d = fake.driver();
        assert_eq!(
            &d.argv[4..],
            [
                "e2e",
                "tests/features/SHOP-1.spec.ts",
                "--project=chromium",
                "--retries=0"
            ]
        );
        assert_eq!(d.env["GUARD_CHECK_APP_START"], "node server.mjs");
        assert!(!d.env.contains_key("GUARD_TEST_TOKEN"), "never in argv");
        assert!(!d.env.values().any(|v| v.contains("tok-1")));
        assert_eq!(*fake.destroyed.lock().unwrap(), 1);
        // The host checkout is never touched; the report is kept as evidence.
        assert_eq!(git(&repo, &["status", "--porcelain"], None).unwrap(), "");
        let results: Vec<_> = std::fs::read_dir(tmp.path().join("work/check-results"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(results.len(), 1, "{results:?}");
        assert!(results[0].join("report.json").is_file());

        // A passing spec and a missing report.
        let fake = FakeChecks::new(Some(&report("passed", "")));
        let checks = sandboxed(&repo, &tmp.path().join("work"), fake);
        assert_eq!(
            checks
                .red_first(&input(&base), SPEC_PATH, b"s")
                .await
                .unwrap(),
            RedFirst::Passed
        );
        let fake = FakeChecks::new(None);
        let checks = sandboxed(&repo, &tmp.path().join("work"), fake);
        let r = checks
            .red_first(&input(&base), SPEC_PATH, b"s")
            .await
            .unwrap();
        assert!(
            matches!(r, RedFirst::Broken(ref why) if why.contains("no JSON report")),
            "{r:?}"
        );
    }

    #[tokio::test]
    async fn the_gate_runs_diff_and_sealed_spec_in_a_check_sandbox() {
        let (tmp, base) = base_repo();
        let repo = tmp.path().join("repo");
        let sealed = SealedSpec::seal(SPEC_PATH, b"sealed");
        let i = input(&base);
        let fake = FakeChecks::new(Some(&report("passed", "")));
        let checks = sandboxed(&repo, &tmp.path().join("work"), fake.clone());
        let r = checks
            .gate(&i, &diff(GOOD_DIFF, &base), &sealed, b"sealed")
            .await
            .unwrap();
        assert!(r.passed, "{r:?}");
        let tree = fake.last_tree();
        let a = tree.iter().find(|f| f.path == "src/a.ts").unwrap();
        assert_eq!(a.bytes, b"export const a = 2;\n");
        let spec = tree.iter().find(|f| f.path == SPEC_PATH).unwrap();
        assert_eq!(spec.bytes, b"sealed");
        assert_eq!(
            &fake.driver().argv[4..],
            [
                "e2e",
                "tests/features/SHOP-1.spec.ts",
                "tests/smoke",
                "--project=chromium",
                "--retries=0"
            ]
        );
        assert_eq!(
            fake.created.lock().unwrap()[0].kind,
            guard_sandbox::check::CheckKind::Gate
        );

        // A diff that does not apply never reaches a sandbox: a red gate.
        let fake = FakeChecks::new(Some(&report("passed", "")));
        let checks = sandboxed(&repo, &tmp.path().join("work"), fake.clone());
        let r = checks
            .gate(&i, &diff(BAD_DIFF, &base), &sealed, b"sealed")
            .await
            .unwrap();
        assert!(!r.passed);
        assert!(r.failure_context.unwrap().contains("does not apply"));
        assert!(fake.created.lock().unwrap().is_empty());

        // The app never becomes ready: a red gate with its log.
        let fake = Arc::new(FakeChecks {
            app_not_ready: true,
            ..Arc::try_unwrap(FakeChecks::new(None)).ok().unwrap()
        });
        let checks = sandboxed(&repo, &tmp.path().join("work"), fake);
        let r = checks
            .gate(&i, &diff(GOOD_DIFF, &base), &sealed, b"sealed")
            .await
            .unwrap();
        assert!(!r.passed);
        let why = r.failure_context.unwrap();
        assert!(
            why.contains("did not answer") && why.contains("boom"),
            "{why}"
        );
    }

    #[test]
    fn host_checks_never_inherit_orchestrator_secrets() {
        // SAFETY: a variable no other test reads or writes.
        unsafe { std::env::set_var("GUARD_TEST_LEAK_SECRET", "s3cr3t-value") };
        let out = scrub_env(&mut crate::process::shell(NODE_PRINT_LEAK))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        assert!(text.contains("leak=none"), "{text}");
        // The app process gets the same treatment.
        let (tmp, _) = base_repo();
        let err = AppProcess::start(
            &tconfig(NODE_PRINT_LEAK),
            &tmp.path().join("repo"),
            "tok",
            Duration::from_secs(2),
        )
        .err()
        .expect("never ready");
        let CheckError::AppNotReady(why) = err else {
            panic!("{err:?}")
        };
        assert!(
            why.contains("leak=none") && !why.contains("s3cr3t"),
            "{why}"
        );
    }

    #[test]
    fn a_chatty_process_is_drained_not_deadlocked() {
        let child = Command::new("node")
            .args(["-e", "process.stderr.write('x'.repeat(1 << 20))"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (timed_out, stderr) = wait_draining_stderr(child, Duration::from_secs(60)).unwrap();
        assert!(!timed_out);
        assert_eq!(stderr.len(), 1 << 20);
    }

    #[tokio::test]
    async fn on_the_unsafe_host_path_a_broken_candidate_is_a_red_gate() {
        let (tmp, base) = base_repo();
        let repo = tmp.path().join("repo");
        let mut checks = HostChecks::new(
            &repo,
            "e2e",
            tconfig("node -e \"process.exit(1)\""),
            "t".into(),
            CheckIsolation::UnsafeHost,
        );
        checks.work_root = tmp.path().join("work");
        checks.app_ready_timeout = Duration::from_secs(2);
        let sealed = SealedSpec::seal(SPEC_PATH, b"spec");
        let i = input(&base);
        // 1. The patch does not apply.
        let r = checks
            .gate(&i, &diff(BAD_DIFF, &base), &sealed, b"spec")
            .await
            .unwrap();
        assert!(!r.passed);
        assert!(r.failure_context.unwrap().contains("does not apply"));
        // 2. The patch applies but the app never becomes ready.
        let r = checks
            .gate(&i, &diff(GOOD_DIFF, &base), &sealed, b"spec")
            .await
            .unwrap();
        assert!(!r.passed);
        assert!(r.failure_context.unwrap().contains("did not answer"));
        // Nothing left behind.
        let left: Vec<_> = std::fs::read_dir(checks.work_root.join("checks"))
            .map(|d| d.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    /// Regression: `git worktree remove --force` deleted the *contents* of a
    /// linked `node_modules` (through the junction). Removing a check
    /// worktree must only remove the link.
    #[test]
    fn removing_a_check_worktree_never_deletes_through_its_links() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join("e2e")).unwrap();
        std::fs::write(repo.join("e2e/a.txt"), "a").unwrap();
        git(&repo, &["init", "--quiet", "-b", "main"], None).unwrap();
        git(&repo, &["add", "-A"], None).unwrap();
        git(&repo, &["commit", "--quiet", "-m", "base"], None).unwrap();
        let shared = tmp.path().join("shared_node_modules");
        std::fs::create_dir_all(shared.join("pkg")).unwrap();
        std::fs::write(shared.join("pkg/index.js"), "module.exports = 1;\n").unwrap();

        let dir = tmp.path().join("checks/x");
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                &slash(&dir),
                "HEAD",
            ],
            None,
        )
        .unwrap();
        link_dir(&shared, &dir.join("e2e/node_modules")).unwrap();
        assert!(dir.join("e2e/node_modules/pkg/index.js").is_file());
        remove_worktree(&repo, &dir);
        assert!(!dir.exists(), "worktree removed");
        assert!(
            shared.join("pkg/index.js").is_file(),
            "the link target must survive the worktree removal"
        );
    }

    /// The committed bytes of `prefixes` of the real checkout in a fresh repo.
    fn fixture_repo(prefixes: &[&str]) -> (tempfile::TempDir, String) {
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let tmp = tempfile::Builder::new()
            .prefix("guard-check-live-")
            .tempdir()
            .unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let mut args = vec!["archive", "--format=tar", "HEAD", "--"];
        args.extend(prefixes);
        let out = Command::new("git")
            .args(&args)
            .current_dir(real)
            .output()
            .unwrap();
        assert!(out.status.success());
        for f in tar::read(&out.stdout, 256 << 20).unwrap() {
            let path = repo.join(tar::safe_relative(&f.path).unwrap());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, f.bytes).unwrap();
        }
        git(&repo, &["init", "--quiet", "-b", "main"], None).unwrap();
        git(&repo, &["add", "-A"], None).unwrap();
        git(&repo, &["commit", "--quiet", "-m", "base"], None).unwrap();
        let base = git(&repo, &["rev-parse", "HEAD"], None).unwrap();
        (tmp, base)
    }

    /// Live: a sealed spec that reads a host file and fetches example.com and 1.1.1.1
    /// runs in a check sandbox, where both fail (by assertion, so the
    /// verdict shows exactly what the spec saw), while the app under test
    /// answers inside the container.
    #[tokio::test]
    #[ignore = "live Docker: GUARD_DOCKER_TESTS=1, GUARD_SANDBOX_TEST_IMAGE"]
    async fn a_spec_cannot_read_host_files_or_reach_example_com_inside_the_check() {
        if std::env::var("GUARD_DOCKER_TESTS").as_deref() != Ok("1") {
            if std::env::var("GUARD_ALLOW_SKIPPED_LIVE_TESTS").as_deref() == Ok("1") {
                return;
            }
            panic!("GUARD_DOCKER_TESTS=1 is not set (GUARD_ALLOW_SKIPPED_LIVE_TESTS=1 skips)");
        }
        let image = std::env::var("GUARD_SANDBOX_TEST_IMAGE")
            .expect("GUARD_SANDBOX_TEST_IMAGE=<repo>@sha256:<digest>");
        let (tmp, base) = fixture_repo(&["examples/demo-shop", "e2e-playwright"]);
        let repo = tmp.path().join("repo");
        let marker = tmp.path().join("host-secret.txt");
        std::fs::write(&marker, "HOST-ONLY-MARKER").unwrap();
        let win = marker.to_string_lossy().replace('\\', "/");
        let rel = win
            .split_once(':')
            .map_or(win.as_str(), |(_, r)| r)
            .to_string();
        let paths = [
            win.clone(),
            format!("/mnt/c{rel}"),
            format!("/host_mnt/c{rel}"),
            format!("/run/desktop/mnt/host/c{rel}"),
        ];
        let spec = format!(
            r#"import {{ expect, test }} from '@playwright/test';
import {{ readFileSync }} from 'node:fs';

test('reads a host file', async () => {{
  const seen = [];
  for (const p of {paths}) {{
    try {{ seen.push(readFileSync(p, 'utf8')); }} catch (e) {{ seen.push('unreadable ' + e.code); }}
  }}
  expect(seen.join(' | ')).toContain('HOST-ONLY-MARKER');
}});

test('reaches the internet', async () => {{
  const got = [];
  for (const url of ['http://example.com/', 'http://1.1.1.1/']) {{
    try {{
      const r = await fetch(url, {{ signal: AbortSignal.timeout(8000) }});
      got.push('reached ' + r.status);
    }} catch (e) {{
      got.push('blocked ' + (e.cause ? e.cause.code || e.cause.message : e.message));
    }}
  }}
  expect(got.join(' | ')).toContain('reached');
}});

test('the app under test answers inside the check', async ({{ request }}) => {{
  const r = await request.get('/healthz');
  expect(r.status()).toBe(200);
}});
"#,
            paths = serde_json::to_string(&paths).unwrap()
        );
        let real = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let target = std::fs::read_to_string(real.join("target.toml")).unwrap();
        let config = guard_core::config::parse_target(&target).unwrap();
        let provider = Arc::new(guard_sandbox::DockerSandbox::new(
            Arc::new(guard_sandbox::docker::cli::ProcessDockerCli::default()),
            guard_sandbox::DockerConfig {
                scratch_root: tmp.path().join("scratch"),
                ..guard_sandbox::DockerConfig::default()
            },
        ));
        let mut checks = HostChecks::new(
            &repo,
            "e2e-playwright",
            config,
            "live-token".into(),
            CheckIsolation::Sandbox {
                provider: provider.clone(),
                image,
            },
        );
        checks.work_root = tmp.path().join("work");
        checks.gate_extra = vec![];
        let path = "e2e-playwright/tests/features/SHOP-9.spec.ts";
        let sealed = SealedSpec::seal(path, spec.as_bytes());
        let mut i = input(&base);
        i.ticket = "SHOP-9".into();
        let empty = ExtractedDiff {
            base_commit: base.clone(),
            unified_diff: String::new(),
            changed_paths: vec![],
        };
        let r = checks
            .gate(&i, &empty, &sealed, spec.as_bytes())
            .await
            .unwrap();
        println!(
            "[gate] {}\n{}",
            r.summary,
            r.failure_context.clone().unwrap_or_default()
        );
        assert!(!r.passed);
        // The setup project and the health check pass: the app answered.
        assert!(r.summary.contains("2 passed, 2 failed"), "{}", r.summary);
        let why = r.failure_context.unwrap();
        assert!(why.contains("unreadable ENOENT"), "{why}");
        assert!(
            why.contains(&["unreadable ENOENT"; 4].join(" | ")),
            "no candidate path holds the host file: {why}"
        );
        assert!(
            why.contains("Received string:    \"blocked EAI_AGAIN | blocked "),
            "example.com does not resolve and 1.1.1.1 is unreachable: {why}"
        );
        assert_eq!(provider.live_count(), 0, "the check sandbox is destroyed");
        let results: Vec<_> = std::fs::read_dir(tmp.path().join("work/check-results"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert!(results[0].join("report.json").is_file(), "{results:?}");
        assert!(results[0].join("app.log").is_file(), "{results:?}");
    }
}
