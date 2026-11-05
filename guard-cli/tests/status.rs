//! `guard status` end to end (cli.md).

use assert_cmd::Command;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
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
    for var in [
        "RUN_ID",
        "GUARD_NPX",
        "GITHUB_TOKEN",
        "GITLAB_TOKEN",
        "JIRA_API_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .replace("\r\n", "\n")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

/// A plain directory (not a git repo) with the fixture target.toml and catalog.
fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "target.toml",
        &fs::read_to_string(fixture("target.toml")).unwrap(),
    );
    write(
        root,
        "e2e-playwright/catalog/journeys.json",
        &fs::read_to_string(fixture("journeys.json")).unwrap(),
    );
    dir
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An HTTP server answering 200 to everything; returns its port.
fn serve_ok() -> u16 {
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
    port
}

/// target.toml with `[app]` on `app_port` and two services: `api` with a
/// ready URL on `api_port`, `worker` without one.
fn with_services(root: &Path, app_port: u16, api_port: u16) {
    let target = read(root, "target.toml").replace(
        "base_url = \"http://127.0.0.1:9\"",
        &format!(
            "start = \"docker compose up -d --wait db\"\nbase_url = \"http://127.0.0.1:{app_port}\"\nready_path = \"/\""
        ),
    );
    let target = target.replace(
        "[testability]",
        &format!(
            "[[services]]\nname = \"api\"\ncwd = \"api\"\nstart = \"bun run dev\"\nready_url = \"http://127.0.0.1:{api_port}/health\"\n\n\
             [[services]]\nname = \"worker\"\nstart = \"node worker.js\"\n\n[testability]"
        ),
    );
    write(root, "target.toml", &target);
}

fn status_json(root: &Path) -> serde_json::Value {
    let output = guard(root).args(["status", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    serde_json::from_str(&stdout(&output)).unwrap()
}

#[test]
fn status_with_nothing_running() {
    let dir = project();
    let root = dir.path();
    let port = free_port();
    with_services(root, port, free_port());
    let json = status_json(root);
    assert_eq!(
        json,
        serde_json::json!({
            "base_url": format!("http://127.0.0.1:{port}"),
            "ready": false,
            "entries": [
                {
                    "kind": "app", "name": "shop", "cwd": ".",
                    "start": "docker compose up -d --wait db",
                    "ready_url": format!("http://127.0.0.1:{port}/"),
                    "pid": null, "alive": null, "ready": false,
                    "log": ".guard/app.log"
                },
                {
                    "kind": "service", "name": "api", "cwd": "api",
                    "start": "bun run dev",
                    "ready_url": json["entries"][1]["ready_url"],
                    "pid": null, "alive": null, "ready": false,
                    "log": ".guard/services/api.log"
                },
                {
                    "kind": "service", "name": "worker", "cwd": ".",
                    "start": "node worker.js",
                    "ready_url": null,
                    "pid": null, "alive": null, "ready": null,
                    "log": ".guard/services/worker.log"
                }
            ]
        })
    );

    let text = guard(root).arg("status").output().unwrap();
    assert_eq!(text.status.code(), Some(0), "{}", stderr(&text));
    let text = stdout(&text);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 5, "{text}");
    let header: Vec<&str> = lines[0].split_whitespace().collect();
    assert_eq!(
        header,
        ["NAME", "CWD", "PID", "ALIVE", "READY", "URL", "LOG"]
    );
    let worker: Vec<&str> = lines[3].split_whitespace().collect();
    assert_eq!(
        worker,
        [
            "worker",
            ".",
            "-",
            "-",
            "-",
            "-",
            ".guard/services/worker.log"
        ]
    );
    let api: Vec<&str> = lines[2].split_whitespace().collect();
    assert_eq!(&api[..5], ["api", "api", "-", "-", "no"]);
    assert_eq!(
        lines[4],
        format!("base_url: http://127.0.0.1:{port} not ready")
    );
}

#[test]
fn status_with_a_running_server_and_a_stale_pid() {
    let dir = project();
    let root = dir.path();
    let app = serve_ok();
    let api = serve_ok();
    with_services(root, app, api);
    // A PID whose recorded identity can never match: reported, never signalled.
    write(
        root,
        ".guard/services/api.pid",
        &std::process::id().to_string(),
    );
    write(root, ".guard/services/api.identity", "not-the-start-time");
    let json = status_json(root);
    assert_eq!(json["ready"], true);
    assert_eq!(json["entries"][0]["ready"], true);
    assert_eq!(json["entries"][1]["ready"], true);
    assert_eq!(json["entries"][1]["pid"], std::process::id());
    assert_eq!(json["entries"][1]["alive"], false);
    assert_eq!(json["entries"][2]["ready"], serde_json::Value::Null);

    let text = stdout(&guard(root).arg("status").output().unwrap());
    assert!(
        text.ends_with(&format!("base_url: http://127.0.0.1:{app} ready\n")),
        "{text}"
    );
}

#[test]
fn status_with_an_invalid_target_is_a_usage_error() {
    let dir = project();
    let root = dir.path();
    write(root, "target.toml", "version = 1\n");
    let output = guard(root).arg("status").output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}
