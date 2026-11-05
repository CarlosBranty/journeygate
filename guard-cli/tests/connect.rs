//! `guard connect` and the user credentials file end to end (cli.md). The
//! validation calls go to a local wiremock server, never to the network.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CONFIG: &str = "version = 1\n\n[app]\nname = \"shop\"\nbase_url = \"http://localhost:3000\"\n\n[areas.shop]\ndescription = \"Shop\"\npaths = [\"src/**\"]\n";

/// Every variable the credentials file may set, plus the ones that move it.
const CREDENTIAL_ENV: &[&str] = &[
    "JIRA_BASE_URL",
    "JIRA_EMAIL",
    "JIRA_API_TOKEN",
    "GITHUB_TOKEN",
    "GITHUB_API_URL",
    "GITLAB_TOKEN",
    "GITLAB_API_URL",
    "RUN_ID",
];

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("repo")).unwrap();
        fs::write(dir.path().join("repo/target.toml"), CONFIG).unwrap();
        Self { dir }
    }

    fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// Outside the repository, as in real use.
    fn creds(&self) -> PathBuf {
        self.dir
            .path()
            .join("home")
            .join("guard")
            .join("credentials.toml")
    }

    fn guard(&self) -> Command {
        let mut cmd = Command::cargo_bin("guard").unwrap();
        cmd.current_dir(self.repo())
            .env("NO_COLOR", "1")
            .env("GUARD_CREDENTIALS_FILE", self.creds());
        for var in CREDENTIAL_ENV {
            cmd.env_remove(var);
        }
        cmd
    }

    fn write_creds(&self, body: &str) {
        fs::create_dir_all(self.creds().parent().unwrap()).unwrap();
        fs::write(self.creds(), body).unwrap();
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn list_shows_sources_with_the_environment_winning_and_secrets_masked() {
    let fx = Fixture::new();
    fx.write_creds(
        "JIRA_EMAIL = \"ada@acme.test\"\nGITHUB_TOKEN = \"ghp_fromfile_aaaa\"\nPATH = \"/evil\"\n",
    );
    let output = fx
        .guard()
        .args(["connect", "--list"])
        .env("GITHUB_TOKEN", "ghp_fromenv_b7c1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    let line = |key: &str| {
        stdout
            .lines()
            .find(|l| l.starts_with(&format!("{key} ")))
            .unwrap_or_else(|| panic!("{key} missing:\n{stdout}"))
            .to_string()
    };
    assert!(line("JIRA_EMAIL").contains("ada@acme.test"));
    assert!(line("JIRA_EMAIL").ends_with("credentials file"));
    assert!(line("GITHUB_TOKEN").contains("****b7c1"));
    assert!(line("GITHUB_TOKEN").ends_with("environment"));
    assert!(line("GITLAB_TOKEN").ends_with("not set"));
    assert!(!stdout.contains("ghp_"), "{stdout}");
    assert!(
        stderr.contains("warning: credentials file: unknown key PATH"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("/evil") && !stderr.contains("ghp_"),
        "{stderr}"
    );
}

#[test]
fn startup_exports_file_values_to_child_processes_only_when_unset() {
    // `guard connect --list` runs in the same process; the export to child
    // processes is the same environment, so `--list` proves the precedence.
    let fx = Fixture::new();
    fx.write_creds("JIRA_BASE_URL = \"https://file.atlassian.net\"\n");
    let output = fx
        .guard()
        .args(["connect", "--list"])
        .env("JIRA_BASE_URL", "https://env.atlassian.net")
        .output()
        .unwrap();
    let stdout = text(&output.stdout);
    assert!(stdout.contains("https://env.atlassian.net"), "{stdout}");
    assert!(!stdout.contains("file.atlassian.net"), "{stdout}");
}

#[test]
fn invalid_credentials_file_is_a_warning_without_its_content() {
    let fx = Fixture::new();
    fx.write_creds("GITHUB_TOKEN = ghp_unquoted_s3cr3t\n");
    let output = fx.guard().args(["connect", "--list"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("warning: credentials file: invalid TOML at line 1"),
        "{stderr}"
    );
    assert!(!stderr.contains("s3cr3t"), "{stderr}");
}

#[test]
fn connect_without_a_service_is_a_usage_error() {
    let fx = Fixture::new();
    let output = fx.guard().arg("connect").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn non_interactive_without_the_token_names_the_variable() {
    let fx = Fixture::new();
    let output = fx
        .guard()
        .args(["connect", "github"])
        .write_stdin("")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        text(&output.stderr).contains("error: missing GITHUB_TOKEN"),
        "{}",
        text(&output.stderr)
    );
    assert!(!fx.creds().exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jira_end_to_end_against_a_local_server() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/rest/api/3/myself"))
        // base64("ada@acme.test:atlassian-token-9f3a")
        .and(header(
            "authorization",
            "Basic YWRhQGFjbWUudGVzdDphdGxhc3NpYW4tdG9rZW4tOWYzYQ==",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "accountId": "a1",
            "displayName": "Ada Lovelace",
            "emailAddress": "ada@acme.test"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let fx = Fixture::new();
    let uri = server.uri();
    let output = fx
        .guard()
        .args([
            "connect",
            "jira",
            "--base-url",
            &uri,
            "--email",
            "ada@acme.test",
            "--project",
            "SHOP",
            "--token-stdin",
        ])
        .write_stdin("atlassian-token-9f3a\n")
        .output()
        .unwrap();
    let stdout = text(&output.stdout);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stdout}{stderr}");
    assert!(
        stdout.starts_with("jira: authenticated as Ada Lovelace (ada@acme.test)\n"),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("updated target.toml [tracker] kind = \"jira\", project = \"SHOP\"\n"),
        "{stdout}"
    );
    let config = read(&fx.repo().join("target.toml"));
    assert!(config.starts_with(CONFIG), "{config}");
    assert!(config.contains("[tracker]\nkind = \"jira\"\nproject = \"SHOP\"\n"));
    let creds = read(&fx.creds());
    assert!(
        creds.contains("JIRA_API_TOKEN = \"atlassian-token-9f3a\""),
        "{creds}"
    );
    for shown in [&stdout, &stderr, &config] {
        assert!(!shown.contains("atlassian-token"), "{shown}");
    }
    // Nothing secret inside the repository.
    for entry in fs::read_dir(fx.repo()).unwrap() {
        let body = fs::read(entry.unwrap().path()).unwrap_or_default();
        assert!(!String::from_utf8_lossy(&body).contains("atlassian-token"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_github_token_is_exit_2_and_stores_nothing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let fx = Fixture::new();
    let uri = server.uri();
    let output = fx
        .guard()
        .args(["connect", "github", "--api-url", &uri])
        .env("GITHUB_TOKEN", "ghp_rejected_1234")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("error: github rejected the credentials (401)"),
        "{stderr}"
    );
    assert!(!stderr.contains("ghp_rejected"), "{stderr}");
    assert!(!fx.creds().exists());
    assert_eq!(read(&fx.repo().join("target.toml")), CONFIG);
}
