//! Unit tests of the Docker adapter against a recording fake `docker` CLI.

use super::cli::{CliOutput, DockerCli, Invocation};
use super::*;
use guard_core::ports::{
    EgressAllowlist, EgressPurpose, EgressRule, ExecRequest, Role, SandboxError, SandboxLayout,
    SandboxProvider, SandboxSpec, Secret,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const SECRET: &str = "sk-very-secret-virtual-key";

type Responder = Box<dyn Fn(&Invocation) -> CliOutput + Send + Sync>;

struct FakeCli {
    log: Mutex<Vec<Invocation>>,
    respond: Responder,
}

impl FakeCli {
    fn new() -> Arc<Self> {
        Self::with(|_| None)
    }

    /// `custom` answers first; `None` falls back to the default answers.
    fn with(
        custom: impl Fn(&Invocation) -> Option<CliOutput> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(vec![]),
            respond: Box::new(move |inv| custom(inv).unwrap_or_else(|| default_answer(inv))),
        })
    }

    fn calls(&self) -> Vec<Invocation> {
        self.log.lock().unwrap().clone()
    }

    fn lines(&self) -> Vec<String> {
        self.calls().iter().map(Invocation::line).collect()
    }
}

fn default_answer(inv: &Invocation) -> CliOutput {
    let a = &inv.args;
    match a.first().map(String::as_str) {
        Some("info") => CliOutput::ok(r#"{"runc":{"path":"runc"}}"#),
        Some("inspect") => CliOutput::ok("172.30.0.2\n"),
        Some("logs") => CliOutput::ok("guard-egress ready 0.0.0.0:4000 -> gw.internal:4000\n"),
        Some("run") => CliOutput::ok("c0ffee\n"),
        _ => CliOutput::ok(""),
    }
}

#[async_trait::async_trait]
impl DockerCli for FakeCli {
    async fn run(&self, invocation: Invocation) -> Result<CliOutput, SandboxError> {
        let out = (self.respond)(&invocation);
        self.log.lock().unwrap().push(invocation);
        Ok(out)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    checkout: String,
    task: String,
    sealed: String,
    scratch: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let p = |s: &str| dir.path().join(s).to_string_lossy().into_owned();
    std::fs::create_dir_all(dir.path().join("checkout/tests/features")).unwrap();
    std::fs::create_dir_all(dir.path().join("task")).unwrap();
    std::fs::write(dir.path().join("sealed.spec.ts"), b"test()").unwrap();
    Fixture {
        checkout: p("checkout"),
        task: p("task"),
        sealed: p("sealed.spec.ts"),
        scratch: dir.path().join("scratch"),
        _dir: dir,
    }
}

fn layout(f: &Fixture, role: Role) -> SandboxLayout {
    SandboxLayout {
        run_id: "run-1".into(),
        ticket: "SHOP-1".into(),
        attempt: 1,
        image: format!("guard/sandbox@sha256:{DIGEST}"),
        checkout: f.checkout.clone(),
        task_dir: f.task.clone(),
        codegraph_index: None,
        sealed_spec: match role {
            Role::Coder => Some((f.sealed.clone(), "tests/features/SHOP-1.spec.ts".into())),
            Role::Qa => None,
        },
        tests_dir: "tests".into(),
        skills_dir: None,
    }
}

fn spec(f: &Fixture, role: Role) -> SandboxSpec {
    let mut s = SandboxSpec::for_role(
        role,
        layout(f, role),
        EgressAllowlist::gateway_only("gw.internal", 4000),
    );
    s.secrets
        .insert("GUARD_GATEWAY_KEY".into(), Secret::new(SECRET));
    s
}

fn config(f: &Fixture) -> DockerConfig {
    DockerConfig {
        scratch_root: f.scratch.clone(),
        forwarder_ready_timeout: Duration::from_millis(200),
        ..DockerConfig::default()
    }
}

fn sandbox_run(cli: &FakeCli) -> Invocation {
    cli.calls()
        .into_iter()
        .find(|i| {
            i.args.first().map(String::as_str) == Some("run")
                && has_pair(&i.args, "--entrypoint", "sleep")
        })
        .expect("sandbox docker run")
}

fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == value)
}

#[tokio::test]
async fn create_rejects_an_invalid_spec_before_calling_docker() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let mut s = spec(&f, Role::Coder);
    s.image = "guard/sandbox:latest".into();
    let err = docker.create(&s).await.unwrap_err();
    assert!(matches!(err, SandboxError::InvalidSpec(_)), "{err:?}");
    assert!(cli.calls().is_empty());
}

#[tokio::test]
async fn coder_container_is_hardened_and_mounted_per_role() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    assert!(sb.id.0.starts_with("guard-sbx-"), "{}", sb.id.0);
    let run = sandbox_run(&cli);
    let a = &run.args;
    for flag in ["--read-only", "--init", "-d"] {
        assert!(a.iter().any(|x| x == flag), "missing {flag}: {a:?}");
    }
    assert!(has_pair(a, "--cap-drop", "ALL"));
    assert!(has_pair(a, "--security-opt", "no-new-privileges"));
    assert!(has_pair(a, "--pids-limit", "512"));
    assert!(has_pair(a, "--cpus", "2"));
    assert!(has_pair(a, "--memory", "4096m"));
    assert!(has_pair(a, "--memory-swap", "4096m"));
    assert!(has_pair(a, "--user", "1000:1000"));
    assert!(has_pair(a, "--network", &format!("{}-net", sb.id.0)));
    assert!(has_pair(a, "--add-host", "gw.internal:172.30.0.2"));
    assert!(
        has_pair(a, "--dns", "127.0.0.1"),
        "external DNS must not resolve"
    );
    assert!(has_pair(a, "-e", "CODEGRAPH_TELEMETRY=0"));
    assert!(has_pair(a, "-e", "DO_NOT_TRACK=1"));
    assert!(
        a.iter()
            .any(|x| x.starts_with("/tmp:") && x.contains("size="))
    );
    assert!(
        !a.iter().any(|x| x == "--runtime=runsc"),
        "runsc not installed"
    );

    let mounts: Vec<&String> = a
        .windows(2)
        .filter(|w| w[0] == "--mount")
        .map(|w| &w[1])
        .collect();
    let workspace = mounts
        .iter()
        .find(|m| m.contains("target=/workspace,") || m.ends_with("target=/workspace"))
        .unwrap();
    assert!(
        !workspace.contains("readonly"),
        "coder workspace is rw: {workspace}"
    );
    let tests = mounts
        .iter()
        .find(|m| {
            m.contains("target=/workspace/tests,")
                || m.ends_with("target=/workspace/tests,readonly")
        })
        .unwrap();
    assert!(tests.ends_with(",readonly"), "{tests}");
    assert!(
        !tests.contains(&f.checkout),
        "hidden tests must not bind the checkout: {tests}"
    );
    let sealed = mounts
        .iter()
        .find(|m| m.contains("SHOP-1.spec.ts"))
        .unwrap();
    assert!(
        sealed.contains(&f.sealed) && sealed.ends_with(",readonly"),
        "{sealed}"
    );
    // The hidden skeleton holds only the mount point of the sealed spec.
    let skeleton = tests
        .split(',')
        .find_map(|kv| kv.strip_prefix("source="))
        .unwrap();
    let placeholder = std::path::Path::new(skeleton).join("features/SHOP-1.spec.ts");
    assert!(placeholder.is_file(), "{placeholder:?}");
    assert_eq!(std::fs::read(&placeholder).unwrap(), b"");

    for inv in cli.calls() {
        assert!(
            !inv.line().contains(SECRET),
            "secret leaked into argv: {}",
            inv.line()
        );
    }
}

#[tokio::test]
async fn qa_container_mounts_workspace_read_only_and_features_rw() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    docker.create(&spec(&f, Role::Qa)).await.unwrap();
    let run = sandbox_run(&cli);
    let mounts: Vec<&String> = run
        .args
        .windows(2)
        .filter(|w| w[0] == "--mount")
        .map(|w| &w[1])
        .collect();
    let ws = mounts
        .iter()
        .find(|m| m.contains("target=/workspace,"))
        .unwrap();
    assert!(ws.ends_with(",readonly"), "{ws}");
    let feat = mounts
        .iter()
        .find(|m| m.contains("target=/workspace/tests/features"))
        .unwrap();
    assert!(!feat.contains("readonly"), "{feat}");
    let task = mounts.iter().find(|m| m.contains("target=/task")).unwrap();
    assert!(!task.contains("readonly"));
}

#[tokio::test]
async fn network_is_internal_and_each_rule_gets_one_forwarder() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let mut s = spec(&f, Role::Qa);
    s.egress = s.egress.with(EgressRule {
        host: "preview.internal".into(),
        port: 8080,
        purpose: EgressPurpose::Preview,
    });
    let sb = docker.create(&s).await.unwrap();
    let lines = cli.lines();
    let net = format!("{}-net", sb.id.0);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("network create --internal") && l.ends_with(&net)),
        "{lines:#?}"
    );
    let calls = cli.calls();
    let fwd: Vec<&Invocation> = calls
        .iter()
        .filter(|i| {
            i.args.first().map(String::as_str) == Some("run")
                && i.line().contains("GUARD_EGRESS_TARGET_HOST")
        })
        .collect();
    assert_eq!(fwd.len(), 2);
    for (inv, (host, port)) in fwd
        .iter()
        .zip([("gw.internal", 4000), ("preview.internal", 8080)])
    {
        let a = &inv.args;
        assert!(has_pair(
            a,
            "-e",
            &format!("GUARD_EGRESS_TARGET_HOST={host}")
        ));
        assert!(has_pair(
            a,
            "-e",
            &format!("GUARD_EGRESS_TARGET_PORT={port}")
        ));
        assert!(
            has_pair(a, "--network", "bridge"),
            "forwarder sits on the egress network"
        );
        assert!(has_pair(a, "--cap-drop", "ALL"));
        assert!(a.iter().any(|x| x == "--read-only"));
    }
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.starts_with("network connect") && l.contains(&net))
            .count(),
        2
    );
    let run = sandbox_run(&cli);
    assert!(has_pair(
        &run.args,
        "--add-host",
        "preview.internal:172.30.0.2"
    ));
}

#[tokio::test]
async fn runsc_is_used_when_the_daemon_has_it() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        (inv.args.first().map(String::as_str) == Some("info"))
            .then(|| CliOutput::ok(r#"{"runc":{},"runsc":{"path":"/usr/bin/runsc"}}"#))
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    docker.create(&spec(&f, Role::Qa)).await.unwrap();
    assert!(
        sandbox_run(&cli)
            .args
            .iter()
            .any(|a| a == "--runtime=runsc")
    );
}

#[tokio::test]
async fn a_failed_container_start_cleans_up_and_reports_unavailable() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        (inv.args.first().map(String::as_str) == Some("run")
            && has_pair(&inv.args, "--entrypoint", "sleep"))
        .then(|| CliOutput::failed(125, "Unable to find image"))
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let err = docker.create(&spec(&f, Role::Qa)).await.unwrap_err();
    assert!(
        matches!(err, SandboxError::Unavailable(ref m) if m.contains("Unable to find image")),
        "{err:?}"
    );
    let lines = cli.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("rm -f") && l.contains("-egress-0")),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("network rm")),
        "{lines:#?}"
    );
}

#[tokio::test]
async fn exec_forwards_secrets_by_name_and_returns_non_zero_exits() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        (inv.args.first().map(String::as_str) == Some("exec")).then(|| CliOutput {
            status: Some(3),
            stdout: b"out".to_vec(),
            stderr: b"err".to_vec(),
            timed_out: false,
        })
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    let mut req = ExecRequest::new(["claude", "-p", "do it"]);
    req.env.insert("FOO".into(), "bar".into());
    let out = docker.exec(&sb, &req).await.unwrap();
    assert_eq!(
        (out.exit_code, out.stdout.as_slice(), out.timed_out),
        (3, &b"out"[..], false)
    );
    let exec = cli
        .calls()
        .into_iter()
        .find(|i| i.args[0] == "exec")
        .unwrap();
    let a = &exec.args;
    assert!(has_pair(a, "-w", "/workspace"));
    assert!(has_pair(a, "-e", "FOO=bar"));
    assert!(
        has_pair(a, "-e", "GUARD_GATEWAY_KEY"),
        "by name only: {a:?}"
    );
    assert_eq!(&a[a.len() - 3..], ["claude", "-p", "do it"]);
    assert_eq!(exec.secret_env.len(), 1);
    assert_eq!(exec.secret_env[0].0, "GUARD_GATEWAY_KEY");
    assert_eq!(exec.secret_env[0].1.expose(), SECRET);
    assert!(!exec.line().contains(SECRET));
    assert!(exec.timeout.is_some(), "bounded by the sandbox wall clock");
}

#[tokio::test]
async fn exec_rejects_empty_argv_and_bad_env_names() {
    let f = fixture();
    let docker = DockerSandbox::new(FakeCli::new(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    assert!(matches!(
        docker.exec(&sb, &ExecRequest::default()).await,
        Err(SandboxError::Exec(_))
    ));
    let mut req = ExecRequest::new(["true"]);
    req.env.insert("bad name".into(), "x".into());
    assert!(matches!(
        docker.exec(&sb, &req).await,
        Err(SandboxError::Exec(_))
    ));
}

#[tokio::test]
async fn exec_timeout_is_reported() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        (inv.args[0] == "exec").then(|| CliOutput {
            status: None,
            timed_out: true,
            ..CliOutput::default()
        })
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    let mut req = ExecRequest::new(["sleep", "100"]);
    req.timeout = Some(Duration::from_secs(1));
    let out = docker.exec(&sb, &req).await.unwrap();
    assert!(out.timed_out);
    let exec = cli
        .calls()
        .into_iter()
        .find(|i| i.args[0] == "exec")
        .unwrap();
    assert_eq!(exec.timeout, Some(Duration::from_secs(1)));
}

#[tokio::test]
async fn put_file_only_writes_inside_writable_mounts() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Qa)).await.unwrap();
    docker
        .put_file(&sb, "/task/task.json", b"{}".to_vec())
        .await
        .unwrap();
    let put = cli
        .calls()
        .into_iter()
        .find(|i| i.args[0] == "exec" && i.line().contains("of=/task/task.json"))
        .unwrap();
    assert_eq!(put.stdin.as_deref(), Some(&b"{}"[..]));
    assert!(put.args.iter().any(|a| a == "-i"));
    for bad in [
        "/workspace/src/app.ts",
        "/task/../etc/passwd",
        "relative.json",
        "/etc/passwd",
    ] {
        let err = docker.put_file(&sb, bad, b"x".to_vec()).await.unwrap_err();
        assert!(matches!(err, SandboxError::Io(_)), "{bad}: {err:?}");
    }
}

#[tokio::test]
async fn read_file_maps_a_missing_file_to_not_found() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        (inv.args[0] == "exec" && inv.args.iter().any(|a| a == "cat")).then(|| {
            if inv.args.last().unwrap() == "/task/result.json" {
                CliOutput::ok("{\"ok\":1}")
            } else {
                CliOutput::failed(1, "cat: /task/nope: No such file or directory")
            }
        })
    });
    let docker = DockerSandbox::new(cli, config(&f));
    let sb = docker.create(&spec(&f, Role::Qa)).await.unwrap();
    assert_eq!(
        docker.read_file(&sb, "/task/result.json").await.unwrap(),
        b"{\"ok\":1}"
    );
    assert!(matches!(
        docker.read_file(&sb, "/task/nope").await,
        Err(SandboxError::NotFound(_))
    ));
}

#[tokio::test]
async fn extract_diff_uses_a_temporary_index_and_excludes_overlay_mounts() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        if inv.args[0] != "exec" {
            return None;
        }
        let l = inv.line();
        if l.contains("--name-only") {
            Some(CliOutput::ok("src/b.ts\0src/a.ts\0src/a.ts\0"))
        } else if l.contains(" diff ") {
            Some(CliOutput::ok("diff --git a/src/a.ts b/src/a.ts\n"))
        } else {
            Some(CliOutput::ok(""))
        }
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    let base = "0123456789abcdef0123456789abcdef01234567";
    let d = docker.extract_diff(&sb, base).await.unwrap();
    assert_eq!(d.base_commit, base);
    assert_eq!(d.changed_paths, ["src/a.ts", "src/b.ts"]);
    assert!(d.unified_diff.starts_with("diff --git"));
    let git: Vec<Invocation> = cli
        .calls()
        .into_iter()
        .filter(|i| i.args[0] == "exec" && i.line().contains(" git "))
        .collect();
    assert!(
        git.iter()
            .any(|i| i.line().contains(&format!("read-tree {base}")))
    );
    let add = git.iter().find(|i| i.line().contains(" add -A")).unwrap();
    assert!(
        add.args.iter().any(|a| a == ":(exclude,literal)tests"),
        "{:?}",
        add.args
    );
    assert!(
        add.args
            .iter()
            .any(|a| a == ":(exclude,literal)tests/features/SHOP-1.spec.ts")
    );
    for i in &git {
        assert!(
            has_pair(&i.args, "-e", "GIT_INDEX_FILE=/tmp/.guard-diff/index"),
            "{}",
            i.line()
        );
        assert!(i.args.iter().any(|a| a == "safe.directory=*"));
    }
    let diff = git
        .iter()
        .find(|i| i.line().contains(" diff --cached") && !i.line().contains("--name-only"))
        .unwrap();
    for flag in ["--no-renames", "--binary", "--no-ext-diff", "--no-textconv"] {
        assert!(diff.args.iter().any(|a| a == flag), "{flag}");
    }
    for bad in ["HEAD", "--output=/tmp/x", "abc"] {
        assert!(
            matches!(
                docker.extract_diff(&sb, bad).await,
                Err(SandboxError::Exec(_))
            ),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn destroy_removes_everything_and_is_idempotent() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    docker.destroy(&sb).await.unwrap();
    docker.destroy(&sb).await.unwrap();
    let lines = cli.lines();
    assert!(
        lines.iter().any(|l| l == &format!("rm -f -v {}", sb.id.0)),
        "{lines:#?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == &format!("rm -f -v {}-egress-0", sb.id.0))
    );
    assert!(
        lines
            .iter()
            .any(|l| l == &format!("network rm {}-net", sb.id.0))
    );
    assert!(
        !f.scratch.join(&sb.id.0).exists(),
        "scratch skeleton removed"
    );
    assert!(matches!(
        docker.read_file(&sb, "/task/x").await,
        Err(SandboxError::NotFound(_))
    ));
    assert!(matches!(
        docker.exec(&sb, &ExecRequest::new(["true"])).await,
        Err(SandboxError::NotFound(_))
    ));
    let mut unknown = sb.clone();
    unknown.id.0 = "guard-sbx-unknown".into();
    assert!(matches!(
        docker.destroy(&unknown).await,
        Err(SandboxError::NotFound(_))
    ));
}

#[test]
fn expired_lists_containers_past_their_deadline() {
    let ps = "guard-sbx-a\t100\tguard-sbx-a\nguard-sbx-a-egress-0\t100\tguard-sbx-a\nguard-sbx-b\t300\tguard-sbx-b\nnoise\t\t\n";
    let got = expired_containers(ps, 200);
    assert_eq!(
        got,
        vec![
            ("guard-sbx-a".to_string(), "guard-sbx-a".to_string()),
            (
                "guard-sbx-a-egress-0".to_string(),
                "guard-sbx-a".to_string()
            ),
        ]
    );
}

#[test]
fn mount_sources_with_csv_metacharacters_are_rejected() {
    assert!(mount_arg("/a,b", "/workspace", true).is_err());
    assert!(mount_arg("/a\"b", "/workspace", true).is_err());
    assert_eq!(
        mount_arg("/host/x", "/workspace", true).unwrap(),
        "type=bind,source=/host/x,target=/workspace,readonly"
    );
    assert_eq!(
        mount_arg("/host/x", "/task", false).unwrap(),
        "type=bind,source=/host/x,target=/task"
    );
}

// ------------------------------------------------------------ review fixes

fn execs_after_create(cli: &FakeCli) -> Vec<Invocation> {
    cli.calls()
        .into_iter()
        .skip_while(|i| !(i.args[0] == "run" && i.args.iter().any(|a| a == "sleep")))
        .filter(|i| i.args[0] == "exec")
        .collect()
}

#[tokio::test]
async fn extract_diff_kills_strays_first_and_ignores_agent_git_config_and_replace_refs() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    let base = "0123456789abcdef0123456789abcdef01234567";
    docker.extract_diff(&sb, base).await.unwrap();
    let execs = execs_after_create(&cli);
    let kill = execs
        .iter()
        .position(|i| i.args.iter().any(|a| a == KILL_STRAYS_NAME))
        .expect("strays killed");
    let read_tree = execs
        .iter()
        .position(|i| i.line().contains("read-tree"))
        .unwrap();
    assert!(kill < read_tree, "strays are killed before the diff");
    let git: Vec<&Invocation> = execs
        .iter()
        .filter(|i| i.line().contains(" git "))
        .collect();
    assert!(!git.is_empty());
    for i in git {
        for env in ["GIT_CONFIG_GLOBAL=/dev/null", "GIT_NO_REPLACE_OBJECTS=1"] {
            assert!(has_pair(&i.args, "-e", env), "{env}: {}", i.line());
        }
        for c in [
            "diff.noprefix=false",
            "diff.mnemonicPrefix=false",
            "diff.relative=false",
            "core.fsmonitor=false",
        ] {
            assert!(has_pair(&i.args, "-c", c), "{c}: {}", i.line());
        }
    }
}

#[tokio::test]
async fn a_timed_out_exec_kills_what_it_left_running() {
    let f = fixture();
    let cli = FakeCli::with(|inv| {
        (inv.args[0] == "exec" && inv.args.iter().any(|a| a == "100")).then(|| CliOutput {
            status: None,
            timed_out: true,
            ..CliOutput::default()
        })
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    let mut req = ExecRequest::new(["sleep", "100"]);
    req.timeout = Some(Duration::from_secs(1));
    assert!(docker.exec(&sb, &req).await.unwrap().timed_out);
    let execs = execs_after_create(&cli);
    assert_eq!(execs.len(), 2, "{execs:?}");
    assert!(execs[1].args.iter().any(|a| a == KILL_STRAYS_NAME));
}

#[tokio::test]
async fn every_docker_management_call_is_bounded_by_a_timeout() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    docker.destroy(&sb).await.unwrap();
    docker.reap_expired().await.unwrap();
    for i in cli.calls() {
        assert!(i.timeout.is_some(), "unbounded: {}", i.line());
    }
}

#[tokio::test]
async fn the_sandbox_idles_only_until_its_deadline() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let s = spec(&f, Role::Coder);
    let secs = s.limits.timeout.as_secs() + IDLE_GRACE_SECS;
    docker.create(&s).await.unwrap();
    let run = sandbox_run(&cli);
    assert!(!run.args.iter().any(|a| a == "infinity"));
    assert!(
        run.args.ends_with(&[s.image.clone(), secs.to_string()]),
        "{:?}",
        run.args
    );
}

#[tokio::test]
async fn a_failed_removal_is_reported_retried_and_then_forgotten() {
    let f = fixture();
    let failures = Arc::new(std::sync::atomic::AtomicU32::new(1));
    let left = failures.clone();
    let cli = FakeCli::with(move |inv| {
        let is_rm = inv.args[0] == "rm" && !inv.args.iter().any(|a| a.contains("egress"));
        if is_rm && left.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            left.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return Some(CliOutput::failed(1, "Error response from daemon: busy"));
        }
        // An already-removed object is not a failure.
        (inv.args[0] == "network" && inv.args[1] == "rm")
            .then(|| CliOutput::failed(1, "Error: No such network: x"))
    });
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create(&spec(&f, Role::Coder)).await.unwrap();
    assert_eq!(docker.live_count(), 1);
    let err = docker.destroy(&sb).await.unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");
    // Destroyed for every other operation, but kept for the retry.
    assert!(matches!(
        docker.exec(&sb, &ExecRequest::new(["true"])).await,
        Err(SandboxError::NotFound(_))
    ));
    assert_eq!(docker.live_count(), 1);
    let before = cli.calls().len();
    docker.destroy(&sb).await.unwrap();
    assert!(
        cli.calls()[before..].iter().any(|i| i.args[0] == "rm"),
        "retried"
    );
    assert_eq!(docker.live_count(), 0, "dropped from the live map");
    // Still idempotent afterwards.
    docker.destroy(&sb).await.unwrap();
}

fn check_spec() -> crate::check::CheckSpec {
    let mut s = crate::check::CheckSpec::new(
        "run-1",
        "SHOP-1",
        crate::check::CheckKind::Gate,
        &format!("guard/sandbox@sha256:{DIGEST}"),
    );
    s.secrets
        .insert("GUARD_TEST_TOKEN".into(), Secret::new(SECRET));
    s
}

#[tokio::test]
async fn a_check_container_has_no_mounts_no_egress_and_a_tmpfs_workspace() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let sb = docker.create_check(&check_spec()).await.unwrap();
    assert!(sb.id.0.starts_with("guard-sbx-"), "reaped like any sandbox");
    assert_eq!(sb.role, Role::Coder, "the gate checks the Coder's output");
    let lines = cli.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("network create --internal")
                && l.ends_with(&format!("{}-net", sb.id.0))),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("-egress-")),
        "a check has no egress forwarder: {lines:#?}"
    );
    let run = sandbox_run(&cli);
    let a = &run.args;
    for flag in ["--read-only", "--init", "-d"] {
        assert!(a.iter().any(|x| x == flag), "missing {flag}: {a:?}");
    }
    assert!(has_pair(a, "--cap-drop", "ALL"));
    assert!(has_pair(a, "--security-opt", "no-new-privileges"));
    assert!(has_pair(a, "--user", "1000:1000"));
    assert!(has_pair(a, "--dns", "127.0.0.1"));
    assert!(has_pair(a, "--network", &format!("{}-net", sb.id.0)));
    assert!(has_pair(a, "--label", "guard.check=gate"));
    assert!(has_pair(a, "--label", "guard.role=coder"));
    assert!(has_pair(a, "--workdir", "/workspace"));
    assert!(
        a.windows(2).any(|w| w[0] == "--tmpfs"
            && w[1].starts_with("/workspace:")
            && w[1].contains("size=1024m")),
        "{a:?}"
    );
    assert!(
        !a.iter()
            .any(|x| x == "--mount" || x == "-v" || x == "--volume"),
        "{a:?}"
    );
    assert!(!a.iter().any(|x| x == "--add-host"), "{a:?}");
    for inv in cli.calls() {
        assert!(
            !inv.line().contains(SECRET),
            "secret in argv: {}",
            inv.line()
        );
    }

    // Execs forward the secrets by name; put_file has no writable mount.
    docker.exec(&sb, &ExecRequest::new(["true"])).await.unwrap();
    let exec = cli.calls().into_iter().last().unwrap();
    assert!(
        has_pair(&exec.args, "-e", "GUARD_TEST_TOKEN"),
        "{:?}",
        exec.args
    );
    assert_eq!(exec.secret_env[0].1.expose(), SECRET);
    assert!(
        docker
            .put_file(&sb, "/workspace/x", b"x".to_vec())
            .await
            .is_err()
    );

    docker.destroy(&sb).await.unwrap();
    assert_eq!(docker.live_count(), 0);
    let lines = cli.lines();
    assert!(lines.iter().any(|l| l == &format!("rm -f -v {}", sb.id.0)));
    assert!(
        lines
            .iter()
            .any(|l| l == &format!("network rm {}-net", sb.id.0))
    );
}

#[tokio::test]
async fn an_invalid_check_spec_never_reaches_docker() {
    let f = fixture();
    let cli = FakeCli::new();
    let docker = DockerSandbox::new(cli.clone(), config(&f));
    let mut s = check_spec();
    s.image = "guard/sandbox:latest".into();
    let err = docker.create_check(&s).await.unwrap_err();
    assert!(matches!(err, SandboxError::InvalidSpec(_)), "{err:?}");
    assert!(cli.calls().is_empty());
}
