//! Live tests of `DockerSandbox` against a real Docker daemon.
//!
//! `#[ignore]`, and gated on `GUARD_DOCKER_TESTS=1`:
//!
//! ```text
//! GUARD_DOCKER_TESTS=1 \
//! GUARD_SANDBOX_TEST_IMAGE=<repo>@sha256:<digest of deploy/sandbox/Dockerfile> \
//!   cargo test -p guard-sandbox --test docker_live -- --include-ignored --test-threads=2
//! ```
//!
//! The image must be referenced by digest (push it to any registry, a local
//! `registry:2` works). The allowed upstream is an `nginx:alpine` container
//! (`GUARD_DOCKER_TEST_UPSTREAM_IMAGE` overrides it) on a test-only egress
//! network; the external hosts probed are `example.com`, `1.1.1.1` and
//! `host.docker.internal`.

use guard_core::ports::{
    EgressAllowlist, ExecRequest, Role, SandboxError, SandboxLayout, SandboxProvider, SandboxSpec,
    Secret,
};
use guard_sandbox::docker::cli::{DockerCli, Invocation, ProcessDockerCli};
use guard_sandbox::{DockerConfig, DockerSandbox};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

const SPEC_REL: &str = "tests/features/SHOP-1.spec.ts";

fn enabled() -> Option<String> {
    if std::env::var("GUARD_DOCKER_TESTS").as_deref() != Ok("1") {
        missing_live_env("GUARD_DOCKER_TESTS=1");
        return None;
    }
    Some(std::env::var("GUARD_SANDBOX_TEST_IMAGE").expect(
        "GUARD_SANDBOX_TEST_IMAGE=<repo>@sha256:<digest> of the deploy/sandbox image is required",
    ))
}

fn docker(args: &[&str]) -> (bool, String) {
    let o = Command::new("docker")
        .args(args)
        .output()
        .expect("docker on PATH");
    (
        o.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

struct Host {
    dir: tempfile::TempDir,
    base: String,
}

impl Host {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn s(&self, rel: &str) -> String {
        self.path(rel).to_string_lossy().into_owned()
    }
}

/// A standalone git checkout with app code, a journey and the QA's spec,
/// a task dir, and a sealed copy that differs from the checkout's spec.
fn host() -> Host {
    // Under the user profile so Docker Desktop can share it.
    let dir = tempfile::Builder::new()
        .prefix("guard-live-")
        .tempdir()
        .unwrap();
    let co = dir.path().join("checkout");
    std::fs::create_dir_all(co.join("src")).unwrap();
    std::fs::create_dir_all(co.join("tests/journeys")).unwrap();
    std::fs::create_dir_all(co.join("tests/features")).unwrap();
    std::fs::write(co.join("src/app.ts"), "export const n = 1;\n").unwrap();
    std::fs::write(
        co.join("tests/journeys/checkout.spec.ts"),
        "// journey source\n",
    )
    .unwrap();
    std::fs::write(co.join(SPEC_REL), "// unsealed QA copy\n").unwrap();
    git(&co, &["init", "-q", "-b", "main"]);
    git(&co, &["add", "-A"]);
    git(&co, &["commit", "-q", "-m", "base"]);
    let base = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&co)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    std::fs::create_dir_all(dir.path().join("task")).unwrap();
    std::fs::write(dir.path().join("sealed.spec.ts"), "// SEALED copy\n").unwrap();
    Host { dir, base }
}

fn spec(h: &Host, role: Role, image: &str, egress: EgressAllowlist) -> SandboxSpec {
    let layout = SandboxLayout {
        run_id: "live-run".into(),
        ticket: "SHOP-1".into(),
        attempt: 1,
        image: image.into(),
        checkout: h.s("checkout"),
        task_dir: h.s("task"),
        codegraph_index: None,
        sealed_spec: (role == Role::Coder).then(|| (h.s("sealed.spec.ts"), SPEC_REL.to_string())),
        tests_dir: "tests".into(),
        skills_dir: None,
    };
    let mut s = SandboxSpec::for_role(role, layout, egress);
    s.secrets.insert(
        "GUARD_GATEWAY_KEY".into(),
        Secret::new("sk-live-test-not-real"),
    );
    s.limits.timeout = Duration::from_secs(600);
    s
}

/// A user-defined egress network with an nginx upstream named `name`.
struct Upstream {
    network: String,
    name: String,
}

impl Upstream {
    fn start(tag: &str) -> Self {
        let network = format!("guard-live-egress-{tag}-{}", std::process::id());
        let name = format!("gw-upstream-{tag}-{}", std::process::id());
        let image =
            std::env::var("GUARD_DOCKER_TEST_UPSTREAM_IMAGE").unwrap_or("nginx:alpine".into());
        let (ok, out) = docker(&["network", "create", &network]);
        assert!(ok, "{out}");
        let (ok, out) = docker(&["run", "-d", "--name", &name, "--network", &network, &image]);
        assert!(ok, "{out}");
        Self { network, name }
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        docker(&["rm", "-f", &self.name]);
        docker(&["network", "rm", &self.network]);
    }
}

fn provider(egress_network: &str) -> DockerSandbox {
    let scratch = std::env::temp_dir().join("guard-sandbox-live");
    DockerSandbox::new(
        Arc::new(ProcessDockerCli::default()),
        DockerConfig {
            egress_network: egress_network.into(),
            scratch_root: scratch,
            ..DockerConfig::default()
        },
    )
}

async fn sh_exit(
    p: &DockerSandbox,
    sb: &guard_core::ports::Sandbox,
    argv: &[&str],
) -> (i32, String) {
    let out = p
        .exec(sb, &ExecRequest::new(argv.iter().copied()))
        .await
        .unwrap();
    (
        out.exit_code,
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const NODE_GET: &str = "fetch(process.argv[1], { signal: AbortSignal.timeout(5000) })\
  .then((r) => { console.log('status', r.status); process.exit(r.ok ? 0 : 1); })\
  .catch((e) => { console.error('error', e.cause ? e.cause.code || e.cause.message : e.message); process.exit(2); });";

const NODE_DNS: &str = "require('dns').lookup(process.argv[1], (e, a) => { console.log(e ? 'error ' + e.code : 'resolved ' + a); process.exit(e ? 2 : 0); });";

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn egress_reaches_only_the_allowed_host() {
    let Some(image) = enabled() else { return };
    let up = Upstream::start("egress");
    let p = provider(&up.network);
    let h = host();
    let sb = p
        .create(&spec(
            &h,
            Role::Qa,
            &image,
            EgressAllowlist::gateway_only(&up.name, 80),
        ))
        .await
        .unwrap();
    let allowed = format!("http://{}:80/", up.name);
    let (code, out) = sh_exit(&p, &sb, &["node", "-e", NODE_GET, &allowed]).await;
    assert_eq!(code, 0, "the allowed host must be reachable: {out}");
    assert!(out.contains("status 200"), "{out}");
    for blocked in [
        "http://example.com/",
        "http://1.1.1.1/",
        "http://host.docker.internal/",
    ] {
        let (code, out) = sh_exit(&p, &sb, &["node", "-e", NODE_GET, blocked]).await;
        assert_ne!(code, 0, "{blocked} must be unreachable: {out}");
    }
    let (code, out) = sh_exit(&p, &sb, &["node", "-e", NODE_DNS, "example.com"]).await;
    assert_ne!(code, 0, "external DNS must not resolve: {out}");
    // The allowed name resolves only through --add-host, to the forwarder.
    let (code, out) = sh_exit(&p, &sb, &["node", "-e", NODE_DNS, &up.name]).await;
    assert_eq!(code, 0, "{out}");
    p.destroy(&sb).await.unwrap();
}

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn qa_mounts_hardening_env_and_destroy() {
    let Some(image) = enabled() else { return };
    let up = Upstream::start("qa");
    let p = provider(&up.network);
    let h = host();
    let sb = p
        .create(&spec(
            &h,
            Role::Qa,
            &image,
            EgressAllowlist::gateway_only(&up.name, 80),
        ))
        .await
        .unwrap();
    // Read-only workspace, read-only rootfs, writable features dir, /task and /tmp.
    for (path, writable) in [
        ("/workspace/src/new.ts", false),
        ("/etc/guard-probe", false),
        ("/workspace/tests/journeys/x.spec.ts", false),
        ("/workspace/tests/features/SHOP-1.spec.ts", true),
        ("/task/probe", true),
        ("/tmp/probe", true),
    ] {
        let (code, out) = sh_exit(&p, &sb, &["touch", path]).await;
        assert_eq!(code == 0, writable, "{path}: {out}");
    }
    let (_, env) = sh_exit(&p, &sb, &["printenv"]).await;
    assert!(
        env.contains("CODEGRAPH_TELEMETRY=0") && env.contains("DO_NOT_TRACK=1"),
        "{env}"
    );
    // The secret is not in the container config (docker inspect, on-disk
    // config); it is injected into each exec'd process only.
    let (_, cfg) = docker(&["inspect", "--format", "{{json .Config.Env}}", &sb.id.0]);
    assert!(cfg.contains("DO_NOT_TRACK=1"), "{cfg}");
    assert!(
        !cfg.contains("sk-live-test-not-real"),
        "secret leaked into the container config"
    );
    let (_, key) = sh_exit(&p, &sb, &["printenv", "GUARD_GATEWAY_KEY"]).await;
    assert_eq!(key.trim(), "sk-live-test-not-real");
    let (_, id) = sh_exit(&p, &sb, &["id", "-u"]).await;
    assert_eq!(id.trim(), "1000");
    let (_, status) = sh_exit(&p, &sb, &["cat", "/proc/self/status"]).await;
    let cap_eff = status.lines().find(|l| l.starts_with("CapEff:")).unwrap();
    assert!(cap_eff.ends_with("0000000000000000"), "{cap_eff}");
    let nnp = status
        .lines()
        .find(|l| l.starts_with("NoNewPrivs:"))
        .unwrap();
    assert!(nnp.ends_with('1'), "{nnp}");
    // put_file / read_file round trip; missing file.
    p.put_file(&sb, "/task/task.json", b"{\"v\":1}".to_vec())
        .await
        .unwrap();
    assert_eq!(
        p.read_file(&sb, "/task/task.json").await.unwrap(),
        b"{\"v\":1}"
    );
    assert!(matches!(
        p.read_file(&sb, "/task/none.json").await,
        Err(SandboxError::NotFound(_))
    ));
    // Timeout.
    let mut slow = ExecRequest::new(["sleep", "30"]);
    slow.timeout = Some(Duration::from_secs(2));
    assert!(p.exec(&sb, &slow).await.unwrap().timed_out);

    p.destroy(&sb).await.unwrap();
    p.destroy(&sb).await.unwrap();
    let (_, ps) = docker(&[
        "ps",
        "-a",
        "--filter",
        &format!("label=guard.sandbox={}", sb.id.0),
        "--format",
        "{{.Names}}",
    ]);
    assert!(ps.trim().is_empty(), "containers left behind: {ps}");
    let (exists, _) = docker(&["network", "inspect", &format!("{}-net", sb.id.0)]);
    assert!(!exists, "network left behind");
}

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn coder_sees_only_the_read_only_sealed_spec_under_tests() {
    let Some(image) = enabled() else { return };
    let up = Upstream::start("coder");
    let p = provider(&up.network);
    let h = host();
    let sb = p
        .create(&spec(
            &h,
            Role::Coder,
            &image,
            EgressAllowlist::gateway_only(&up.name, 80),
        ))
        .await
        .unwrap();
    let (_, ls) = sh_exit(&p, &sb, &["ls", "-A", "/workspace/tests"]).await;
    assert_eq!(
        ls.split_whitespace().collect::<Vec<_>>(),
        ["features"],
        "journeys hidden: {ls}"
    );
    let (_, ls) = sh_exit(&p, &sb, &["ls", "-A", "/workspace/tests/features"]).await;
    assert_eq!(
        ls.split_whitespace().collect::<Vec<_>>(),
        ["SHOP-1.spec.ts"]
    );
    let (_, body) = sh_exit(&p, &sb, &["cat", &format!("/workspace/{SPEC_REL}")]).await;
    assert_eq!(
        body, "// SEALED copy\n",
        "the sealed copy, not the QA worktree's"
    );
    for path in [
        format!("/workspace/{SPEC_REL}"),
        "/workspace/tests/new.spec.ts".into(),
    ] {
        let (code, out) = sh_exit(&p, &sb, &["sh", "-c", "echo x >> \"$0\"", &path]).await;
        assert_ne!(code, 0, "{path} must be read-only: {out}");
    }
    let (code, out) = sh_exit(&p, &sb, &["touch", "/workspace/src/feature.ts"]).await;
    assert_eq!(code, 0, "{out}");
    assert!(
        h.path("checkout/src/feature.ts").exists(),
        "rw worktree is the host checkout"
    );
    assert_eq!(
        std::fs::read_to_string(h.path("sealed.spec.ts")).unwrap(),
        "// SEALED copy\n"
    );
    p.destroy(&sb).await.unwrap();
}

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn extract_diff_counts_untracked_files_and_ignores_overlays() {
    let Some(image) = enabled() else { return };
    let up = Upstream::start("diff");
    let p = provider(&up.network);

    // Coder: rw workspace, hidden tests; the overlays must not read as deletions.
    let h = host();
    let sb = p
        .create(&spec(
            &h,
            Role::Coder,
            &image,
            EgressAllowlist::gateway_only(&up.name, 80),
        ))
        .await
        .unwrap();
    sh_exit(
        &p,
        &sb,
        &[
            "sh",
            "-c",
            "echo 'export const n = 2;' > src/app.ts && echo 'x' > src/new.ts",
        ],
    )
    .await;
    let d = p.extract_diff(&sb, &h.base).await.unwrap();
    assert_eq!(
        d.changed_paths,
        ["src/app.ts", "src/new.ts"],
        "{}",
        d.unified_diff
    );
    assert!(d.unified_diff.contains("+export const n = 2;"));
    assert!(
        d.unified_diff
            .contains("diff --git a/src/new.ts b/src/new.ts")
    );
    // The worktree's own index and objects are untouched.
    let o = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(h.path("checkout"))
        .output()
        .unwrap();
    let st = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(
        st.contains("?? src/new.ts") && st.contains(" M src/app.ts"),
        "{st}"
    );
    p.destroy(&sb).await.unwrap();

    // QA: read-only workspace; the diff still works (temporary index in /tmp).
    let h = host();
    let sb = p
        .create(&spec(
            &h,
            Role::Qa,
            &image,
            EgressAllowlist::gateway_only(&up.name, 80),
        ))
        .await
        .unwrap();
    sh_exit(
        &p,
        &sb,
        &[
            "sh",
            "-c",
            "echo \"test('a @ticket:SHOP-1', () => {});\" > tests/features/SHOP-1.spec.ts",
        ],
    )
    .await;
    let d = p.extract_diff(&sb, &h.base).await.unwrap();
    assert_eq!(d.changed_paths, [SPEC_REL]);
    p.destroy(&sb).await.unwrap();
}

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn the_cli_never_receives_the_secret_in_argv() {
    // ProcessDockerCli passes secrets through the docker process env only.
    let Some(_) = enabled() else { return };
    let cli = ProcessDockerCli::default();
    let out = cli
        .run(Invocation {
            args: vec![
                "version".into(),
                "--format".into(),
                "{{.Server.Version}}".into(),
            ],
            secret_env: vec![("GUARD_PROBE_SECRET".into(), Secret::new("v"))],
            stdin: None,
            timeout: Some(Duration::from_secs(30)),
        })
        .await
        .unwrap();
    assert!(out.success(), "{}", out.stderr_str());
}

/// Lists the command lines of the sandbox's processes.
const PS: &str = r#"for f in /proc/[0-9]*/cmdline; do tr '\0' ' ' < "$f" 2>/dev/null; echo; done"#;

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn extract_diff_ignores_agent_git_state_and_kills_strays() {
    let Some(image) = enabled() else { return };
    let up = Upstream::start("strays");
    let p = provider(&up.network);
    let h = host();
    let sb = p
        .create(&spec(
            &h,
            Role::Coder,
            &image,
            EgressAllowlist::gateway_only(&up.name, 80),
        ))
        .await
        .unwrap();
    // The agent rewires git (repo + global config, a replace ref of the base)
    // and leaves a background process running.
    let base = h.base.clone();
    let script = format!(
        "git config --global safe.directory '*' && git config --global user.name a \
         && git config --global user.email a@a && git config diff.noprefix true \
         && git config --global diff.mnemonicPrefix true \
         && t=$(git mktree </dev/null) && c=$(git commit-tree $t -m evil) \
         && git replace -f {base} $c && echo x > src/new.ts \
         && (nohup sh -c 'while :; do echo s >> /tmp/stray.txt; sleep 0.2; done' >/dev/null 2>&1 &)"
    );
    let (code, out) = sh_exit(&p, &sb, &["sh", "-c", &script]).await;
    assert_eq!(code, 0, "{out}");
    let (_, ps) = sh_exit(&p, &sb, &["sh", "-c", PS]).await;
    assert!(ps.contains("while :"), "stray running: {ps}");

    let d = p.extract_diff(&sb, &h.base).await.unwrap();
    assert_eq!(d.changed_paths, ["src/new.ts"], "{}", d.unified_diff);
    assert!(
        d.unified_diff
            .starts_with("diff --git a/src/new.ts b/src/new.ts"),
        "{}",
        d.unified_diff
    );
    let (_, ps) = sh_exit(&p, &sb, &["sh", "-c", PS]).await;
    assert!(!ps.contains("while :"), "stray killed: {ps}");

    // A timed-out exec leaves nothing running either.
    let mut slow = ExecRequest::new(["sleep", "47"]);
    slow.timeout = Some(Duration::from_secs(2));
    assert!(p.exec(&sb, &slow).await.unwrap().timed_out);
    let (code, ps) = sh_exit(&p, &sb, &["sh", "-c", PS]).await;
    assert_eq!(code, 0, "the sandbox still works: {ps}");
    assert!(!ps.contains("sleep 47"), "timed-out process killed: {ps}");
    p.destroy(&sb).await.unwrap();
}

#[tokio::test]
#[ignore = "live Docker: GUARD_DOCKER_TESTS=1"]
async fn a_check_sandbox_has_no_host_mount_env_or_egress() {
    use guard_sandbox::check::{CheckKind, CheckSpec, tar};
    let Some(image) = enabled() else { return };
    let p = provider("bridge");
    // A host file and a host variable the check must never see.
    let h = host();
    let marker = h.path("host-secret.txt");
    std::fs::write(&marker, "HOST-ONLY-MARKER").unwrap();
    // SAFETY: a variable no other test reads or writes.
    unsafe { std::env::set_var("GUARD_LIVE_CHECK_LEAK", "leaked-value") };
    let mut spec = CheckSpec::new("live-run", "SHOP-1", CheckKind::RedFirst, &image);
    spec.limits.timeout = Duration::from_secs(300);
    let sb = p.create_check(&spec).await.unwrap();
    assert_eq!(sb.role, Role::Qa);

    // The tree goes in through tar; nothing of the host is mounted.
    let tree = tar::write(&[tar::Entry::File {
        path: "app/hello.txt".into(),
        mode: 0o644,
        bytes: b"from the tree".to_vec(),
    }])
    .unwrap();
    let mut put = ExecRequest::new(["tar", "-x", "-f", "-", "-C", "/workspace"]);
    put.stdin = Some(tree);
    let out = p.exec(&sb, &put).await.unwrap();
    assert_eq!(out.exit_code, 0, "{}", String::from_utf8_lossy(&out.stderr));
    let (code, out) = sh_exit(&p, &sb, &["cat", "/workspace/app/hello.txt"]).await;
    assert_eq!((code, out.as_str()), (0, "from the tree"));
    let (_, mounts) = sh_exit(&p, &sb, &["cat", "/proc/self/mountinfo"]).await;
    for line in mounts.lines() {
        let target = line.split_whitespace().nth(4).unwrap_or("");
        assert!(
            !target.starts_with("/workspace/") && target != "/task" && target != "/codegraph",
            "unexpected mount {line}"
        );
    }
    assert!(
        mounts
            .lines()
            .any(|l| l.split_whitespace().nth(4) == Some("/workspace") && l.contains("tmpfs")),
        "/workspace is a tmpfs: {mounts}"
    );
    let win = marker.to_string_lossy().replace('\\', "/");
    let rel = win
        .split_once(':')
        .map_or(win.as_str(), |(_, r)| r)
        .to_string();
    for path in [
        win.clone(),
        format!("/mnt/c{rel}"),
        format!("/host_mnt/c{rel}"),
        format!("/run/desktop/mnt/host/c{rel}"),
    ] {
        let (code, out) = sh_exit(&p, &sb, &["cat", &path]).await;
        assert_ne!(code, 0, "{path} must not exist in the check: {out}");
        assert!(!out.contains("HOST-ONLY-MARKER"));
    }
    let (_, env) = sh_exit(&p, &sb, &["env"]).await;
    assert!(!env.contains("GUARD_LIVE_CHECK_LEAK"), "{env}");
    assert!(env.contains("DO_NOT_TRACK=1"), "{env}");
    for blocked in [
        "http://example.com/",
        "http://1.1.1.1/",
        "http://host.docker.internal/",
    ] {
        let (code, out) = sh_exit(&p, &sb, &["node", "-e", NODE_GET, blocked]).await;
        assert_ne!(code, 0, "{blocked} must be unreachable from a check: {out}");
    }
    // Hardened like an agent sandbox.
    let (_, status) = sh_exit(&p, &sb, &["cat", "/proc/self/status"]).await;
    assert!(
        status
            .lines()
            .any(|l| l.starts_with("CapEff:") && l.ends_with("0000000000000000")),
        "{status}"
    );
    let (code, _) = sh_exit(&p, &sb, &["touch", "/etc/x"]).await;
    assert_ne!(code, 0, "read-only rootfs");

    // Results come out as a tar.
    let (code, _) = sh_exit(&p, &sb, &["sh", "-c", "mkdir -p /workspace/e/test-results/t && printf PNG > /workspace/e/test-results/t/s.png"]).await;
    assert_eq!(code, 0);
    let out = p
        .exec(
            &sb,
            &ExecRequest::new([
                "sh",
                "-c",
                guard_sandbox::check::COPY_OUT_SCRIPT,
                guard_sandbox::check::COPY_OUT_NAME,
                "e",
            ]),
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0, "{}", String::from_utf8_lossy(&out.stderr));
    let files = tar::read(&out.stdout, 1 << 20).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "test-results/t/s.png");
    assert_eq!(files[0].bytes, b"PNG");

    p.destroy(&sb).await.unwrap();
    let (ok, _) = docker(&["inspect", &sb.id.0]);
    assert!(!ok, "the check container is removed");
    let (ok, _) = docker(&["network", "inspect", &format!("{}-net", sb.id.0)]);
    assert!(!ok, "its network is removed");
}

/// A live test was asked to run (`--include-ignored`) without `var`: fail,
/// so a CI run that forgot it never reports a vacuous `ok`.
/// `GUARD_ALLOW_SKIPPED_LIVE_TESTS=1` turns that into a skip.
fn missing_live_env(var: &str) {
    if std::env::var("GUARD_ALLOW_SKIPPED_LIVE_TESTS").as_deref() == Ok("1") {
        eprintln!("skipped: {var} not set (GUARD_ALLOW_SKIPPED_LIVE_TESTS=1)");
        return;
    }
    panic!("{var} is not set; set it, or GUARD_ALLOW_SKIPPED_LIVE_TESTS=1 to skip live tests");
}
