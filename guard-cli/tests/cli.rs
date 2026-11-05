//! End-to-end tests of the `guard` binary against fixture repositories.

use assert_cmd::Command;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A credentials file that never exists: the developer's own
/// (`guard connect`) must not leak into tests.
const NO_CREDENTIALS: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/no-credentials.toml");

fn guard(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("guard").unwrap();
    cmd.current_dir(dir)
        .env("NO_COLOR", "1")
        .env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS);
    // Nothing from the developer's or the CI machine's environment leaks in.
    for var in LEAKY_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// Variables that change guard's behavior and must come only from a test.
const LEAKY_ENV: &[&str] = &[
    "RUN_ID",
    "GUARD_NPX",
    "GUARD_SERVER_TOKEN",
    "GUARD_STORAGE_URL",
    "GUARD_PR_NUMBER",
    "GUARD_BASE_SHA",
    "GUARD_THRESHOLDS",
    "GUARD_JEV_FAKE",
    "GUARD_JIRA_FAKE",
    "GUARD_LISTEN",
    "GUARD_DASHBOARD_TOKEN",
    "GUARD_JIRA_WEBHOOK_SECRET",
    "TYPESAFE_API_KEY",
    "JEV_URL",
    "DATABASE_URL",
    "JIRA_BASE_URL",
    "JIRA_EMAIL",
    "JIRA_API_TOKEN",
    "GITHUB_TOKEN",
    "GITHUB_REPOSITORY",
    "GITHUB_EVENT_PATH",
    "GITHUB_HEAD_REF",
    "GITHUB_SERVER_URL",
    "GITHUB_RUN_ID",
    "GITLAB_TOKEN",
    "GITLAB_PROJECT",
    "CI_PROJECT_ID",
    "CI_PROJECT_PATH",
    "CI_PIPELINE_URL",
    "CI_MERGE_REQUEST_IID",
    "CI_MERGE_REQUEST_DIFF_BASE_SHA",
    "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME",
    "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
    "GUARD_BASE",
    "GITHUB_BASE_REF",
];

/// Fixture git: non-interactive and without the fsmonitor daemon the Git for
/// Windows system config enables (one long-lived background git per temp repo).
fn git(dir: &Path, args: &[&str]) {
    let status = StdCommand::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&status.stderr)
    );
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn copy_fixture(dir: &Path, name: &str, rel: &str) {
    write(dir, rel, &fs::read_to_string(fixture(name)).unwrap());
}

/// A repo with target.toml + catalog committed on `main`.
fn repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    copy_fixture(root, "target.toml", "target.toml");
    copy_fixture(
        root,
        "journeys.json",
        "e2e-playwright/catalog/journeys.json",
    );
    write(root, "web/src/checkout/cart.ts", "export const cart = 1;\n");
    write(root, "services/billing/invoice.go", "package billing\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    dir
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

/// Starts an HTTP server answering 200 to everything; returns its base URL.
fn serve_ok() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// A fake `npx`: dumps its environment to `$FAKE_ENV_OUT`, copies `$FAKE_REPORT`
/// to `$PLAYWRIGHT_JSON_OUTPUT_NAME` when both are set, creates the shard blob
/// when `GUARD_SHARD_INDEX` is set (unless `$FAKE_NO_BLOB` is), and exits with `$FAKE_EXIT`.
/// A baseline run (`PLAYWRIGHT_JSON_OUTPUT_NAME` contains `baseline-`) instead
/// gets `$FAKE_BASE_REPORT`, dumps its environment to `$FAKE_BASE_ENV_OUT` and
/// exits with `$FAKE_BASE_EXIT` (default 1).
fn fake_npx(dir: &Path) -> PathBuf {
    if cfg!(windows) {
        let path = dir.join("fake-npx.cmd");
        let body = [
            "@echo off",
            "set \"GUARD_OUT=%PLAYWRIGHT_JSON_OUTPUT_NAME%\"",
            "if not defined FAKE_BASE_REPORT goto :head",
            "if not defined PLAYWRIGHT_JSON_OUTPUT_NAME goto :head",
            "if \"%GUARD_OUT:baseline-=%\"==\"%GUARD_OUT%\" goto :head",
            "if defined FAKE_BASE_ENV_OUT set > \"%FAKE_BASE_ENV_OUT%\"",
            "if defined FAKE_BASE_ENV_OUT echo ARGS=%* >> \"%FAKE_BASE_ENV_OUT%\"",
            "copy /Y \"%FAKE_BASE_REPORT%\" \"%PLAYWRIGHT_JSON_OUTPUT_NAME%\" >nul",
            "if not defined FAKE_BASE_EXIT exit /b 1",
            "exit /b %FAKE_BASE_EXIT%",
            ":head",
            "if defined FAKE_ENV_OUT set > \"%FAKE_ENV_OUT%\"",
            "if defined FAKE_ENV_OUT echo ARGS=%* >> \"%FAKE_ENV_OUT%\"",
            "if defined FAKE_REPORT if defined PLAYWRIGHT_JSON_OUTPUT_NAME copy /Y \"%FAKE_REPORT%\" \"%PLAYWRIGHT_JSON_OUTPUT_NAME%\" >nul",
            "if defined FAKE_NO_BLOB goto :done",
            "if defined GUARD_SHARD_INDEX if not exist blob-report mkdir blob-report",
            "if defined GUARD_SHARD_INDEX echo zip> blob-report\\report-%GUARD_SHARD_INDEX%.zip",
            ":done",
            "if not defined FAKE_EXIT exit /b 0",
            "exit /b %FAKE_EXIT%",
            "",
        ]
        .join("\r\n");
        fs::write(&path, body).unwrap();
        path
    } else {
        let path = dir.join("fake-npx");
        let body = r#"#!/bin/sh
case "$PLAYWRIGHT_JSON_OUTPUT_NAME" in
  *baseline-*)
    if [ -n "$FAKE_BASE_REPORT" ]; then
      if [ -n "$FAKE_BASE_ENV_OUT" ]; then env > "$FAKE_BASE_ENV_OUT"; echo "ARGS=$*" >> "$FAKE_BASE_ENV_OUT"; fi
      cp "$FAKE_BASE_REPORT" "$PLAYWRIGHT_JSON_OUTPUT_NAME"
      exit "${FAKE_BASE_EXIT:-1}"
    fi;;
esac
if [ -n "$FAKE_ENV_OUT" ]; then env > "$FAKE_ENV_OUT"; echo "ARGS=$*" >> "$FAKE_ENV_OUT"; fi
if [ -n "$FAKE_REPORT" ] && [ -n "$PLAYWRIGHT_JSON_OUTPUT_NAME" ]; then cp "$FAKE_REPORT" "$PLAYWRIGHT_JSON_OUTPUT_NAME"; fi
if [ -n "$GUARD_SHARD_INDEX" ] && [ -z "$FAKE_NO_BLOB" ]; then mkdir -p blob-report; echo zip > "blob-report/report-$GUARD_SHARD_INDEX.zip"; fi
exit "${FAKE_EXIT:-0}"
"#;
        fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }
}

// ---------------------------------------------------------------- general

#[test]
fn version_and_help_exit_zero() {
    let dir = TempDir::new().unwrap();
    guard(dir.path()).arg("--version").assert().success();
    guard(dir.path()).arg("--help").assert().success();
}

#[test]
fn unknown_flags_are_usage_errors() {
    let dir = TempDir::new().unwrap();
    guard(dir.path())
        .args(["doctor", "--nope"])
        .assert()
        .code(2);
}

// ---------------------------------------------------------------- init

#[test]
fn init_detects_the_repository_and_writes_a_valid_target() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    git(
        root,
        &["remote", "add", "origin", "git@github.com:acme/shop.git"],
    );
    write(root, "web/src/app.ts", "x\n");
    write(root, "services/api/main.go", "package main\n");
    write(root, "docs/guide.md", "x\n");
    write(root, "docker-compose.yml", "services: {}\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["branch", "SHOP-12-x"]);

    let output = guard(root).arg("init").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("created target.toml\n"), "{out}");
    assert!(out.contains("created e2e-playwright/ ("), "{out}");
    assert!(
        out.contains("detected languages: go, typescript\n"),
        "{out}"
    );
    assert!(out.contains("detected code host: github\n"), "{out}");
    assert!(
        out.contains("detected start command: docker compose up -d --wait\n"),
        "{out}"
    );
    assert!(
        out.contains("proposed areas: services, web (edit their descriptions)\n"),
        "{out}"
    );
    assert!(out.contains("next steps:\n"), "{out}");

    let target = fs::read_to_string(root.join("target.toml")).unwrap();
    assert!(target.contains("[areas.web]"), "{target}");
    assert!(target.contains(r"ticket_pattern = 'SHOP-\d+'"), "{target}");
    assert!(target.contains("kind = \"github\""), "{target}");
    let catalog = fs::read_to_string(root.join("e2e-playwright/catalog/journeys.json")).unwrap();
    assert!(catalog.contains("\"journeys\": []"));

    // The generated files pass doctor's config checks.
    let doctor = guard(root).arg("doctor").output().unwrap();
    assert!(
        stdout(&doctor).contains("ok    target.toml valid"),
        "{}",
        stdout(&doctor)
    );
}

#[test]
fn init_refuses_to_overwrite_without_force_and_never_touches_the_catalog() {
    let dir = repo();
    let root = dir.path();
    let before = fs::read_to_string(root.join("e2e-playwright/catalog/journeys.json")).unwrap();
    let output = guard(root).arg("init").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("error: "), "{}", stderr(&output));

    guard(root).args(["init", "--force"]).assert().success();
    let after = fs::read_to_string(root.join("e2e-playwright/catalog/journeys.json")).unwrap();
    assert_eq!(before, after);
}

// ---------------------------------------------------------------- doctor

#[test]
fn doctor_reports_every_check_with_a_summary() {
    let dir = repo();
    let output = guard(dir.path())
        .arg("doctor")
        .env_remove("GUARD_TEST_TOKEN")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_ne!(output.status.code(), Some(2), "{out}");
    assert!(out.contains("ok    target.toml valid\n"), "{out}");
    assert!(
        out.contains("ok    catalog valid (3 journeys, 1 quarantined)\n"),
        "{out}"
    );
    assert!(
        out.contains("ok    catalog areas exist in target.toml\n"),
        "{out}"
    );
    assert!(out.contains("warn  GUARD_TEST_TOKEN is not set"), "{out}");
    assert!(
        out.contains("warn  app not ready: GET http://127.0.0.1:9/healthz -> "),
        "{out}"
    );
    assert!(out.lines().last().unwrap().starts_with("doctor: "), "{out}");
    assert!(stderr(&output).contains("hint: run guard up"));
}

#[test]
fn doctor_warns_about_dead_globs() {
    let dir = repo();
    let root = dir.path();
    fs::remove_file(root.join("services/billing/invoice.go")).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "rm"]);
    let out = stdout(&guard(root).arg("doctor").output().unwrap());
    assert!(
        out.contains("warn  dead glob in areas.billing.paths: services/billing/**"),
        "{out}"
    );
}

#[test]
fn doctor_exits_two_on_config_errors_but_runs_every_check() {
    let dir = repo();
    let root = dir.path();
    copy_fixture(root, "target.bad-values.toml", "target.toml");
    let output = guard(root).arg("doctor").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let out = stdout(&output);
    assert!(out.contains("fail  target.toml invalid: version:"), "{out}");
    assert!(
        out.contains("fail  target.toml invalid: app.name:"),
        "{out}"
    );
    assert!(out.contains("ok    catalog valid"), "{out}");
    assert!(out.contains("git "), "{out}");
}

#[test]
fn doctor_reports_unknown_catalog_areas() {
    let dir = repo();
    let root = dir.path();
    copy_fixture(
        root,
        "journeys.unknown-area.json",
        "e2e-playwright/catalog/journeys.json",
    );
    let output = guard(root).arg("doctor").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).contains("fail  journey checkout.customer-card: area nope"));
}

// ---------------------------------------------------------------- affected

fn feature_branch(root: &Path) {
    git(root, &["checkout", "-q", "-b", "SHOP-123-fix-cart"]);
    write(root, "web/src/checkout/cart.ts", "export const cart = 2;\n");
    git(root, &["commit", "-q", "-am", "change cart"]);
    write(root, "README.md", "untracked\n");
}

#[test]
fn affected_json_matches_the_contract() {
    let dir = repo();
    let root = dir.path();
    feature_branch(root);
    let output = guard(root).args(["affected", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["base"], "main");
    assert_eq!(json["base_source"], "local_branch");
    assert_eq!(json["merge_base"].as_str().unwrap().len(), 40);
    assert_eq!(json["branch"], "SHOP-123-fix-cart");
    assert_eq!(json["ticket"], "SHOP-123");
    assert_eq!(
        json["changed_files"],
        serde_json::json!(["web/src/checkout/cart.ts", "README.md"])
    );
    assert_eq!(
        json["tags"],
        serde_json::json!([
            "@journey:checkout.customer-card",
            "@smoke",
            "@ticket:SHOP-123"
        ])
    );
    assert_eq!(
        json["journeys"],
        serde_json::json!([{
            "id": "checkout.customer-card",
            "tag": "@journey:checkout.customer-card",
            "reasons": [
                {"kind": "path", "file": "web/src/checkout/cart.ts", "glob": "web/src/checkout/**"},
                {"kind": "smoke"}
            ]
        }])
    );
    let text = stdout(&output);
    let order: Vec<usize> = [
        "\"base\"",
        "\"base_source\"",
        "\"merge_base\"",
        "\"branch\"",
        "\"ticket\"",
        "\"changed_files\"",
        "\"tags\"",
        "\"journeys\"",
    ]
    .iter()
    .map(|k| text.find(k).unwrap())
    .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
}

#[test]
fn affected_text_lists_tags_with_reasons() {
    let dir = repo();
    let root = dir.path();
    feature_branch(root);
    let output = guard(root).arg("affected").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("base: main (merge-base "), "{out}");
    assert!(
        out.lines().next().unwrap().ends_with("; local_branch)"),
        "{out}"
    );
    assert!(out.contains("branch: SHOP-123-fix-cart\n"), "{out}");
    assert!(out.contains("changed files: 2\n"), "{out}");
    assert!(
        out.contains("@smoke                            smoke\n"),
        "{out}"
    );
    assert!(
        out.contains("@ticket:SHOP-123                  ticket from branch SHOP-123-fix-cart\n"),
        "{out}"
    );
    assert!(
        out.contains("@journey:checkout.customer-card   path web/src/checkout/cart.ts matches web/src/checkout/**\n"),
        "{out}"
    );
}

// ------------------------------------------------------------- submodules

const UMBRELLA_CATALOG: &str = r#"{
  "version": 1,
  "journeys": [
    {"id": "fe.login", "area": "checkout", "title": "Login", "impact_question": "q",
     "paths": ["frontend/src/**"], "smoke": false, "quarantined": false},
    {"id": "be.api", "area": "billing", "title": "Api", "impact_question": "q",
     "paths": ["backend/**"], "smoke": false, "quarantined": false},
    {"id": "web.x", "area": "checkout", "title": "Web", "impact_question": "q",
     "paths": ["web/src/checkout/**"], "smoke": true, "quarantined": false}
  ]
}"#;

/// A repository `init -b main` with `files` and one commit.
fn sub_repo(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    for (rel, body) in files {
        write(dir.path(), rel, body);
    }
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// An umbrella repo whose app code lives in the submodules `frontend/` and
/// `backend/`; target and catalog are committed on `main`.
struct Umbrella {
    dir: TempDir,
    _subs: Vec<TempDir>,
}

impl Umbrella {
    fn root(&self) -> &Path {
        self.dir.path()
    }
}

fn umbrella() -> Umbrella {
    let frontend = sub_repo(&[
        (
            "src/features/auth/components/login.tsx",
            "export const login = 1;
",
        ),
        (
            "package.json",
            "{}
",
        ),
    ]);
    let backend = sub_repo(&[(
        "api/server.go",
        "package api
",
    )]);
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    let target = fs::read_to_string(fixture("target.toml"))
        .unwrap()
        .replace("web/src/checkout/**", "frontend/src/**")
        .replace("services/billing/**", "backend/**");
    write(root, "target.toml", &target);
    write(
        root,
        "e2e-playwright/catalog/journeys.json",
        UMBRELLA_CATALOG,
    );
    write(
        root,
        "web/src/checkout/cart.ts",
        "export const cart = 1;
",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    for (sub, name) in [(&frontend, "frontend"), (&backend, "backend")] {
        git(root, &["submodule", "add", "-q", &slash(sub.path()), name]);
    }
    git(root, &["commit", "-q", "-m", "submodules"]);
    Umbrella {
        dir,
        _subs: vec![frontend, backend],
    }
}

fn affected_json(root: &Path) -> (serde_json::Value, String) {
    let output = guard(root).args(["affected", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    (
        serde_json::from_slice(&output.stdout).unwrap(),
        stderr(&output),
    )
}

fn journey_reason(json: &serde_json::Value, id: &str) -> serde_json::Value {
    json["journeys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == id)
        .unwrap_or_else(|| panic!("{id} not selected: {json}"))["reasons"][0]
        .clone()
}

#[test]
fn affected_umbrella_worktree_change_in_submodule_selects_journey() {
    let u = umbrella();
    let root = u.root();
    write(
        &root.join("frontend"),
        "src/features/auth/components/login.tsx",
        "export const login = 2;
",
    );
    let (json, _) = affected_json(root);
    assert_eq!(
        json["changed_files"],
        serde_json::json!(["frontend/src/features/auth/components/login.tsx"])
    );
    assert_eq!(
        json["tags"],
        serde_json::json!(["@journey:fe.login", "@smoke"])
    );
    assert_eq!(
        journey_reason(&json, "fe.login"),
        serde_json::json!({
            "kind": "path",
            "file": "frontend/src/features/auth/components/login.tsx",
            "glob": "frontend/src/**"
        })
    );
}

#[test]
fn affected_umbrella_committed_bump_selects_journey() {
    let u = umbrella();
    let root = u.root();
    git(root, &["checkout", "-q", "-b", "bump"]);
    let backend = root.join("backend");
    write(
        &backend,
        "api/server.go",
        "package api // v2
",
    );
    git(&backend, &["commit", "-q", "-am", "v2"]);
    git(root, &["commit", "-q", "-am", "bump backend"]);
    let (json, _) = affected_json(root);
    assert_eq!(
        json["changed_files"],
        serde_json::json!(["backend/api/server.go"])
    );
    assert_eq!(
        json["tags"],
        serde_json::json!(["@journey:be.api", "@smoke"])
    );
}

#[test]
fn affected_umbrella_uninitialized_bump_selects_by_subtree_and_warns() {
    let u = umbrella();
    let root = u.root();
    git(root, &["checkout", "-q", "-b", "bump"]);
    let frontend = root.join("frontend");
    write(
        &frontend,
        "package.json",
        "{\"v\": 2}
",
    );
    git(&frontend, &["commit", "-q", "-am", "v2"]);
    git(root, &["commit", "-q", "-am", "bump frontend"]);
    git(root, &["submodule", "deinit", "-q", "-f", "frontend"]);
    let (json, err) = affected_json(root);
    assert!(
        err.contains(
            "warning: submodule frontend is not initialized; treating it as entirely changed"
        ),
        "{err}"
    );
    assert_eq!(json["changed_files"], serde_json::json!(["frontend/"]));
    assert_eq!(
        journey_reason(&json, "fe.login"),
        serde_json::json!({"kind": "path", "file": "frontend/", "glob": "frontend/src/**"})
    );
    assert!(
        !json["tags"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("@journey:be.api")),
        "{json}"
    );
}

#[test]
fn doctor_does_not_report_submodule_globs_as_dead() {
    let u = umbrella();
    let root = u.root();
    let out = stdout(&guard(root).arg("doctor").output().unwrap());
    assert!(!out.contains("dead glob"), "{out}");
    git(root, &["submodule", "deinit", "-q", "-f", "frontend"]);
    let out = stdout(&guard(root).arg("doctor").output().unwrap());
    assert!(!out.contains("dead glob"), "{out}");
}

#[test]
fn init_detection_sees_files_inside_submodules() {
    let app = sub_repo(&[
        (
            "web/src/app.ts",
            "x
",
        ),
        (
            "services/api/main.go",
            "package main
",
        ),
    ]);
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(
        root,
        "README.md",
        "umbrella
",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    git(root, &["submodule", "add", "-q", &slash(app.path()), "app"]);
    git(root, &["commit", "-q", "-m", "app"]);
    let output = guard(root).arg("init").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains(
            "detected languages: go, typescript
"
        ),
        "{out}"
    );
}

#[test]
fn affected_with_an_unknown_base_is_a_usage_error() {
    let dir = repo();
    let output = guard(dir.path())
        .args(["affected", "--base", "does-not-exist"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let err = stderr(&output);
    assert!(
        err.contains(
            "error: unknown git ref: does-not-exist
"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "hint: detected default base: main
"
        ),
        "{err}"
    );
}

/// `repo()` moved to the branch `feature`, with nothing a base could be.
fn repo_without_a_base() -> TempDir {
    let dir = repo();
    git(dir.path(), &["branch", "-q", "-m", "feature"]);
    git(dir.path(), &["config", "init.defaultBranch", "main"]);
    dir
}

#[test]
fn affected_without_local_main_uses_origin_head() {
    let origin = repo();
    git(origin.path(), &["branch", "-q", "-m", "claude/x"]);
    let parent = TempDir::new().unwrap();
    git(parent.path(), &["clone", "-q", &slash(origin.path()), "c"]);
    let root = parent.path().join("c");
    git(&root, &["checkout", "-q", "-b", "SHOP-1-fix"]);
    git(&root, &["branch", "-q", "-D", "claude/x"]);
    write(
        &root,
        "web/src/checkout/cart.ts",
        "export const cart = 3;
",
    );
    let (json, _) = affected_json(&root);
    assert_eq!(json["base"], "origin/claude/x");
    assert_eq!(json["base_source"], "remote_head");
    assert_eq!(
        json["changed_files"],
        serde_json::json!(["web/src/checkout/cart.ts"])
    );
    // GUARD_BASE wins over detection and is strict.
    let output = guard(&root)
        .args(["affected", "--json"])
        .env("GUARD_BASE", "HEAD")
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["base"], "HEAD");
    assert_eq!(json["base_source"], "env");
    guard(&root)
        .args(["affected"])
        .env("GUARD_BASE", "nope")
        .assert()
        .code(2);
}

#[test]
fn affected_no_base_found_exits_2_naming_candidates() {
    let dir = repo_without_a_base();
    let output = guard(dir.path()).arg("affected").output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains(
            "error: no base branch found; tried: main, master, trunk
"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "hint: pass --base <ref> or set GUARD_BASE
"
        ),
        "{err}"
    );
    // An explicit base still works.
    guard(dir.path())
        .args(["affected", "--base", "HEAD"])
        .assert()
        .code(0);
}

#[test]
fn affected_empty_repo_exits_2_no_commits() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    copy_fixture(root, "target.toml", "target.toml");
    copy_fixture(
        root,
        "journeys.json",
        "e2e-playwright/catalog/journeys.json",
    );
    let output = guard(root).arg("affected").output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(
        err.contains(
            "error: repository has no commits yet
"
        ),
        "{err}"
    );
    assert!(
        err.contains(
            "hint: commit once, or use --all
"
        ),
        "{err}"
    );
}

#[test]
fn affected_with_unrelated_history_exits_2() {
    let dir = repo();
    let root = dir.path();
    git(root, &["checkout", "-q", "--orphan", "other"]);
    git(root, &["commit", "-q", "-m", "orphan"]);
    let output = guard(root).arg("affected").output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(
            "error: main and HEAD share no history
"
        ),
        "{}",
        stderr(&output)
    );
}

#[test]
fn test_all_does_not_resolve_base() {
    let dir = repo_without_a_base();
    let output = guard(dir.path()).args(["test", "--all"]).output().unwrap();
    let err = stderr(&output);
    assert!(!err.contains("base"), "{err}");
    // The app is not running: readiness, not the base, ends the run.
    assert_eq!(output.status.code(), Some(3), "{err}");
    let output = guard(dir.path()).arg("test").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("error: no base branch found"));
}

#[test]
fn affected_outside_a_git_repo_is_an_environment_error() {
    let dir = TempDir::new().unwrap();
    copy_fixture(dir.path(), "target.toml", "target.toml");
    copy_fixture(
        dir.path(),
        "journeys.json",
        "e2e-playwright/catalog/journeys.json",
    );
    guard(dir.path()).arg("affected").assert().code(3);
}

#[test]
fn affected_with_an_invalid_config_is_a_usage_error() {
    let dir = repo();
    copy_fixture(dir.path(), "target.invalid.toml", "target.toml");
    let output = guard(dir.path()).arg("affected").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("base_ulr"), "{}", stderr(&output));
}

// ---------------------------------------------------------------- test (argument validation)

#[test]
fn test_rejects_conflicting_and_invalid_flags() {
    let dir = repo();
    let root = dir.path();
    for args in [
        vec!["test", "--all", "--affected"],
        vec!["test", "--all", "--journey", "billing.invoice-pdf"],
        vec!["test", "--headed", "--ui"],
        vec!["test", "--docker", "--ui"],
        vec!["test", "--docker", "--headed"],
        vec!["test", "--shard", "3/2"],
        vec!["test", "--shard", "0/2"],
        vec!["test", "--shard", "x"],
        vec!["test", "--journey", "nope.nope"],
    ] {
        let output = guard(root).args(&args).output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn test_requires_a_ready_app() {
    let dir = repo();
    let output = guard(dir.path()).args(["test", "--all"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    // The fixture has no [app].start and no [[services]]: self mode.
    assert!(
        stderr(&output).contains("hint: start your app, then run guard up again"),
        "{}",
        stderr(&output)
    );
    assert!(stdout(&output).contains("@journey:billing.invoice-pdf"));
}

#[test]
fn test_with_a_start_command_still_hints_guard_up() {
    let dir = repo();
    let root = dir.path();
    target_with_app(root, Some("exit 0"), None, 9);
    let output = guard(root).args(["test", "--all"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(
        stderr(&output).contains("hint: run guard up\n"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn test_in_self_mode_lists_the_commented_commands_before_playwright_runs() {
    let dir = repo();
    let root = dir.path();
    write_self_block(root);
    let npx = fake_npx(root);
    let output = guard(root)
        .args(["test", "--all"])
        .env("GUARD_NPX", &npx)
        .env("FAKE_ENV_OUT", root.join("env.out"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let err = stderr(&output);
    assert!(
        err.contains("error: app not ready: GET http://127.0.0.1:9/healthz -> "),
        "{err}"
    );
    assert!(err.contains(SELF_HINT), "{err}");
    assert!(!root.join("env.out").exists(), "Playwright must not run");
}

#[test]
fn test_runs_playwright_with_the_runner_env_and_summarises_the_report() {
    let dir = repo();
    let root = dir.path();
    let url = serve_ok();
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace("http://127.0.0.1:9", &url);
    write(root, "target.toml", &target);
    let env_out = root.join("env.txt");
    let output = guard(root)
        .args(["test", "--journey", "billing.refund"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_ENV_OUT", &env_out)
        .env("FAKE_EXIT", "1")
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env(
            "PLAYWRIGHT_JSON_OUTPUT_NAME",
            root.join("e2e-playwright/results/report.json"),
        )
        .env("GUARD_TEST_TOKEN", "dev-token")
        .output()
        .unwrap();
    // The fake only writes the report when PLAYWRIGHT_JSON_OUTPUT_NAME is set, which we did above.
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "{out}\n{}", stderr(&output));
    assert!(
        out.contains("passed 1  flaky 1  failed 1  skipped 0  (20.0s)\n"),
        "{out}"
    );
    assert!(
        out.contains("FAILED @journey:billing.invoice-pdf  tests/journeys/billing/invoice-pdf.spec.ts:7  issues the invoice\n"),
        "{out}"
    );
    assert!(
        out.contains("  Error: Timeout 30000ms exceeded.\n"),
        "{out}"
    );
    assert!(out.contains("report: guard report\n"), "{out}");

    let env = fs::read_to_string(env_out).unwrap().replace("\r\n", "\n");
    assert!(env.contains("E2E_TAGS=@journey:billing.refund\n"), "{env}");
    assert!(env.contains(&format!("TARGET_BASE_URL={url}\n")), "{env}");
    assert!(env.contains("GUARD_ROLES=customer\n"), "{env}");
    assert!(env.contains("GUARD_VIDEO=on-first-retry\n"), "{env}");
    assert!(env.contains("GUARD_TEST_TOKEN=dev-token\n"), "{env}");
    assert!(env.contains("GUARD_LOGIN_PATH=/__test__/login\n"), "{env}");
    assert!(env.contains("GUARD_SEED_PATH=/__test__/seed\n"), "{env}");
    assert!(env.contains("RUN_ID=r"), "{env}");
    assert!(env.contains("ARGS=playwright test"), "{env}");
}

// ---------------------------------------------------------------- gate plan

fn e2e_with_specs(root: &Path) {
    for spec in [
        "tests/journeys/checkout/customer-card.spec.ts",
        "tests/journeys/billing/invoice-pdf.spec.ts",
        "tests/smoke/healthz.spec.ts",
        "tests/setup/ignored.spec.ts",
        "tests/setup/auth.setup.ts",
    ] {
        write(root, &format!("e2e-playwright/{spec}"), "// spec\n");
    }
}

#[test]
fn gate_plan_balances_shards_by_history() {
    let dir = repo();
    let root = dir.path();
    e2e_with_specs(root);
    copy_fixture(root, "durations.json", ".guard/durations.json");
    let output = guard(root)
        .args(["gate", "plan", "--shards", "2", "--out", "plan"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "shard 1/2: 1 file, est 12.5s\nshard 2/2: 2 files, est 9.9s\n"
    );
    let one: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("plan/shard-1.json")).unwrap()).unwrap();
    let two: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("plan/shard-2.json")).unwrap()).unwrap();
    assert_eq!(
        one,
        serde_json::json!({"files": ["**/tests/journeys/checkout/customer-card.spec.ts"]})
    );
    assert_eq!(
        two,
        serde_json::json!({"files": ["**/tests/journeys/billing/invoice-pdf.spec.ts", "**/tests/smoke/healthz.spec.ts"]})
    );
}

#[test]
fn gate_plan_without_history_weighs_files_equally_and_pads_extra_shards() {
    let dir = repo();
    let root = dir.path();
    e2e_with_specs(root);
    let output = guard(root)
        .args(["gate", "plan", "--shards", "4", "--out", "plan"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).ends_with("shard 4/4: 0 files, est 0.0s\n"),
        "{}",
        stdout(&output)
    );
    let four: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join("plan/shard-4.json")).unwrap()).unwrap();
    assert_eq!(four, serde_json::json!({"files": []}));
}

#[test]
fn gate_plan_rejects_zero_shards_and_malformed_history() {
    let dir = repo();
    let root = dir.path();
    e2e_with_specs(root);
    guard(root)
        .args(["gate", "plan", "--shards", "0", "--out", "plan"])
        .assert()
        .code(2);
    write(root, ".guard/durations.json", "{ not json");
    guard(root)
        .args(["gate", "plan", "--shards", "2", "--out", "plan"])
        .assert()
        .code(2);
}

// ---------------------------------------------------------------- gate shard

#[test]
fn gate_shard_with_an_empty_plan_runs_nothing() {
    let dir = repo();
    let root = dir.path();
    write(root, "plan/shard-1.json", "{\"files\": []}");
    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "shard 1: nothing to run\n");
}

#[test]
fn gate_shard_with_a_missing_or_invalid_plan_is_a_usage_error() {
    let dir = repo();
    let root = dir.path();
    guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .assert()
        .code(2);
    write(root, "plan/shard-1.json", "{\"nope\": 1}");
    guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .assert()
        .code(2);
}

#[test]
fn gate_shard_passes_shard_variables_and_requires_a_blob() {
    let dir = repo();
    let root = dir.path();
    let url = serve_ok();
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace("http://127.0.0.1:9", &url);
    write(root, "target.toml", &target);
    write(
        root,
        "plan/shard-2.json",
        "{\"files\": [\"**/tests/smoke/healthz.spec.ts\"]}",
    );
    let env_out = root.join("env.txt");
    let output = guard(root)
        .args(["gate", "shard", "--index", "2", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_ENV_OUT", &env_out)
        .env("FAKE_EXIT", "1")
        .env("RUN_ID", "12345-1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        root.join("e2e-playwright/blob-report/report-2.zip")
            .exists()
    );
    let env = fs::read_to_string(env_out).unwrap().replace("\r\n", "\n");
    assert!(env.contains("CI=1\n"), "{env}");
    assert!(env.contains("GUARD_SHARD_INDEX=2\n"), "{env}");
    assert!(env.contains("RUN_ID=12345-1\n"), "{env}");
    assert!(
        env.contains(
            "E2E_TAGS=@journey:billing.invoice-pdf,@journey:checkout.customer-card,@smoke\n"
        ),
        "{env}"
    );
    let plan_line = env
        .lines()
        .find(|l| l.starts_with("GUARD_SHARD_PLAN="))
        .unwrap();
    assert!(plan_line.ends_with("/plan/shard-2.json"), "{plan_line}");
    assert!(!plan_line.contains('\\'), "{plan_line}");

    // Playwright exiting outside {0, 1} is an infrastructure error.
    let output = guard(root)
        .args(["gate", "shard", "--index", "2", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_EXIT", "2")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
}

// ---------------------------------------------------------------- gate finalize

fn blobs(root: &Path) {
    write(root, "blobs/report-1.zip", "zip");
    write(root, "blobs/report-2.zip", "zip");
}

#[test]
fn gate_finalize_with_a_missing_dir_or_no_blobs() {
    let dir = repo();
    let root = dir.path();
    guard(root)
        .args(["gate", "finalize", "--blobs", "missing"])
        .assert()
        .code(2);
    fs::create_dir_all(root.join("empty")).unwrap();
    guard(root)
        .args(["gate", "finalize", "--blobs", "empty"])
        .assert()
        .code(3);
}

#[test]
fn gate_finalize_red_writes_verdict_and_durations() {
    let dir = repo();
    let root = dir.path();
    blobs(root);
    copy_fixture(root, "durations.json", ".guard/durations.json");
    let env_out = root.join("env.txt");
    let output = guard(root)
        .args(["gate", "finalize", "--blobs", "blobs"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("FAKE_ENV_OUT", &env_out)
        .env("RUN_ID", "12345-1")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "{out}\n{}", stderr(&output));
    // No base commit can be discovered in this repo (no PR, no origin/HEAD).
    assert_eq!(
        out,
        "baseline: no base commit, failures stay blocking\n\
         verdict: red  (passed 1, flaky 1, failed 1, skipped 0; regressions 0, pre-existing 0)\n\
         FAILED       tests/journeys/billing/invoice-pdf.spec.ts:7  billing.invoice-pdf  guard-fp-6892d075\n\
         FLAKY (blocking: smoke)  tests/journeys/checkout/customer-card.spec.ts:9  checkout.customer-card\n\
         evidence: storage not configured, nothing uploaded\n\
         jira: not configured\n"
    );
    let env = fs::read_to_string(env_out).unwrap();
    assert!(env.contains("ARGS=playwright merge-reports "), "{env}");
    assert!(env.contains("json,html"), "{env}");

    let verdict: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(".guard/verdict.json")).unwrap())
            .unwrap();
    assert_eq!(verdict["verdict"], "red");
    assert_eq!(verdict["run_id"], "12345-1");
    assert_eq!(
        verdict["baseline"],
        serde_json::json!({"base_sha": null, "outcomes": {}})
    );
    assert_eq!(verdict["not_implemented"], serde_json::json!([]));
    assert_eq!(verdict["counts"]["failed"], 1);
    assert_eq!(verdict["tests"][1]["blocking"], true);
    assert!(verdict["generated_at"].as_str().unwrap().ends_with('Z'));

    let durations: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(".guard/durations.json")).unwrap())
            .unwrap();
    let checkout = &durations["files"]["tests/journeys/checkout/customer-card.spec.ts"];
    assert_eq!(checkout["duration_ms"], 15340); // round((12450 + 18230) / 2)
    assert_eq!(checkout["samples"], 8);
    assert_eq!(
        durations["files"]["tests/smoke/healthz.spec.ts"]["duration_ms"],
        45
    );
}

#[test]
fn gate_finalize_green_when_only_non_smoke_tests_are_flaky() {
    let dir = repo();
    let root = dir.path();
    blobs(root);
    let output = guard(root)
        .args(["gate", "finalize", "--blobs", "blobs"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.green.json"))
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "{out}\n{}", stderr(&output));
    assert!(
        out.starts_with(
            "verdict: green  (passed 2, flaky 1, failed 0, skipped 0; regressions 0, pre-existing 0)\n"
        ),
        "{out}"
    );
    assert!(
        out.contains("FLAKY  tests/journeys/billing/invoice-pdf.spec.ts:7  billing.invoice-pdf\n"),
        "{out}"
    );
    let durations = fs::read_to_string(root.join(".guard/durations.json")).unwrap();
    assert!(durations.contains("\"samples\": 1"), "{durations}");
}

#[test]
fn gate_finalize_with_zero_tests_or_a_failed_merge_is_an_infrastructure_error() {
    let dir = repo();
    let root = dir.path();
    blobs(root);
    guard(root)
        .args(["gate", "finalize", "--blobs", "blobs"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.empty.json"))
        .assert()
        .code(3);
    guard(root)
        .args(["gate", "finalize", "--blobs", "blobs"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("FAKE_EXIT", "1")
        .assert()
        .code(3);
    assert!(!root.join(".guard/verdict.json").exists());
}

// ---------------------------------------------------------------- up / down / report

#[test]
fn up_is_a_no_op_when_the_app_is_already_ready() {
    let dir = repo();
    let root = dir.path();
    let url = serve_ok();
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace("http://127.0.0.1:9", &url);
    write(root, "target.toml", &target);
    let output = guard(root).arg("up").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!("up: already ready at {url}/healthz\n")
    );
}

#[test]
fn up_fails_when_the_start_command_fails() {
    let dir = repo();
    let root = dir.path();
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace("base_url = ", "start = \"exit 7\"\nbase_url = ");
    write(root, "target.toml", &target);
    let output = guard(root)
        .args(["up", "--timeout", "10"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(root.join(".guard/app.log").exists());
    // A later `guard down` must not kill whatever reuses the dead PID.
    assert!(!root.join(".guard/app.pid").exists());
    assert!(!root.join(".guard/app.identity").exists());
}

#[test]
fn up_times_out_without_a_start_command() {
    let dir = repo();
    let output = guard(dir.path())
        .args(["up", "--timeout", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
}

/// The fixture's target.toml (self mode) plus a commented detected-commands block.
fn write_self_block(root: &Path) {
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace(
            "base_url = \"http://127.0.0.1:9\"\n",
            "base_url = \"http://127.0.0.1:9\"\n\n\
         # guard does not start the app: you run it yourself (guard init --runner self).\n\
         # Detected commands, uncomment to let guard start it (needed in CI):\n\
         # start = \"docker compose up -d --wait db\"\n\
         #\n\
         # [[services]]\n\
         # name = \"web\"\n\
         # cwd = \"web\"\n\
         # start = \"npm run dev\"\n",
        );
    write(root, "target.toml", &target);
}

const SELF_HINT: &str = "hint: guard does not start the app (no [app].start in target.toml); start it yourself:\n\
hint:   docker compose up -d --wait db\n\
hint:   web (in web): npm run dev\n\
hint: then run guard up again (or uncomment those lines so guard starts them)\n";

#[test]
fn up_in_self_mode_waits_without_starting_anything_and_hints_on_timeout() {
    let dir = repo();
    let root = dir.path();
    let output = guard(root).args(["up", "--timeout", "1"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(
        stdout(&output),
        "up: waiting for your app at http://127.0.0.1:9/healthz (start it yourself; guard does not start it)\n"
    );
    let err = stderr(&output);
    assert!(
        err.contains("error: app not ready at http://127.0.0.1:9/healthz after 1s: "),
        "{err}"
    );
    assert!(
        err.contains("hint: start your app, then run guard up again\n"),
        "{err}"
    );
    assert!(!root.join(".guard").exists(), "self mode writes nothing");

    write_self_block(root);
    let output = guard(root).args(["up", "--timeout", "1"]).output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(stderr(&output).contains(SELF_HINT), "{}", stderr(&output));
    assert!(!root.join(".guard").exists(), "self mode spawns nothing");
}

#[test]
fn up_in_self_mode_is_ready_once_the_developer_starts_the_app() {
    let dir = repo();
    let root = dir.path();
    let port = free_port();
    target_with_app(root, None, None, port);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    let output = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    let url = format!("http://127.0.0.1:{port}/healthz");
    assert!(
        out.starts_with(&format!(
            "up: waiting for your app at {url} (start it yourself; guard does not start it)\nup: ready at {url} in "
        )),
        "{out}"
    );
}

/// Runs `guard <args>` in `root` with stdout on a pipe the test reads to EOF,
/// like `guard up | tail`; returns the output and the time until EOF, or
/// `None` when the pipe stays open for `limit`.
fn read_until_eof(
    root: &Path,
    args: &[&str],
    limit: std::time::Duration,
) -> Option<(String, std::time::Duration)> {
    let begin = std::time::Instant::now();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("guard"))
        .args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut pipe = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = pipe.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let result = rx.recv_timeout(limit).ok().map(|t| (t, begin.elapsed()));
    let _ = child.wait();
    result
}

#[test]
fn up_releases_the_callers_pipe_while_the_app_keeps_running() {
    let port = free_port();
    let dir = repo_with_app(port);
    let root = dir.path();
    // `localhost` resolves to ::1 first on Windows while the app binds 127.0.0.1.
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace(&format!("127.0.0.1:{port}"), &format!("localhost:{port}"));
    write(root, "target.toml", &target);
    let piped = read_until_eof(root, &["up", "--timeout", "60"], Duration::from_secs(30));
    let app_alive = std::net::TcpStream::connect(("127.0.0.1", port)).is_ok();
    let down = guard(root).arg("down").output().unwrap();
    let (out, elapsed) = piped.expect("`guard up | tail` never saw EOF: the app holds the pipe");
    assert!(
        !root.join(".guard/services").exists(),
        "without [[services]] nothing is written under .guard/services/"
    );
    assert!(out.starts_with("up: ready at"), "{out}");
    assert!(app_alive, "the app must outlive `guard up`");
    assert_eq!(down.status.code(), Some(0), "{}", stderr(&down));
    // The app binds 127.0.0.1; probing must not wait on a refused ::1 first.
    assert!(
        elapsed < Duration::from_secs(4),
        "guard up took {elapsed:?}"
    );
}

#[test]
fn down_with_nothing_to_stop() {
    let dir = repo();
    let output = guard(dir.path()).arg("down").output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    // The fixture has no [app].start and no [[services]]: self mode.
    assert_eq!(
        stdout(&output),
        "down: nothing to stop: guard did not start the app\n"
    );
}

#[test]
fn down_with_a_start_command_and_nothing_to_stop() {
    let dir = repo();
    let root = dir.path();
    target_with_app(root, Some("exit 0"), None, 9);
    let output = guard(root).arg("down").output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout(&output), "down: nothing to stop\n");
}

#[test]
fn report_without_a_html_report_is_an_environment_error() {
    let dir = repo();
    let output = guard(dir.path()).arg("report").output().unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert!(stderr(&output).contains("hint: run guard test first"));
}

// ---------------------------------------------------------------- regressions

/// A free local port (the listener is dropped before it is used).
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// target.toml with `[app].start` / `[app].stop` and the base URL on `port`.
fn target_with_app(root: &Path, start: Option<&str>, stop: Option<&str>, port: u16) {
    let mut app = String::new();
    if let Some(start) = start {
        app.push_str(&format!("start = '{start}'\n"));
    }
    if let Some(stop) = stop {
        app.push_str(&format!("stop = '{stop}'\n"));
    }
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace(
            "base_url = \"http://127.0.0.1:9\"",
            &format!("{app}base_url = \"http://127.0.0.1:{port}\""),
        );
    write(root, "target.toml", &target);
}

/// A process that stays alive for 60 s.
fn long_running() -> std::process::Child {
    StdCommand::new("node")
        .args(["-e", "setTimeout(() => {}, 60000)"])
        .spawn()
        .expect("node is on PATH")
}

#[test]
fn an_invalid_inherited_run_id_is_a_usage_error() {
    let dir = repo();
    let root = dir.path();
    blobs(root);
    let output = guard(root)
        .args(["gate", "finalize", "--blobs", "blobs"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.green.json"))
        .env("RUN_ID", "not valid/../x")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("RUN_ID"), "{}", stderr(&output));
    assert!(!root.join(".guard/verdict.json").exists());

    write(
        root,
        "plan/shard-1.json",
        "{\"files\": [\"**/tests/smoke/healthz.spec.ts\"]}",
    );
    let env_out = root.join("env.txt");
    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_ENV_OUT", &env_out)
        .env("RUN_ID", "a b")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(!env_out.exists(), "Playwright must not be launched");
}

#[test]
fn gate_shard_without_a_blob_is_an_infrastructure_error() {
    let dir = repo();
    let root = dir.path();
    let url = serve_ok();
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace("http://127.0.0.1:9", &url);
    write(root, "target.toml", &target);
    write(
        root,
        "plan/shard-1.json",
        "{\"files\": [\"**/tests/smoke/healthz.spec.ts\"]}",
    );
    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_NO_BLOB", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("no blob report produced"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn gate_shard_starts_the_app_when_it_is_not_ready_and_stops_it_afterwards() {
    let dir = repo();
    let root = dir.path();
    let port = free_port();
    write(
        root,
        "server.js",
        "require('http').createServer((q, s) => s.end('ok')).listen(Number(process.argv[2]), '127.0.0.1');\nsetTimeout(() => process.exit(0), 60000);\n",
    );
    write(
        root,
        "stop.js",
        "require('fs').writeFileSync('stopped.txt', 'x');\n",
    );
    target_with_app(
        root,
        Some(&format!("node server.js {port}")),
        Some("node stop.js"),
        port,
    );
    write(
        root,
        "plan/shard-1.json",
        "{\"files\": [\"**/tests/smoke/healthz.spec.ts\"]}",
    );
    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(root.join("stopped.txt").exists(), "[app].stop ran");
    assert!(
        !root.join(".guard/app.pid").exists(),
        "the PID file is gone"
    );

    // The app is also stopped when Playwright crashes.
    fs::remove_file(root.join("stopped.txt")).unwrap();
    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_EXIT", "2")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(root.join("stopped.txt").exists(), "[app].stop ran");
}

#[test]
fn down_still_clears_the_pid_file_when_stop_fails() {
    let dir = repo();
    let root = dir.path();
    target_with_app(root, None, Some("exit 5"), free_port());
    let mut child = long_running();
    write(root, ".guard/app.pid", &child.id().to_string());
    let output = guard(root).arg("down").output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("[app].stop exited"),
        "{}",
        stderr(&output)
    );
    assert!(!root.join(".guard/app.pid").exists());
    // No identity was recorded, so the recorded PID was terminated.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "process still alive");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn down_never_kills_a_pid_whose_identity_changed() {
    let dir = repo();
    let root = dir.path();
    let mut child = long_running();
    write(root, ".guard/app.pid", &child.id().to_string());
    write(root, ".guard/app.identity", "not the process guard started");
    let output = guard(root).arg("down").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("no longer the process"),
        "{}",
        stderr(&output)
    );
    assert!(!root.join(".guard/app.pid").exists());
    assert!(!root.join(".guard/app.identity").exists());
    assert!(
        child.try_wait().unwrap().is_none(),
        "the reused PID survives"
    );
    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn init_warns_about_directories_with_colliding_area_keys() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    write(root, "my-app/a.ts", "x\n");
    write(root, "my_app/b.ts", "x\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    let output = guard(root).arg("init").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).contains(
            "warning: directories \"my-app\" and \"my_app\" both map to area key `my_app`; proposed `my_app_2` for \"my_app\""
        ),
        "{}",
        stderr(&output)
    );
    let target = fs::read_to_string(root.join("target.toml")).unwrap();
    assert!(target.contains("[areas.my_app]"), "{target}");
    assert!(target.contains("[areas.my_app_2]"), "{target}");
    assert!(target.contains("\"my_app/**\""), "{target}");
}

// ---------------------------------------------------------------- Phase 2

/// A tiny app answering 200 on every path: `node app.js <port>`.
const APP_JS: &str = "require('http').createServer((q, s) => { s.end('ok'); }).listen(Number(process.argv[2]), '127.0.0.1');\n";

/// `repo()` plus a startable app on `port`, committed so the baseline
/// worktree has it too.
fn repo_with_app(port: u16) -> TempDir {
    let dir = repo();
    let root = dir.path();
    write(root, "app.js", APP_JS);
    target_with_app(root, Some(&format!("node app.js {port}")), None, port);
    write(root, ".gitignore", ".guard/\nblobs/\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "app"]);
    dir
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn head(root: &Path) -> String {
    String::from_utf8(
        StdCommand::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

fn invoice(report: &serde_json::Value) -> serde_json::Value {
    report["tests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["journey"] == "billing.invoice-pdf")
        .unwrap()
        .clone()
}

#[test]
fn gate_finalize_without_baseline_keeps_every_failure_blocking() {
    let dir = repo();
    let root = dir.path();
    blobs(root);
    let output = guard(root)
        .args([
            "gate",
            "finalize",
            "--blobs",
            "blobs",
            "--no-baseline",
            "--no-report",
        ])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("RUN_ID", "nb-1")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "{out}\n{}", stderr(&output));
    assert!(!out.contains("baseline:"), "{out}");
    assert!(!out.contains("jira:"), "{out}");
    let verdict = read_json(&root.join(".guard/verdict.json"));
    assert_eq!(verdict["baseline"], serde_json::Value::Null);

    let report = read_json(&root.join(".guard/gate-report.json"));
    assert_eq!(report["version"], 2);
    assert_eq!(report["run_id"], "nb-1");
    assert_eq!(report["verdict"], "red");
    assert_eq!(report["head_sha"].as_str().unwrap().len(), 40);
    assert_eq!(report["repo"], "shop");
    let invoice = invoice(&report);
    assert_eq!(
        invoice["test_id"],
        "chromium::tests/journeys/billing/invoice-pdf.spec.ts::issues the invoice"
    );
    assert_eq!(invoice["fingerprint"], "6892d075");
    assert_eq!(invoice["baseline"], serde_json::Value::Null);
    assert_eq!(invoice["evidence"]["fingerprint"], "6892d075");
    assert_eq!(invoice["evidence"]["attempts"].as_array().unwrap().len(), 3);
}

#[test]
fn gate_finalize_classifies_failures_against_the_base_commit() {
    let port = free_port();
    let dir = repo_with_app(port);
    let root = dir.path();
    let base = head(root);
    blobs(root);
    let base_env = root.join("base-env.txt");
    let output = guard(root)
        .args(["gate", "finalize", "--blobs", "blobs", "--base", &base])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("FAKE_BASE_REPORT", fixture("report.base-passes.json"))
        .env("FAKE_BASE_ENV_OUT", &base_env)
        .env("RUN_ID", "base-1")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "{out}\n{}", stderr(&output));
    assert!(
        out.contains("regressions 1, pre-existing 0)"),
        "{out}\n{}",
        stderr(&output)
    );
    assert!(
        out.contains(
            "REGRESSION   tests/journeys/billing/invoice-pdf.spec.ts:7  billing.invoice-pdf  guard-fp-6892d075\n"
        ),
        "{out}"
    );
    let verdict = read_json(&root.join(".guard/verdict.json"));
    assert_eq!(verdict["baseline"]["base_sha"], base.as_str());
    assert_eq!(
        verdict["baseline"]["outcomes"]["chromium::tests/journeys/billing/invoice-pdf.spec.ts::issues the invoice"],
        "regression"
    );
    // The base run: whole spec file, one project, 3 attempts, no tag filter.
    // (cmd.exe echoes arguments containing `=` with quotes.)
    let env = fs::read_to_string(&base_env).unwrap().replace('"', "");
    assert!(
        env.contains(
            "ARGS=playwright test tests/journeys/billing/invoice-pdf.spec.ts --project=chromium --retries=2 --reporter=json"
        ),
        "{env}"
    );
    assert!(!env.contains("E2E_TAGS="), "{env}");
    assert!(env.contains("GUARD_VIDEO=off"), "{env}");
    // The worktree and the app are gone.
    assert!(!root.join(".guard/baseline").exists());
    let worktrees = StdCommand::new("git")
        .args(["worktree", "list"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&worktrees.stdout).lines().count(),
        1
    );
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the base app is still running"
    );

    // Same failure at base: pre-existing, does not block. The smoke flaky still does.
    let output = guard(root)
        .args([
            "gate",
            "finalize",
            "--blobs",
            "blobs",
            "--base",
            &base,
            "--no-report",
        ])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("FAKE_BASE_REPORT", fixture("report.base-fails.json"))
        .env("RUN_ID", "base-2")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert!(
        out.contains("regressions 0, pre-existing 1)"),
        "{out}\n{}",
        stderr(&output)
    );
    assert!(
        out.contains("PRE-EXISTING tests/journeys/billing/invoice-pdf.spec.ts:7"),
        "{out}"
    );
    let invoice = invoice(&read_json(&root.join(".guard/gate-report.json")));
    assert_eq!(invoice["baseline"], "pre_existing");
    assert_eq!(invoice["blocking"], false);
}

#[test]
fn gate_finalize_marks_failures_unavailable_when_another_app_is_running() {
    let url = serve_ok();
    let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
    let dir = repo_with_app(port);
    let root = dir.path();
    blobs(root);
    let output = guard(root)
        .args([
            "gate",
            "finalize",
            "--blobs",
            "blobs",
            "--base",
            "HEAD",
            "--no-report",
        ])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("FAKE_BASE_REPORT", fixture("report.base-passes.json"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(
        out.contains("UNAVAILABLE  tests/journeys/billing/invoice-pdf.spec.ts:7"),
        "{out}"
    );
    assert!(
        stderr(&output).contains("another app is running"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn agents_check_and_sync_validate_their_inputs() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path())
        .args(["agents", "check"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("agents.toml"),
        "{}",
        stderr(&output)
    );

    // A valid agents.toml and the repo's agents/ sources: sync renders, then --check is clean.
    let root = dir.path();
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    std::fs::write(
        root.join("agents.toml"),
        std::fs::read_to_string(repo_root.join("guard-agents/tests/fixtures/agents.toml")).unwrap(),
    )
    .unwrap();
    let output = guard(root).args(["agents", "check"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("ok"), "{}", stdout(&output));
    copy_dir(&repo_root.join("agents"), &root.join("agents"));
    let output = guard(root)
        .args(["agents", "sync", "--check"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "dirty before the first sync: {}",
        stdout(&output)
    );
    let output = guard(root).args(["agents", "sync"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let output = guard(root)
        .args(["agents", "sync", "--check"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn serve_refuses_to_start_without_a_token_or_with_invalid_thresholds() {
    let dir = repo();
    let root = dir.path();
    let output = guard(root).args(["serve", "--fake"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("GUARD_SERVER_TOKEN"),
        "{}",
        stderr(&output)
    );
    write(root, "thresholds.toml", "agent_suitable_min = 2.0\n");
    let output = guard(root)
        .args(["serve", "--fake"])
        .env("GUARD_SERVER_TOKEN", "t")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("thresholds.toml"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn repro_needs_a_server() {
    let dir = repo();
    let output = guard(dir.path())
        .args(["repro", "chromium::tests/a.spec.ts::x", "--pr", "3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("[server]"), "{}", stderr(&output));
}

fn with_server(root: &Path, url: &str) {
    let target = fs::read_to_string(root.join("target.toml")).unwrap();
    write(
        root,
        "target.toml",
        &format!("{target}\n[server]\nurl = \"{url}\"\n"),
    );
}

#[test]
fn affected_with_an_unreachable_server_runs_every_active_journey() {
    let dir = repo();
    let root = dir.path();
    feature_branch(root);
    with_server(root, &format!("http://127.0.0.1:{}", free_port()));
    let output = guard(root)
        .args(["affected", "--json"])
        .env("GUARD_SERVER_TOKEN", "t")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["impact"],
        serde_json::json!({"source": "server", "status": "engine_unavailable", "threshold": 0.25})
    );
    assert_eq!(
        json["tags"],
        serde_json::json!([
            "@journey:billing.invoice-pdf",
            "@journey:checkout.customer-card",
            "@smoke",
            "@ticket:SHOP-123"
        ])
    );
    let text = guard(root)
        .args(["affected"])
        .env("GUARD_SERVER_TOKEN", "t")
        .output()
        .unwrap();
    assert!(
        stdout(&text).contains("impact: engine unavailable, running every journey"),
        "{}",
        stdout(&text)
    );
}

#[test]
fn affected_without_an_engine_keeps_the_path_rules() {
    let dir = repo();
    let root = dir.path();
    feature_branch(root);
    let output = guard(root).args(["affected", "--json"]).output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["impact"],
        serde_json::json!({"source": "none", "status": "not_configured"})
    );
}

/// One HTTP/1.1 request over a fresh connection; returns status and body.
fn http(
    base: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let authority = base.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(authority).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        request.push_str(&format!("{k}: {v}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    let text = String::from_utf8_lossy(&response).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// Kills the server when the test ends, even on panic.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until(mut ok: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn serve_in_fake_mode_triages_webhooks_and_receives_gate_results() {
    let app_port = free_port();
    let dir = repo_with_app(app_port);
    let root = dir.path();
    let target = fs::read_to_string(root.join("target.toml"))
        .unwrap()
        .replace("kind = \"jira\"", "kind = \"jira\"\nproject = \"SHOP\"");
    write(root, "target.toml", &target);
    git(root, &["commit", "-q", "-am", "project"]);

    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let mut cmd = StdCommand::new(assert_cmd::cargo::cargo_bin("guard"));
    for var in LEAKY_ENV {
        cmd.env_remove(var);
    }
    cmd.env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS);
    let server = Running(
        cmd.args(["serve", "--fake", "--listen", &format!("127.0.0.1:{port}")])
            .current_dir(root)
            .env("GUARD_SERVER_TOKEN", "srv-token")
            .env("GUARD_JIRA_WEBHOOK_SECRET", "hook-secret")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_until(
        || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
        "guard serve",
    );
    let (status, body) = http(&base, "GET", "/healthz", &[], "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"database\":\"ok\""), "{body}");

    // A recorded Jira delivery: wrong secret first, then the right one.
    let delivery = fs::read_to_string(fixture("jira_issue_created.json")).unwrap();
    let json_type = ("Content-Type", "application/json");
    let (status, _) = http(
        &base,
        "POST",
        "/webhooks/jira",
        &[("Authorization", "Bearer nope"), json_type],
        &delivery,
    );
    assert_eq!(status, 401);
    let (status, body) = http(
        &base,
        "POST",
        "/webhooks/jira",
        &[("Authorization", "Bearer hook-secret"), json_type],
        &delivery,
    );
    assert_eq!(status, 202, "{body}");
    assert!(body.contains("SHOP-7"), "{body}");
    // The decision is logged (shadow mode: the default thresholds).
    wait_until(
        || {
            http(&base, "GET", "/decisions", &[], "")
                .1
                .contains("SHOP-7")
        },
        "the triage decision",
    );
    let decisions = http(&base, "GET", "/decisions", &[], "").1;
    assert!(decisions.to_lowercase().contains("shadow"), "{decisions}");

    // guard affected asks the server for impact.
    with_server(root, &base);
    feature_branch(root);
    let output = guard(root)
        .args(["affected", "--json"])
        .env("GUARD_SERVER_TOKEN", "srv-token")
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["impact"]["source"], "server", "{json}");
    assert_eq!(json["impact"]["status"], "ok", "{json}");
    let checkout = &json["journeys"][0];
    assert_eq!(checkout["id"], "checkout.customer-card");
    assert!(
        checkout["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "impact" && r["probability"] == 0.9),
        "{json}"
    );

    // finalize: the regression is reported through the server (Bug created).
    blobs(root);
    let base_sha = head(root);
    let output = guard(root)
        .args(["gate", "finalize", "--blobs", "blobs", "--base", &base_sha])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_REPORT", fixture("report.red.json"))
        .env("FAKE_BASE_REPORT", fixture("report.base-passes.json"))
        .env("GUARD_SERVER_TOKEN", "srv-token")
        .env("RUN_ID", "srv-1")
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(1), "{out}\n{}", stderr(&output));
    assert!(
        out.contains("guard-fp-6892d075  SHOP-1001 (created)"),
        "{out}\n{}",
        stderr(&output)
    );
    assert!(out.contains("jira: 1 created, 0 commented"), "{out}");
    let guards = http(&base, "GET", "/guards", &[], "").1;
    assert!(guards.contains("srv-1"), "{guards}");
    drop(server);
}

#[test]
fn mcp_serves_the_guard_tools_over_stdio() {
    let dir = repo();
    let root = dir.path();
    feature_branch(root);
    let messages = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"guard_catalog","arguments":{"area":"billing"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"guard_affected","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"guard_failure","arguments":{"test_id":"chromium::x::y"}}}"#,
    ];
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("guard"));
    for var in LEAKY_ENV {
        child.env_remove(var);
    }
    child.env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS);
    let mut child = child
        .arg("mcp")
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut answers = std::collections::BTreeMap::new();
    for message in messages {
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
        let id = serde_json::from_str::<serde_json::Value>(message).unwrap()["id"].clone();
        if id.is_null() {
            continue;
        }
        let mut line = String::new();
        std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
        let answer: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answer["id"], id, "{line}");
        answers.insert(id.as_i64().unwrap(), answer);
    }
    drop(stdin);
    let _ = child.wait();

    assert_eq!(answers[&1]["result"]["protocolVersion"], "2025-06-18");
    let names: Vec<&str> = answers[&2]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "guard_affected",
            "guard_test",
            "guard_failure",
            "guard_repro",
            "guard_catalog",
            "guard_history",
            "guard_map"
        ]
    );
    let catalog = &answers[&3]["result"]["structuredContent"]["journeys"];
    let ids: Vec<&str> = catalog
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["billing.invoice-pdf", "billing.refund"]);
    let affected = &answers[&4]["result"];
    assert_eq!(affected["isError"], false, "{affected}");
    assert_eq!(affected["structuredContent"]["ticket"], "SHOP-123");
    assert_eq!(
        affected["structuredContent"]["impact"]["status"],
        "not_configured"
    );
    let failure = &answers[&5]["result"];
    assert_eq!(failure["isError"], true, "{failure}");
    assert_eq!(failure["structuredContent"]["error"], "not_found");
}

// ---------------------------------------------------------------- [[services]]

/// `node svc.js <port> <name> [dependency port | fail]`, run from a service `cwd`:
/// exits 1 with `fail` or while the dependency port does not answer, else
/// appends its name to `../order.txt` and answers 200 on every path.
const SVC_JS: &str = r#"const http = require('http');
const fs = require('fs');
const [port, name, dep] = process.argv.slice(2);
const start = () => {
  fs.appendFileSync('../order.txt', name + '\n');
  http.createServer((q, s) => s.end('ok')).listen(Number(port), '127.0.0.1', () => console.log(name + ' listening'));
};
if (dep === 'fail') {
  console.log(name + ': failing on purpose');
  process.exit(1);
} else if (dep) {
  http.get('http://127.0.0.1:' + dep + '/', () => start()).on('error', () => {
    console.log(name + ': dependency down');
    process.exit(1);
  });
} else {
  start();
}
setTimeout(() => process.exit(0), 60000);
"#;

const STOP_JS: &str = "require('fs').appendFileSync('../stops.txt', process.argv[2] + '\\n');\n";

/// A repo whose base URL is served on `base_port` and whose `[[services]]` are `services`.
fn repo_with_services(base_port: u16, services: &str) -> TempDir {
    let dir = repo();
    let root = dir.path();
    write(root, "svc.js", SVC_JS);
    write(root, "stop.js", STOP_JS);
    write(root, "api/.keep", "");
    write(root, "web/.keep", "");
    target_with_app(root, None, None, base_port);
    let target = fs::read_to_string(root.join("target.toml")).unwrap();
    write(root, "target.toml", &format!("{target}\n{services}"));
    dir
}

fn service(
    name: &str,
    cwd: &str,
    start: &str,
    stop: Option<&str>,
    ready_url: Option<&str>,
) -> String {
    let mut out =
        format!("[[services]]\nname = \"{name}\"\ncwd = \"{cwd}\"\nstart = \"{start}\"\n");
    if let Some(stop) = stop {
        out.push_str(&format!("stop = \"{stop}\"\n"));
    }
    if let Some(url) = ready_url {
        out.push_str(&format!("ready_url = \"{url}\"\n"));
    }
    out.push('\n');
    out
}

fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

#[test]
fn up_starts_services_in_order_and_down_stops_them_in_reverse() {
    let (api, web) = (free_port(), free_port());
    let services = service(
        "api",
        "api",
        &format!("node ../svc.js {api} api"),
        Some("node ../stop.js api"),
        Some(&format!("http://127.0.0.1:{api}/health")),
    ) + &service(
        "web",
        "web",
        &format!("node ../svc.js {web} web {api}"),
        Some("node ../stop.js web"),
        Some(&format!("http://127.0.0.1:{web}/")),
    );
    let dir = repo_with_services(web, &services);
    let root = dir.path();

    let output = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    let out = stdout(&output);
    assert_eq!(output.status.code(), Some(0), "{out}{}", stderr(&output));
    let line = |prefix: &str| {
        out.lines()
            .position(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no line starting with {prefix:?} in:\n{out}"))
    };
    let api_line = line(&format!(
        "up: api ready at http://127.0.0.1:{api}/health in "
    ));
    let web_line = line(&format!("up: web ready at http://127.0.0.1:{web}/ in "));
    let app_line = line(&format!("up: ready at http://127.0.0.1:{web}/healthz in "));
    assert!(api_line < web_line && web_line < app_line, "{out}");
    assert_eq!(
        fs::read_to_string(root.join("order.txt")).unwrap(),
        "api\nweb\n"
    );
    for file in ["api.pid", "api.identity", "api.log", "web.pid", "web.log"] {
        assert!(root.join(".guard/services").join(file).exists(), "{file}");
    }
    assert!(
        fs::read_to_string(root.join(".guard/services/api.log"))
            .unwrap()
            .contains("api listening")
    );
    assert!(!root.join(".guard/app.pid").exists(), "no [app].start ran");

    let down = guard(root).arg("down").output().unwrap();
    assert_eq!(down.status.code(), Some(0), "{}", stderr(&down));
    assert_eq!(stdout(&down), "down: stopped\n");
    assert_eq!(
        fs::read_to_string(root.join("stops.txt")).unwrap(),
        "web\napi\n"
    );
    assert!(!root.join(".guard/services/api.pid").exists());
    assert!(!root.join(".guard/services/web.identity").exists());
    wait_until(
        || !port_open(api) && !port_open(web),
        "both services to stop",
    );
}

#[test]
fn up_with_a_failing_service_stops_what_it_started_and_shows_its_log() {
    let (api, web) = (free_port(), free_port());
    let services = service(
        "api",
        "api",
        &format!("node ../svc.js {api} api"),
        None,
        Some(&format!("http://127.0.0.1:{api}/")),
    ) + &service(
        "web",
        "web",
        &format!("node ../svc.js {web} web fail"),
        None,
        Some(&format!("http://127.0.0.1:{web}/")),
    );
    let dir = repo_with_services(web, &services);
    let root = dir.path();
    let output = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    let err = stderr(&output);
    assert_eq!(output.status.code(), Some(3), "{err}");
    assert!(err.contains("service web"), "{err}");
    assert!(
        err.contains("web: failing on purpose"),
        "the log tail is shown: {err}"
    );
    assert!(
        !root.join(".guard/services/api.pid").exists(),
        "api was terminated"
    );
    assert!(!root.join(".guard/services/web.pid").exists());
    wait_until(|| !port_open(api), "the api this call started to stop");
}

#[test]
fn up_skips_a_service_that_is_already_ready_and_checks_cwd() {
    let web = free_port();
    let ready = serve_ok();
    let services = service(
        "api",
        "api",
        "exit 9",
        None,
        Some(&format!("{ready}/health")),
    ) + &service(
        "web",
        "web",
        &format!("node ../svc.js {web} web"),
        None,
        Some(&format!("http://127.0.0.1:{web}/")),
    );
    let dir = repo_with_services(web, &services);
    let root = dir.path();
    let output = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stdout(&output).contains("up: api already ready\n"),
        "{}",
        stdout(&output)
    );
    assert!(!root.join(".guard/services/api.pid").exists());
    let down = guard(root).arg("down").output().unwrap();
    assert_eq!(down.status.code(), Some(0), "{}", stderr(&down));
    wait_until(|| !port_open(web), "web to stop");

    let missing = repo_with_services(
        free_port(),
        &service("api", "nowhere", "exit 0", None, None),
    );
    let output = guard(missing.path())
        .args(["up", "--timeout", "5"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(stderr(&output).contains("nowhere"), "{}", stderr(&output));
}

#[test]
fn gate_shard_starts_services_and_stops_them_afterwards() {
    let web = free_port();
    let services = service(
        "web",
        "web",
        &format!("node ../svc.js {web} web"),
        Some("node ../stop.js web"),
        Some(&format!("http://127.0.0.1:{web}/")),
    );
    let dir = repo_with_services(web, &services);
    let root = dir.path();
    write(
        root,
        "plan/shard-1.json",
        "{\"files\": [\"**/tests/smoke/healthz.spec.ts\"]}",
    );
    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(fs::read_to_string(root.join("order.txt")).unwrap(), "web\n");
    assert_eq!(fs::read_to_string(root.join("stops.txt")).unwrap(), "web\n");
    assert!(!root.join(".guard/services/web.pid").exists());
    wait_until(|| !port_open(web), "web to stop");
}

/// `node ../brief.js <port> <name>`: like `svc.js`, but exits on its own
/// after 3 s, so a test can see a service go down without signalling it.
const BRIEF_JS: &str = r#"const http = require('http');
const fs = require('fs');
const [port, name] = process.argv.slice(2);
fs.appendFileSync('../order.txt', name + '\n');
http.createServer((q, s) => s.end('ok')).listen(Number(port), '127.0.0.1');
setTimeout(() => process.exit(0), 3000);
"#;

#[test]
fn up_does_not_respawn_a_recorded_service_that_is_still_running() {
    let (worker, web) = (free_port(), free_port());
    let services = service(
        "worker",
        "api",
        &format!("node ../svc.js {worker} worker"),
        None,
        None,
    ) + &service(
        "web",
        "web",
        &format!("node ../brief.js {web} web"),
        None,
        Some(&format!("http://127.0.0.1:{web}/")),
    );
    let dir = repo_with_services(web, &services);
    let root = dir.path();
    write(root, "brief.js", BRIEF_JS);

    let first = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    wait_until(|| port_open(worker), "the worker to listen");
    let worker_pid = fs::read_to_string(root.join(".guard/services/worker.pid")).unwrap();
    wait_until(|| !port_open(web), "the brief web service to exit");

    let second = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    let out = stdout(&second);
    assert_eq!(second.status.code(), Some(0), "{out}{}", stderr(&second));
    assert!(out.contains("up: worker already running\n"), "{out}");
    // The worker has no ready_url, so it and web start concurrently and the
    // first two lines may come in either order: count spawns, not order.
    let order = fs::read_to_string(root.join("order.txt")).unwrap();
    let spawns = |name: &str| order.lines().filter(|l| *l == name).count();
    assert_eq!(
        (spawns("worker"), spawns("web")),
        (1, 2),
        "the worker was not spawned a second time: {order:?}"
    );
    assert_eq!(
        fs::read_to_string(root.join(".guard/services/worker.pid")).unwrap(),
        worker_pid,
        "the recorded worker PID is kept"
    );

    let down = guard(root).arg("down").output().unwrap();
    assert_eq!(down.status.code(), Some(0), "{}", stderr(&down));
    wait_until(|| !port_open(worker), "the worker to stop");
}

#[test]
fn gate_shard_only_stops_what_it_started() {
    let (web, api) = (free_port(), free_port());
    let services = service(
        "web",
        "web",
        &format!("node ../svc.js {web} web"),
        Some("node ../stop.js web"),
        Some(&format!("http://127.0.0.1:{web}/")),
    ) + &service(
        "api",
        "api",
        &format!("node ../brief.js {api} api"),
        Some("node ../stop.js api"),
        Some(&format!("http://127.0.0.1:{api}/")),
    );
    let dir = repo_with_services(web, &services);
    let root = dir.path();
    write(root, "brief.js", BRIEF_JS);
    write(
        root,
        "plan/shard-1.json",
        "{\"files\": [\"**/tests/smoke/healthz.spec.ts\"]}",
    );

    let up = guard(root)
        .args(["up", "--timeout", "30"])
        .output()
        .unwrap();
    assert_eq!(up.status.code(), Some(0), "{}", stderr(&up));
    wait_until(|| !port_open(api), "the brief api service to exit");

    let output = guard(root)
        .args(["gate", "shard", "--index", "1", "--plan", "plan"])
        .env("GUARD_NPX", fake_npx(root))
        .env("FAKE_EXIT", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        fs::read_to_string(root.join("order.txt")).unwrap(),
        "web\napi\napi\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("stops.txt")).unwrap(),
        "api\n",
        "only the service the shard started is stopped"
    );
    assert!(port_open(web), "the developer's web service keeps running");
    assert!(root.join(".guard/services/web.pid").exists());

    let down = guard(root).arg("down").output().unwrap();
    assert_eq!(down.status.code(), Some(0), "{}", stderr(&down));
    wait_until(|| !port_open(web), "web to stop");
}

#[test]
fn doctor_fails_on_a_service_cwd_that_does_not_exist() {
    let dir = repo_with_services(
        free_port(),
        &(service("api", "api", "x", None, None)
            + &service("gone", "missing/dir", "x", None, None)),
    );
    let output = guard(dir.path()).arg("doctor").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let out = stdout(&output);
    assert!(
        out.contains("fail  services[1].cwd: directory missing/dir does not exist"),
        "{out}"
    );
    assert!(!out.contains("services[0].cwd"), "{out}");
}
