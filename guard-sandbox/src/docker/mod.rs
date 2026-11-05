//! `DockerSandbox`: [`SandboxProvider`] on the Docker CLI (docs/contracts/sandbox.md).
//!
//! One `create` builds, for a single (ticket, role, attempt):
//!
//! 1. An **internal** Docker network `<name>-net` (`docker network create
//!    --internal`): no default route, so nothing on it can reach the host or
//!    the internet.
//! 2. One **egress forwarder** container per [`EgressRule`]
//!    (`<name>-egress-<i>`). It starts on the egress network
//!    ([`DockerConfig::egress_network`]), is then attached to the internal
//!    network, and relays TCP on `rule.port` to exactly `rule.host:rule.port`
//!    (`assets/egress-proxy.mjs`, passed with `node -e`, so the forwarder
//!    image needs nothing but Node).
//! 3. The **sandbox** container on the internal network only, with
//!    `--add-host <rule.host>:<forwarder ip>` per rule and `--dns 127.0.0.1`
//!    so no other name resolves (DNS cannot be used as an exfiltration
//!    channel through Docker's embedded resolver). Hardening: `--read-only`
//!    rootfs, size-capped tmpfs at `/tmp` and `$HOME`, `--cap-drop ALL`,
//!    `no-new-privileges`, `--pids-limit`/`--cpus`/`--memory` from
//!    [`ResourceLimits`](guard_core::ports::sandbox::ResourceLimits), a
//!    non-root `--user`, `--init`, and `--runtime=runsc` when the daemon has
//!    gVisor.
//!
//! Mounts follow the role matrix of [`SandboxSpec::for_role`]. A
//! [`MountMode::Hidden`] target is a read-only bind of an empty host
//! directory (the "skeleton") that holds only the mount points of mounts
//! nested under it, so the Coder's `/workspace/<tests>` shows nothing but the
//! read-only sealed spec. Secrets are never in `docker` argv: they are set on
//! the `docker exec` process environment and forwarded by name.
//!
//! `extract_diff` runs git inside the sandbox with a temporary index and
//! object directory under `/tmp`, so it also works on the QA's read-only
//! `/workspace`. The checkout must be a standalone clone (its `.git` a
//! directory), not a `git worktree` whose `.git` file points outside the mount.

pub mod cli;

use crate::check::{CHECK_WORKSPACE, CheckSandboxProvider, CheckSpec};
use crate::docker::cli::{CliOutput, DockerCli, Invocation, ProcessDockerCli};
use guard_core::ports::agent::TASK_DIR;
use guard_core::ports::sandbox::{REQUIRED_ENV, ResourceLimits, WORKSPACE_DIR};
use guard_core::ports::{
    EgressRule, ExecOutput, ExecRequest, ExtractedDiff, Mount, MountMode, Sandbox, SandboxError,
    SandboxId, SandboxProvider, SandboxSpec, Secret,
};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::OnceCell;

/// Source of the egress forwarder, passed with `node --input-type=module -e`.
pub const EGRESS_PROXY_SOURCE: &str = include_str!("../../assets/egress-proxy.mjs");

/// Pinned Node image the forwarders run in (`node:24-bookworm-slim`).
pub const DEFAULT_FORWARDER_IMAGE: &str =
    "node:24-bookworm-slim@sha256:c86ecead9e55a5210ac1727de38a4ca840bf03dcaf34dc9749addbf383b1920c";

/// Label every container and network of a sandbox carries (value: the sandbox name).
pub const LABEL_SANDBOX: &str = "guard.sandbox";
/// Unix seconds after which the reaper removes the sandbox.
pub const LABEL_DEADLINE: &str = "guard.deadline";

const DIFF_DIR: &str = "/tmp/.guard-diff";

/// `$0` of [`KILL_STRAYS_SCRIPT`] (shows up in `ps`, and in tests).
pub const KILL_STRAYS_NAME: &str = "guard-kill-strays";

/// Kills every process in the sandbox except PID 1 (`--init`), the idle
/// `sleep` (the lowest PID whose parent is PID 1: it starts with the
/// container, before any `docker exec`) and the script itself. Three passes,
/// so a process forking during a pass is caught by the next one. Runs before
/// `extract_diff` (no leftover agent process may touch `/workspace` or the
/// temporary index while the diff is computed) and after a timed-out exec
/// (killing the local `docker exec` client leaves its process running).
pub const KILL_STRAYS_SCRIPT: &str = r#"self=$$
keep=
for d in /proc/[0-9]*; do
  p=${d#/proc/}
  pp=
  while read -r k v; do
    if [ "$k" = "PPid:" ]; then pp=$v; break; fi
  done 2>/dev/null < "$d/status"
  if [ "$pp" = 1 ] && { [ -z "$keep" ] || [ "$p" -lt "$keep" ]; }; then keep=$p; fi
done
for pass in 1 2 3; do
  for d in /proc/[0-9]*; do
    p=${d#/proc/}
    case "$p" in
      1|"$self"|"$keep") ;;
      *) kill -9 "$p" 2>/dev/null ;;
    esac
  done
done
exit 0"#;

/// Seconds the idle container outlives the sandbox timeout. After that its
/// `sleep` exits, so a sandbox orphaned by a crash or a lost `destroy` stops
/// by itself (the reaper then removes it).
pub const IDLE_GRACE_SECS: u64 = 300;

/// Destroyed sandbox ids remembered so a repeated `destroy` stays a no-op.
const FORGOTTEN_CAP: usize = 4096;

/// Whether to run the sandbox under gVisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimePreference {
    /// `runsc` when `docker info` lists it, else the default runtime.
    #[default]
    Auto,
    /// Always the daemon's default runtime (runc).
    Default,
    /// `runsc` or `Unavailable`.
    Runsc,
}

#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// Network the forwarders use to reach the allowed hosts (`bridge`, or
    /// the compose network where the gateway lives).
    pub egress_network: String,
    /// Digest-pinned image with `node` on `PATH`.
    pub forwarder_image: String,
    /// `--user` of the sandbox and the forwarders (non-root).
    pub user: String,
    /// `$HOME` inside the sandbox (a tmpfs).
    pub home: String,
    pub tmp_size_mb: u32,
    pub home_size_mb: u32,
    pub runtime: RuntimePreference,
    /// Prefix of every container and network name.
    pub name_prefix: String,
    /// Host directory for the hidden-mount skeletons (must be shareable with
    /// the Docker daemon; on Docker Desktop, under the user profile).
    pub scratch_root: PathBuf,
    pub forwarder_ready_timeout: Duration,
    /// Bound of every `docker` management call (`network create`, `run -d`,
    /// `inspect`, `rm -f`, ...). A hung daemon then fails the call instead
    /// of wedging the worker while its heartbeat keeps the lease alive.
    pub management_timeout: Duration,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            egress_network: "bridge".into(),
            forwarder_image: DEFAULT_FORWARDER_IMAGE.into(),
            user: "1000:1000".into(),
            home: "/home/node".into(),
            tmp_size_mb: 512,
            home_size_mb: 256,
            runtime: RuntimePreference::Auto,
            name_prefix: "guard-sbx".into(),
            scratch_root: std::env::temp_dir().join("guard-sandbox"),
            forwarder_ready_timeout: Duration::from_secs(20),
            management_timeout: Duration::from_secs(300),
        }
    }
}

struct Live {
    mounts: Vec<Mount>,
    secrets: BTreeMap<String, Secret>,
    network: String,
    forwarders: Vec<String>,
    scratch: PathBuf,
    deadline: Instant,
    destroyed: bool,
}

/// Snapshot of a live sandbox's state for one operation.
struct LiveView {
    mounts: Vec<Mount>,
    secrets: BTreeMap<String, Secret>,
    deadline: Instant,
}

/// What one container start needs (an agent sandbox or a check sandbox).
struct Plan<'a> {
    image: &'a str,
    /// `guard.*` labels besides the sandbox name and deadline.
    labels: Vec<(String, String)>,
    limits: &'a ResourceLimits,
    env: &'a BTreeMap<String, String>,
    /// Already materialized (hidden mounts replaced by their skeletons).
    mounts: Vec<Mount>,
    rules: &'a [EgressRule],
    /// `--tmpfs` values besides `/tmp` and `$HOME`.
    extra_tmpfs: Vec<String>,
}

/// One `docker exec`.
struct ExecCall<'a> {
    argv: &'a [String],
    env: &'a [(String, String)],
    secrets: &'a BTreeMap<String, Secret>,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    workdir: &'a str,
}

pub struct DockerSandbox {
    cli: Arc<dyn DockerCli>,
    config: DockerConfig,
    live: Mutex<HashMap<SandboxId, Live>>,
    /// Ids fully removed (bounded): a repeated `destroy` is `Ok`.
    forgotten: Mutex<std::collections::VecDeque<SandboxId>>,
    runsc: OnceCell<bool>,
    counter: AtomicU64,
}

impl DockerSandbox {
    pub fn new(cli: Arc<dyn DockerCli>, config: DockerConfig) -> Self {
        Self {
            cli,
            config,
            live: Mutex::new(HashMap::new()),
            forgotten: Mutex::new(std::collections::VecDeque::new()),
            runsc: OnceCell::new(),
            counter: AtomicU64::new(0),
        }
    }

    /// The `docker` binary on `PATH` with the default config.
    pub fn with_docker_cli() -> Self {
        Self::new(
            Arc::new(ProcessDockerCli::default()),
            DockerConfig::default(),
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SandboxId, Live>> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sandboxes still tracked (live, or destroyed with a failed removal).
    pub fn live_count(&self) -> usize {
        self.lock().len()
    }

    async fn docker(&self, mut inv: Invocation) -> Result<CliOutput, SandboxError> {
        if inv.timeout.is_none() {
            inv.timeout = Some(self.config.management_timeout);
        }
        let out = self.cli.run(inv).await?;
        if out.timed_out {
            return Err(SandboxError::Unavailable(format!(
                "docker did not answer within {}s",
                self.config.management_timeout.as_secs()
            )));
        }
        Ok(out)
    }

    async fn docker_ok(&self, args: Vec<String>, what: &str) -> Result<CliOutput, SandboxError> {
        let out = self.docker(Invocation::new(args)).await?;
        if out.success() {
            Ok(out)
        } else {
            Err(SandboxError::Unavailable(format!(
                "{what}: {}",
                out.stderr_str().trim()
            )))
        }
    }

    fn next_name(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let nonce = nanos ^ (u64::from(std::process::id()) << 32) ^ n.rotate_left(48);
        format!("{}-{nonce:016x}", self.config.name_prefix)
    }

    async fn use_runsc(&self) -> Result<bool, SandboxError> {
        match self.config.runtime {
            RuntimePreference::Default => Ok(false),
            pref => {
                let has = *self
                    .runsc
                    .get_or_try_init(|| async {
                        let out = self
                            .docker_ok(
                                vec![
                                    "info".into(),
                                    "--format".into(),
                                    "{{json .Runtimes}}".into(),
                                ],
                                "docker info",
                            )
                            .await?;
                        Ok::<bool, SandboxError>(out.stdout_str().contains("\"runsc\""))
                    })
                    .await?;
                if pref == RuntimePreference::Runsc && !has {
                    return Err(SandboxError::Unavailable(
                        "runtime runsc requested but the daemon does not list it".into(),
                    ));
                }
                Ok(has)
            }
        }
    }

    /// Builds the skeleton directories of hidden mounts and returns the mount
    /// list with each hidden source replaced by its skeleton.
    fn materialize_mounts(
        &self,
        mounts: &[Mount],
        scratch: &Path,
    ) -> Result<Vec<Mount>, SandboxError> {
        let io = |e: std::io::Error| SandboxError::Io(format!("sandbox scratch: {e}"));
        let mut out = Vec::with_capacity(mounts.len());
        for (i, m) in mounts.iter().enumerate() {
            match m.mode {
                MountMode::Hidden => {
                    let skel = scratch.join(format!("hidden-{i}"));
                    std::fs::create_dir_all(&skel).map_err(io)?;
                    let prefix = format!("{}/", m.target.trim_end_matches('/'));
                    for nested in mounts.iter().filter(|n| n.target.starts_with(&prefix)) {
                        let rel = &nested.target[prefix.len()..];
                        let point = skel.join(rel);
                        if nested.mode != MountMode::Hidden && Path::new(&nested.source).is_file() {
                            if let Some(parent) = point.parent() {
                                std::fs::create_dir_all(parent).map_err(io)?;
                            }
                            std::fs::write(&point, b"").map_err(io)?;
                        } else {
                            std::fs::create_dir_all(&point).map_err(io)?;
                        }
                    }
                    out.push(Mount {
                        source: skel.to_string_lossy().into_owned(),
                        target: m.target.clone(),
                        mode: MountMode::Hidden,
                    });
                }
                MountMode::ReadWrite => {
                    // A missing rw source (e.g. a fresh `tests/features`) would
                    // make Docker fail on the bind; create it on the host.
                    if !Path::new(&m.source).exists() {
                        std::fs::create_dir_all(&m.source).map_err(io)?;
                    }
                    out.push(m.clone());
                }
                MountMode::ReadOnly => out.push(m.clone()),
            }
        }
        Ok(out)
    }

    async fn start_forwarder(
        &self,
        name: &str,
        index: usize,
        rule: &EgressRule,
        network: &str,
        labels: &[String],
    ) -> Result<(String, String), SandboxError> {
        let fwd = format!("{name}-egress-{index}");
        let mut args: Vec<String> = vec!["run".into(), "-d".into(), "--name".into(), fwd.clone()];
        args.extend(labels.iter().cloned());
        args.extend(
            [
                "--network",
                &self.config.egress_network,
                "--read-only",
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--user",
                &self.config.user,
                "--pids-limit",
                "64",
                "--memory",
                "128m",
                "-e",
                &format!("GUARD_EGRESS_TARGET_HOST={}", rule.host),
                "-e",
                &format!("GUARD_EGRESS_TARGET_PORT={}", rule.port),
                "-e",
                &format!("GUARD_EGRESS_LISTEN_PORT={}", rule.port),
                &self.config.forwarder_image,
                "node",
                "--input-type=module",
                "-e",
                EGRESS_PROXY_SOURCE,
            ]
            .map(String::from),
        );
        self.docker_ok(args, "start egress forwarder").await?;
        self.docker_ok(
            vec![
                "network".into(),
                "connect".into(),
                network.into(),
                fwd.clone(),
            ],
            "attach egress forwarder",
        )
        .await?;
        let ip = self
            .docker_ok(
                vec![
                    "inspect".into(),
                    "-f".into(),
                    format!("{{{{(index .NetworkSettings.Networks \"{network}\").IPAddress}}}}"),
                    fwd.clone(),
                ],
                "inspect egress forwarder",
            )
            .await?
            .stdout_str()
            .trim()
            .to_string();
        if ip.is_empty() {
            return Err(SandboxError::Unavailable(format!(
                "{fwd} has no address on {network}"
            )));
        }
        let started = Instant::now();
        loop {
            let logs = self
                .docker(Invocation::new(["logs".to_string(), fwd.clone()]))
                .await?;
            if logs.stdout_str().contains("guard-egress ready") {
                break;
            }
            if started.elapsed() >= self.config.forwarder_ready_timeout {
                return Err(SandboxError::Unavailable(format!(
                    "{fwd} not ready: {}",
                    logs.stderr_str().trim()
                )));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        Ok((fwd, ip))
    }

    /// Removes the containers, the network and the scratch dir; every step
    /// runs, and the failures (an object that is already gone is not one)
    /// are reported together.
    async fn remove_all(
        &self,
        name: &str,
        forwarders: &[String],
        network: &str,
        scratch: &Path,
    ) -> Result<(), SandboxError> {
        let mut failures = Vec::new();
        let mut step = |what: String, r: Result<CliOutput, SandboxError>| match r {
            Ok(out) if out.success() => {}
            Ok(out) if out.stderr_str().contains("No such") => {}
            Ok(out) => failures.push(format!("{what}: {}", out.stderr_str().trim())),
            Err(e) => failures.push(format!("{what}: {e}")),
        };
        for c in std::iter::once(name.to_string()).chain(forwarders.iter().cloned()) {
            let r = self
                .docker(Invocation::new(["rm", "-f", "-v", c.as_str()]))
                .await;
            step(format!("rm {c}"), r);
        }
        let r = self
            .docker(Invocation::new(["network", "rm", network]))
            .await;
        step(format!("network rm {network}"), r);
        match std::fs::remove_dir_all(scratch) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failures.push(format!("remove {}: {e}", scratch.display())),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(SandboxError::Unavailable(failures.join("; ")))
        }
    }

    /// Best effort: [`KILL_STRAYS_SCRIPT`] in the sandbox.
    async fn kill_strays(&self, sandbox: &Sandbox) {
        let argv: Vec<String> = ["sh", "-c", KILL_STRAYS_SCRIPT, KILL_STRAYS_NAME]
            .map(String::from)
            .to_vec();
        let _ = self
            .exec_raw(
                sandbox,
                ExecCall {
                    argv: &argv,
                    env: &[],
                    secrets: &BTreeMap::new(),
                    stdin: None,
                    timeout: Duration::from_secs(30),
                    workdir: "/",
                },
            )
            .await;
    }

    async fn try_create(
        &self,
        spec: &SandboxSpec,
        name: &str,
        network: &str,
        scratch: &Path,
        forwarders: &mut Vec<String>,
    ) -> Result<(), SandboxError> {
        let mounts = self.materialize_mounts(&spec.mounts, scratch)?;
        let l = &spec.layout;
        let plan = Plan {
            image: &spec.image,
            labels: vec![
                ("guard.run_id".into(), l.run_id.clone()),
                ("guard.ticket".into(), l.ticket.clone()),
                ("guard.role".into(), spec.role.as_str().into()),
                ("guard.attempt".into(), l.attempt.to_string()),
            ],
            limits: &spec.limits,
            env: &spec.env,
            mounts,
            rules: &spec.egress.rules,
            extra_tmpfs: vec![],
        };
        self.launch(&plan, name, network, forwarders).await
    }

    /// The internal network, one forwarder per egress rule, and the idle
    /// sandbox container of `plan`.
    async fn launch(
        &self,
        plan: &Plan<'_>,
        name: &str,
        network: &str,
        forwarders: &mut Vec<String>,
    ) -> Result<(), SandboxError> {
        let runsc = self.use_runsc().await?;
        let deadline_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + plan.limits.timeout.as_secs();
        let labels: Vec<String> = [
            format!("{LABEL_SANDBOX}={name}"),
            format!("{LABEL_DEADLINE}={deadline_unix}"),
        ]
        .into_iter()
        .flat_map(|l| ["--label".to_string(), l])
        .collect();

        let mut net_args: Vec<String> =
            vec!["network".into(), "create".into(), "--internal".into()];
        net_args.extend(labels.iter().cloned());
        net_args.push(network.into());
        self.docker_ok(net_args, "create internal network").await?;

        let mut hosts = Vec::new();
        for (i, rule) in plan.rules.iter().enumerate() {
            forwarders.push(format!("{name}-egress-{i}"));
            let (_, ip) = self
                .start_forwarder(name, i, rule, network, &labels)
                .await?;
            hosts.push(format!("{}:{ip}", rule.host));
        }

        let mut args: Vec<String> = vec![
            "run".into(),
            "-d".into(),
            "--init".into(),
            "--name".into(),
            name.into(),
            "--hostname".into(),
            "sandbox".into(),
        ];
        args.extend(labels);
        for (k, v) in &plan.labels {
            args.push("--label".into());
            args.push(format!("{k}={v}"));
        }
        let lim = plan.limits;
        args.extend(
            [
                "--network",
                network,
                "--dns",
                "127.0.0.1",
                "--read-only",
                "--tmpfs",
                &format!(
                    "/tmp:rw,nosuid,nodev,size={}m,mode=1777",
                    self.config.tmp_size_mb
                ),
                "--tmpfs",
                &format!(
                    "{}:rw,nosuid,nodev,size={}m,mode=1777",
                    self.config.home, self.config.home_size_mb
                ),
            ]
            .map(String::from),
        );
        for t in &plan.extra_tmpfs {
            args.push("--tmpfs".into());
            args.push(t.clone());
        }
        args.extend(
            [
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--pids-limit",
                &lim.pids.to_string(),
                "--cpus",
                &lim.cpus.to_string(),
                "--memory",
                &format!("{}m", lim.memory_mb),
                "--memory-swap",
                &format!("{}m", lim.memory_mb),
                "--user",
                &self.config.user,
                "--workdir",
                WORKSPACE_DIR,
            ]
            .map(String::from),
        );
        if runsc {
            args.push("--runtime=runsc".into());
        }
        for h in hosts {
            args.push("--add-host".into());
            args.push(h);
        }
        args.push("-e".into());
        args.push(format!("HOME={}", self.config.home));
        for (k, v) in plan.env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        for m in &plan.mounts {
            args.push("--mount".into());
            args.push(mount_arg(
                &m.source,
                &m.target,
                m.mode != MountMode::ReadWrite,
            )?);
        }
        // The image's own entrypoint never runs: the container only idles and
        // every agent process starts through `docker exec`. The idle `sleep`
        // ends shortly after the sandbox timeout, so an orphaned sandbox
        // (crash, cancelled future) stops by itself.
        args.extend([
            "--entrypoint".into(),
            "sleep".into(),
            plan.image.to_string(),
            (plan.limits.timeout.as_secs() + IDLE_GRACE_SECS).to_string(),
        ]);
        self.docker_ok(args, "start sandbox").await?;
        Ok(())
    }

    /// A check sandbox (`crate::check`): no mounts, no egress rule, a tmpfs
    /// workspace; the same network isolation and hardening as an agent
    /// sandbox. The forwarder image is not needed (there is no forwarder).
    pub async fn create_check(&self, spec: &CheckSpec) -> Result<Sandbox, SandboxError> {
        spec.validate().map_err(SandboxError::InvalidSpec)?;
        let name = self.next_name();
        let network = format!("{name}-net");
        let scratch = self.config.scratch_root.join(&name);
        let env: BTreeMap<String, String> = REQUIRED_ENV
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let role = spec.kind.role();
        let plan = Plan {
            image: &spec.image,
            labels: vec![
                ("guard.run_id".into(), spec.run_id.clone()),
                ("guard.ticket".into(), spec.ticket.clone()),
                ("guard.role".into(), role.as_str().into()),
                ("guard.check".into(), spec.kind.as_str().into()),
            ],
            limits: &spec.limits,
            env: &env,
            mounts: vec![],
            rules: &[],
            extra_tmpfs: vec![format!(
                "{CHECK_WORKSPACE}:rw,nosuid,nodev,size={}m,mode=1777",
                spec.workspace_size_mb
            )],
        };
        let mut forwarders = Vec::new();
        if let Err(e) = self.launch(&plan, &name, &network, &mut forwarders).await {
            let _ = self
                .remove_all(&name, &forwarders, &network, &scratch)
                .await;
            return Err(e);
        }
        let id = SandboxId(name);
        self.lock().insert(
            id.clone(),
            Live {
                mounts: vec![],
                secrets: spec.secrets.clone(),
                network,
                forwarders,
                scratch,
                deadline: Instant::now() + spec.limits.timeout,
                destroyed: false,
            },
        );
        Ok(Sandbox {
            id,
            role,
            run_id: spec.run_id.clone(),
            ticket: spec.ticket.clone(),
            attempt: 0,
        })
    }

    fn checked(&self, sandbox: &Sandbox) -> Result<LiveView, SandboxError> {
        let live = self.lock();
        match live.get(&sandbox.id) {
            Some(l) if !l.destroyed => Ok(LiveView {
                mounts: l.mounts.clone(),
                secrets: l.secrets.clone(),
                deadline: l.deadline,
            }),
            _ => Err(SandboxError::NotFound(sandbox.id.0.clone())),
        }
    }

    async fn exec_raw(
        &self,
        sandbox: &Sandbox,
        call: ExecCall<'_>,
    ) -> Result<CliOutput, SandboxError> {
        let ExecCall {
            argv,
            env,
            secrets,
            stdin,
            timeout,
            workdir,
        } = call;
        let mut args: Vec<String> = vec!["exec".into()];
        if stdin.is_some() {
            args.push("-i".into());
        }
        args.extend(["-w".into(), workdir.into()]);
        for (k, v) in env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        for name in secrets.keys() {
            args.push("-e".into());
            args.push(name.clone());
        }
        args.push(sandbox.id.0.clone());
        args.extend(argv.iter().cloned());
        // Not `docker()`: a timed-out exec is a result, not an error.
        self.cli
            .run(Invocation {
                args,
                secret_env: secrets
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                stdin,
                timeout: Some(timeout),
            })
            .await
    }

    /// Removes every guard sandbox container (and its network) whose
    /// `guard.deadline` label is before now. Returns the removed containers.
    pub async fn reap_expired(&self) -> Result<Vec<String>, SandboxError> {
        let ps = self
            .docker_ok(
                vec![
                    "ps".into(),
                    "-a".into(),
                    "--filter".into(),
                    format!("label={LABEL_SANDBOX}"),
                    "--format".into(),
                    format!(
                        "{{{{.Names}}}}\t{{{{.Label \"{LABEL_DEADLINE}\"}}}}\t{{{{.Label \"{LABEL_SANDBOX}\"}}}}"
                    ),
                ],
                "list sandboxes",
            )
            .await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let expired = expired_containers(&ps.stdout_str(), now);
        let mut networks = std::collections::BTreeSet::new();
        let mut removed = Vec::new();
        for (container, sandbox) in expired {
            let _ = self
                .docker(Invocation::new(["rm", "-f", "-v", container.as_str()]))
                .await;
            networks.insert(format!("{sandbox}-net"));
            removed.push(container);
        }
        for n in networks {
            let _ = self
                .docker(Invocation::new(["network", "rm", n.as_str()]))
                .await;
        }
        Ok(removed)
    }
}

/// `(container, sandbox)` pairs of `docker ps` lines
/// (`name\tdeadline\tsandbox`) whose deadline is before `now`.
pub fn expired_containers(ps: &str, now: u64) -> Vec<(String, String)> {
    ps.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let (name, deadline, sandbox) = (f.next()?, f.next()?, f.next()?);
            let deadline: u64 = deadline.trim().parse().ok()?;
            (!name.is_empty() && !sandbox.is_empty() && deadline < now)
                .then(|| (name.to_string(), sandbox.to_string()))
        })
        .collect()
}

/// One `--mount` value. Rejects sources that would break its CSV syntax.
pub fn mount_arg(source: &str, target: &str, readonly: bool) -> Result<String, SandboxError> {
    for s in [source, target] {
        if s.is_empty() || s.contains([',', '"', '\n', '\r']) {
            return Err(SandboxError::InvalidSpec(format!(
                "mount path {s:?} is empty or contains , \" or a newline"
            )));
        }
    }
    let mut arg = format!("type=bind,source={source},target={target}");
    if readonly {
        arg.push_str(",readonly");
    }
    Ok(arg)
}

pub(crate) fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_commit(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_clean_absolute(path: &str) -> bool {
    path.starts_with('/') && !path.contains('\0') && !path.split('/').any(|s| s == ".." || s == ".")
}

fn under(path: &str, dir: &str) -> bool {
    path == dir || path.starts_with(&format!("{}/", dir.trim_end_matches('/')))
}

/// The mount that governs `path`: the deepest target containing it.
fn governing_mount<'a>(mounts: &'a [Mount], path: &str) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|m| under(path, &m.target))
        .max_by_key(|m| m.target.len())
}

#[async_trait::async_trait]
impl SandboxProvider for DockerSandbox {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox, SandboxError> {
        spec.validate().map_err(SandboxError::InvalidSpec)?;
        let forwarder_digest_ok = self
            .config
            .forwarder_image
            .split_once("@sha256:")
            .is_some_and(|(r, d)| !r.is_empty() && d.len() == 64);
        if !forwarder_digest_ok {
            return Err(SandboxError::InvalidSpec(
                "the forwarder image must be pinned by digest".into(),
            ));
        }
        for rule in &spec.egress.rules {
            if !rule
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
                || rule.port == 0
            {
                return Err(SandboxError::InvalidSpec(format!(
                    "egress rule {}:{} is not a plain host and port",
                    rule.host, rule.port
                )));
            }
        }
        for m in &spec.mounts {
            if m.mode != MountMode::Hidden {
                mount_arg(&m.source, &m.target, true)?;
            }
        }
        let name = self.next_name();
        let network = format!("{name}-net");
        let scratch = self.config.scratch_root.join(&name);
        let mut forwarders = Vec::new();
        if let Err(e) = self
            .try_create(spec, &name, &network, &scratch, &mut forwarders)
            .await
        {
            // The labelled leftovers of a failed cleanup go to the reaper.
            let _ = self
                .remove_all(&name, &forwarders, &network, &scratch)
                .await;
            return Err(e);
        }
        let id = SandboxId(name);
        self.lock().insert(
            id.clone(),
            Live {
                mounts: spec.mounts.clone(),
                secrets: spec.secrets.clone(),
                network,
                forwarders,
                scratch,
                deadline: Instant::now() + spec.limits.timeout,
                destroyed: false,
            },
        );
        Ok(Sandbox {
            id,
            role: spec.role,
            run_id: spec.layout.run_id.clone(),
            ticket: spec.layout.ticket.clone(),
            attempt: spec.layout.attempt,
        })
    }

    async fn put_file(
        &self,
        sandbox: &Sandbox,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<(), SandboxError> {
        let LiveView {
            mounts, deadline, ..
        } = self.checked(sandbox)?;
        let writable = is_clean_absolute(path)
            && governing_mount(&mounts, path).is_some_and(|m| m.mode == MountMode::ReadWrite)
            && path != TASK_DIR;
        if !writable {
            return Err(SandboxError::Io(format!(
                "{path} is not inside a writable mount"
            )));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let out = self
            .exec_raw(
                sandbox,
                ExecCall {
                    argv: &["dd".into(), format!("of={path}"), "status=none".into()],
                    env: &[],
                    secrets: &BTreeMap::new(),
                    stdin: Some(bytes),
                    timeout: remaining.max(Duration::from_secs(1)),
                    workdir: "/",
                },
            )
            .await?;
        if out.success() {
            Ok(())
        } else {
            Err(SandboxError::Io(format!(
                "write {path}: {}",
                out.stderr_str().trim()
            )))
        }
    }

    async fn read_file(&self, sandbox: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
        let LiveView { deadline, .. } = self.checked(sandbox)?;
        if !is_clean_absolute(path) {
            return Err(SandboxError::Io(format!(
                "{path} is not a clean absolute path"
            )));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        let out = self
            .exec_raw(
                sandbox,
                ExecCall {
                    argv: &["cat".into(), "--".into(), path.into()],
                    env: &[],
                    secrets: &BTreeMap::new(),
                    stdin: None,
                    timeout: remaining.max(Duration::from_secs(1)),
                    workdir: "/",
                },
            )
            .await?;
        if out.success() {
            Ok(out.stdout)
        } else if out.stderr_str().contains("No such file") {
            Err(SandboxError::NotFound(path.to_string()))
        } else {
            Err(SandboxError::Io(format!(
                "read {path}: {}",
                out.stderr_str().trim()
            )))
        }
    }

    async fn exec(
        &self,
        sandbox: &Sandbox,
        request: &ExecRequest,
    ) -> Result<ExecOutput, SandboxError> {
        let LiveView {
            secrets, deadline, ..
        } = self.checked(sandbox)?;
        if request.argv.is_empty() {
            return Err(SandboxError::Exec("empty argv".into()));
        }
        if let Some(bad) = request.env.keys().find(|k| !is_env_name(k)) {
            return Err(SandboxError::Exec(format!("env name {bad:?} is invalid")));
        }
        let workdir = request.workdir.as_deref().unwrap_or(WORKSPACE_DIR);
        if !is_clean_absolute(workdir) {
            return Err(SandboxError::Exec(format!(
                "workdir {workdir:?} must be absolute"
            )));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(ExecOutput {
                exit_code: -1,
                timed_out: true,
                ..ExecOutput::default()
            });
        }
        let timeout = request.timeout.map_or(remaining, |t| t.min(remaining));
        let env: Vec<(String, String)> = request
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let out = self
            .exec_raw(
                sandbox,
                ExecCall {
                    argv: &request.argv,
                    env: &env,
                    secrets: &secrets,
                    stdin: request.stdin.clone(),
                    timeout,
                    workdir,
                },
            )
            .await?;
        if out.timed_out {
            // Only the local `docker exec` client was killed.
            self.kill_strays(sandbox).await;
        }
        Ok(ExecOutput {
            exit_code: out.status.unwrap_or(-1),
            stdout: out.stdout,
            stderr: out.stderr,
            timed_out: out.timed_out,
        })
    }

    async fn extract_diff(
        &self,
        sandbox: &Sandbox,
        base_commit: &str,
    ) -> Result<ExtractedDiff, SandboxError> {
        let LiveView {
            mounts, deadline, ..
        } = self.checked(sandbox)?;
        if !is_commit(base_commit) {
            return Err(SandboxError::Exec(format!(
                "base_commit {base_commit:?} is not a lowercase hex sha"
            )));
        }
        // Overlay mounts under /workspace (hidden tests dir, sealed spec) are
        // not the worktree's content: they would read as deletions/no-ops.
        let ws_prefix = format!("{WORKSPACE_DIR}/");
        let mut excludes: Vec<String> = mounts
            .iter()
            .filter(|m| m.target.starts_with(&ws_prefix) && m.mode != MountMode::ReadWrite)
            .map(|m| format!(":(exclude,literal){}", &m.target[ws_prefix.len()..]))
            .collect();
        excludes.sort();
        let env: Vec<(String, String)> = [
            ("GIT_INDEX_FILE", format!("{DIFF_DIR}/index")),
            ("GIT_OBJECT_DIRECTORY", format!("{DIFF_DIR}/objects")),
            (
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                format!("{WORKSPACE_DIR}/.git/objects"),
            ),
            // The agent controls `$HOME/.gitconfig`, `/workspace/.git/config`
            // and `refs/replace/*`: none may change which base is diffed or
            // how the patch headers name paths (the host applies the patch).
            ("GIT_CONFIG_NOSYSTEM", "1".into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_NO_REPLACE_OBJECTS", "1".into()),
            ("GIT_ATTR_NOSYSTEM", "1".into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let git = |rest: &[&str]| -> Vec<String> {
            [
                "git",
                "-c",
                "safe.directory=*",
                "-c",
                "core.quotePath=false",
                "-c",
                "diff.noprefix=false",
                "-c",
                "diff.mnemonicPrefix=false",
                "-c",
                "diff.relative=false",
                "-c",
                "diff.srcPrefix=a/",
                "-c",
                "diff.dstPrefix=b/",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ]
            .iter()
            .chain(rest)
            .map(|s| s.to_string())
            .collect()
        };
        let mut add = git(&["add", "-A", "--", "."]);
        add.extend(excludes);
        let steps: Vec<Vec<String>> = vec![
            vec!["rm".into(), "-rf".into(), DIFF_DIR.into()],
            vec!["mkdir".into(), "-p".into(), format!("{DIFF_DIR}/objects")],
            git(&["read-tree", base_commit]),
            add,
            git(&[
                "diff",
                "--cached",
                "--no-renames",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                base_commit,
            ]),
            git(&[
                "diff",
                "--cached",
                "--no-renames",
                "--name-only",
                "-z",
                base_commit,
            ]),
        ];
        // No leftover agent process may change the workspace or swap the
        // temporary index between the steps below.
        self.kill_strays(sandbox).await;
        let mut outputs = Vec::new();
        for argv in &steps {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let out = self
                .exec_raw(
                    sandbox,
                    ExecCall {
                        argv,
                        env: &env,
                        secrets: &BTreeMap::new(),
                        stdin: None,
                        timeout: remaining.max(Duration::from_secs(1)),
                        workdir: WORKSPACE_DIR,
                    },
                )
                .await?;
            if !out.success() {
                return Err(SandboxError::Exec(format!(
                    "{}: {}",
                    argv.join(" "),
                    out.stderr_str().trim()
                )));
            }
            outputs.push(out);
        }
        let _ = self
            .exec_raw(
                sandbox,
                ExecCall {
                    argv: &["rm".into(), "-rf".into(), DIFF_DIR.into()],
                    env: &[],
                    secrets: &BTreeMap::new(),
                    stdin: None,
                    timeout: Duration::from_secs(30),
                    workdir: "/",
                },
            )
            .await;
        let names = outputs.pop().expect("name-only step").stdout_str();
        let unified_diff = outputs.pop().expect("diff step").stdout_str();
        let mut changed_paths: Vec<String> = names
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        changed_paths.sort();
        changed_paths.dedup();
        Ok(ExtractedDiff {
            base_commit: base_commit.to_string(),
            unified_diff,
            changed_paths,
        })
    }

    async fn destroy(&self, sandbox: &Sandbox) -> Result<(), SandboxError> {
        let (forwarders, network, scratch) = {
            let mut live = self.lock();
            let Some(l) = live.get_mut(&sandbox.id) else {
                let forgotten = self.forgotten.lock().unwrap_or_else(|e| e.into_inner());
                return if forgotten.contains(&sandbox.id) {
                    Ok(())
                } else {
                    Err(SandboxError::NotFound(sandbox.id.0.clone()))
                };
            };
            // A destroyed entry still here is a failed removal: retry it.
            l.destroyed = true;
            l.secrets.clear();
            (l.forwarders.clone(), l.network.clone(), l.scratch.clone())
        };
        self.remove_all(&sandbox.id.0, &forwarders, &network, &scratch)
            .await?;
        self.lock().remove(&sandbox.id);
        let mut forgotten = self.forgotten.lock().unwrap_or_else(|e| e.into_inner());
        if forgotten.len() >= FORGOTTEN_CAP {
            forgotten.pop_front();
        }
        forgotten.push_back(sandbox.id.clone());
        Ok(())
    }
}

#[async_trait::async_trait]
impl CheckSandboxProvider for DockerSandbox {
    async fn create_check(&self, spec: &CheckSpec) -> Result<Sandbox, SandboxError> {
        DockerSandbox::create_check(self, spec).await
    }
}

#[cfg(test)]
mod tests;
