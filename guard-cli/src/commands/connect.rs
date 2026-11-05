//! `guard connect jira|github|gitlab` and `guard connect --list` (cli.md).
//!
//! Input and the validation call go through two ports ([`Input`],
//! [`Verifier`]) so tests use fakes and never touch the network. Secrets only
//! ever reach the verifier and the user credentials file: they are never
//! printed, written to `target.toml` or quoted in an error.

use crate::cli::{ConnectArgs, ConnectHostArgs, ConnectService};
use crate::context::Context;
use crate::credentials;
use crate::error::{CliError, CliResult, EXIT_OK};
use guard_core::config::parse_target;
use guard_core::config_edit::set_table_keys;
use guard_core::credentials::{list_rows, render_list};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

pub const GITHUB_DEFAULT_API_URL: &str = "https://api.github.com";
pub const GITLAB_DEFAULT_API_URL: &str = "https://gitlab.com/api/v4";

/// Where the non-secret fields and the secret come from.
pub trait Input {
    /// stdin is a terminal: missing fields are prompted.
    fn interactive(&self) -> bool;
    /// Prints `prompt` and reads one line (empty at end of input).
    fn line(&mut self, prompt: &str) -> io::Result<String>;
    /// Prints `prompt` and reads one line without echo.
    fn secret(&mut self, prompt: &str) -> io::Result<String>;
    /// The first line of stdin (`--token-stdin`).
    fn stdin_line(&mut self) -> io::Result<String>;
}

/// What the validation call checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verify {
    Jira {
        base_url: String,
        email: String,
        token: String,
    },
    Github {
        api_url: String,
        token: String,
    },
    Gitlab {
        api_url: String,
        token: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// 401 / 403.
    Rejected(u16),
    /// Another 4xx (a wrong URL, usually).
    Http(u16),
    /// Connection failure, timeout, 429, 5xx or an unexpected body. The
    /// message never contains the secret.
    Unavailable(String),
}

/// The validation call (`GET /rest/api/3/myself`, `GET /user`).
pub trait Verifier {
    /// The identity the credentials belong to, as printed after
    /// `authenticated as`.
    fn verify(&self, request: &Verify) -> Result<String, VerifyError>;
}

pub fn run(ctx: &Context, args: &ConnectArgs) -> CliResult<u8> {
    let env = |key: &str| std::env::var(key).ok();
    let stdout = io::stdout();
    if args.list {
        list(&env, &credentials::exported_from_file, &mut stdout.lock())?;
        return Ok(EXIT_OK);
    }
    let Some(service) = &args.service else {
        return Err(
            CliError::usage("guard connect needs jira, github, gitlab or --list")
                .with_hint("guard connect jira | guard connect github | guard connect gitlab"),
        );
    };
    let creds_path = credentials::path().ok_or_else(|| {
        CliError::env("cannot locate the user credentials file")
            .with_hint("set HOME (or APPDATA on Windows), or GUARD_CREDENTIALS_FILE")
    })?;
    let mut input = Terminal::new();
    connect(
        service,
        &Paths {
            config: &ctx.config_path,
            config_shown: &ctx.display(&ctx.config_path),
            credentials: &creds_path,
        },
        &env,
        &mut input,
        &Network,
        &mut stdout.lock(),
    )
}

pub fn list(
    env: &dyn Fn(&str) -> Option<String>,
    from_file: &dyn Fn(&str) -> bool,
    out: &mut dyn Write,
) -> CliResult<()> {
    let rows = list_rows(env, from_file);
    out.write_all(render_list(&rows).as_bytes())
        .map_err(|e| CliError::env(format!("cannot write to stdout: {e}")))
}

pub struct Paths<'a> {
    pub config: &'a Path,
    pub config_shown: &'a str,
    pub credentials: &'a Path,
}

/// A connection request with every field resolved.
struct Resolved {
    service: &'static str,
    verify: Verify,
    /// Keys to store (`None` = remove from the file).
    store: Vec<(&'static str, Option<String>)>,
    table: &'static str,
    table_keys: Vec<(&'static str, String)>,
}

pub fn connect(
    service: &ConnectService,
    paths: &Paths,
    env: &dyn Fn(&str) -> Option<String>,
    input: &mut dyn Input,
    verifier: &dyn Verifier,
    out: &mut dyn Write,
) -> CliResult<u8> {
    // target.toml first: nothing is asked when it cannot be updated.
    let config_text = fs::read_to_string(paths.config).map_err(|_| {
        CliError::usage(format!("cannot read {}", paths.config_shown)).with_hint("run guard init")
    })?;
    invalid_config(paths.config_shown, parse_target(&config_text).err())?;

    let resolved = resolve(service, env, input)?;

    let identity = verifier
        .verify(&resolved.verify)
        .map_err(|e| verify_error(resolved.service, e))?;

    let keys: Vec<(&str, &str)> = resolved
        .table_keys
        .iter()
        .map(|(k, v)| (*k, v.as_str()))
        .collect();
    let new_config = set_table_keys(&config_text, resolved.table, &keys).map_err(|message| {
        CliError::usage(format!("cannot update {}: {message}", paths.config_shown))
    })?;
    invalid_config(paths.config_shown, parse_target(&new_config).err())?;

    let mut stored = credentials::read_for_update(paths.credentials).map_err(|message| {
        CliError::env(format!(
            "cannot update {}: {message}",
            paths.credentials.display()
        ))
        .with_hint("fix or delete the file, then run guard connect again")
    })?;
    let mut saved = Vec::new();
    for (key, value) in &resolved.store {
        match value {
            Some(value) => {
                stored.insert(key.to_string(), value.clone());
                saved.push(*key);
            }
            None => {
                stored.remove(*key);
            }
        }
    }
    credentials::write(paths.credentials, &stored).map_err(|e| {
        CliError::env(format!(
            "cannot write {}: {}",
            paths.credentials.display(),
            e.kind()
        ))
    })?;
    fs::write(paths.config, &new_config)
        .map_err(|e| CliError::env(format!("cannot write {}: {}", paths.config_shown, e.kind())))?;

    let shown_keys = resolved
        .table_keys
        .iter()
        .map(|(k, v)| format!("{k} = {}", toml_string(v)))
        .collect::<Vec<_>>()
        .join(", ");
    let report = format!(
        "{}: authenticated as {identity}\nsaved {} to {}\nupdated {} [{}] {shown_keys}\n",
        resolved.service,
        saved.join(", "),
        paths.credentials.display(),
        paths.config_shown,
        resolved.table,
    );
    out.write_all(report.as_bytes())
        .map_err(|e| CliError::env(format!("cannot write to stdout: {e}")))?;
    Ok(EXIT_OK)
}

fn invalid_config(shown: &str, error: Option<guard_core::config::ConfigError>) -> CliResult<()> {
    use guard_core::config::ConfigError;
    match error {
        None => Ok(()),
        Some(error) => {
            let messages = match error {
                ConfigError::Parse(message) => vec![message],
                ConfigError::Invalid(errors) => errors.iter().map(ToString::to_string).collect(),
            };
            Err(CliError::usage(format!("invalid {shown}")).with_messages(messages))
        }
    }
}

/// `"value"` as shown in the `updated target.toml` line (the table values
/// are plain identifiers: `jira`, `github`, a project key).
fn toml_string(value: &str) -> String {
    format!("\"{value}\"")
}

fn resolve(
    service: &ConnectService,
    env: &dyn Fn(&str) -> Option<String>,
    input: &mut dyn Input,
) -> CliResult<Resolved> {
    match service {
        ConnectService::Jira {
            base_url,
            email,
            project,
            token_stdin,
        } => {
            let base_url = field(
                input,
                env,
                Field {
                    flag: base_url.as_deref(),
                    flag_name: "--base-url",
                    env_key: "JIRA_BASE_URL",
                    label: "Jira base URL",
                    default: None,
                },
            )?;
            check_url("--base-url", &base_url)?;
            let base_url = base_url.trim_end_matches('/').to_string();
            let email = field(
                input,
                env,
                Field {
                    flag: email.as_deref(),
                    flag_name: "--email",
                    env_key: "JIRA_EMAIL",
                    label: "Jira email",
                    default: None,
                },
            )?;
            if !email.contains('@') || email.chars().any(char::is_whitespace) {
                return Err(CliError::usage("--email must be an email address"));
            }
            if let Some(project) = project {
                let valid = project.starts_with(|c: char| c.is_ascii_uppercase())
                    && project
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                if !valid {
                    return Err(CliError::usage(format!(
                        "--project {project:?} must match ^[A-Z][A-Z0-9_]*$"
                    )));
                }
            }
            let token = secret(input, env, "JIRA_API_TOKEN", "Jira API token", *token_stdin)?;
            let mut table_keys = vec![("kind", "jira".to_string())];
            if let Some(project) = project {
                table_keys.push(("project", project.clone()));
            }
            Ok(Resolved {
                service: "jira",
                verify: Verify::Jira {
                    base_url: base_url.clone(),
                    email: email.clone(),
                    token: token.clone(),
                },
                store: vec![
                    ("JIRA_BASE_URL", Some(base_url)),
                    ("JIRA_EMAIL", Some(email)),
                    ("JIRA_API_TOKEN", Some(token)),
                ],
                table: "tracker",
                table_keys,
            })
        }
        ConnectService::Github(args) => host(args, env, input, Host::Github),
        ConnectService::Gitlab(args) => host(args, env, input, Host::Gitlab),
    }
}

#[derive(Clone, Copy)]
enum Host {
    Github,
    Gitlab,
}

fn host(
    args: &ConnectHostArgs,
    env: &dyn Fn(&str) -> Option<String>,
    input: &mut dyn Input,
    host: Host,
) -> CliResult<Resolved> {
    let (service, url_key, token_key, default, label) = match host {
        Host::Github => (
            "github",
            "GITHUB_API_URL",
            "GITHUB_TOKEN",
            GITHUB_DEFAULT_API_URL,
            "GitHub",
        ),
        Host::Gitlab => (
            "gitlab",
            "GITLAB_API_URL",
            "GITLAB_TOKEN",
            GITLAB_DEFAULT_API_URL,
            "GitLab",
        ),
    };
    let api_url = field(
        input,
        env,
        Field {
            flag: args.api_url.as_deref(),
            flag_name: "--api-url",
            env_key: url_key,
            label: &format!("{label} API URL"),
            default: Some(default),
        },
    )?;
    check_url("--api-url", &api_url)?;
    let api_url = api_url.trim_end_matches('/').to_string();
    let token = secret(
        input,
        env,
        token_key,
        &format!("{label} token"),
        args.token_stdin,
    )?;
    let stored_url = (api_url != default).then(|| api_url.clone());
    let verify = match host {
        Host::Github => Verify::Github {
            api_url,
            token: token.clone(),
        },
        Host::Gitlab => Verify::Gitlab {
            api_url,
            token: token.clone(),
        },
    };
    Ok(Resolved {
        service,
        verify,
        store: vec![(token_key, Some(token)), (url_key, stored_url)],
        table: "code_host",
        table_keys: vec![("kind", service.to_string())],
    })
}

struct Field<'a> {
    flag: Option<&'a str>,
    flag_name: &'a str,
    env_key: &'a str,
    label: &'a str,
    default: Option<&'a str>,
}

/// Flag, else (prompted with the current value as default when
/// interactive) the environment, else the default.
fn field(
    input: &mut dyn Input,
    env: &dyn Fn(&str) -> Option<String>,
    f: Field,
) -> CliResult<String> {
    if let Some(value) = f.flag.map(str::trim).filter(|v| !v.is_empty()) {
        return Ok(value.to_string());
    }
    let current = env(f.env_key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| f.default.map(str::to_string));
    if input.interactive() {
        let prompt = match &current {
            Some(current) => format!("{} [{current}]: ", f.label),
            None => format!("{}: ", f.label),
        };
        let typed = input.line(&prompt).map_err(read_error)?;
        let typed = typed.trim();
        if !typed.is_empty() {
            return Ok(typed.to_string());
        }
    }
    current.ok_or_else(|| CliError::usage(format!("missing {} (or {})", f.flag_name, f.env_key)))
}

/// `--token-stdin`, else a hidden prompt when interactive (Enter keeps the
/// current value), else the environment.
fn secret(
    input: &mut dyn Input,
    env: &dyn Fn(&str) -> Option<String>,
    key: &str,
    label: &str,
    token_stdin: bool,
) -> CliResult<String> {
    let missing = || {
        CliError::usage(format!("missing {key}"))
            .with_hint(format!("set {key} or pass --token-stdin"))
    };
    if token_stdin {
        let line = input.stdin_line().map_err(read_error)?;
        let line = line.trim();
        return if line.is_empty() {
            Err(missing())
        } else {
            Ok(line.to_string())
        };
    }
    let current = env(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    if input.interactive() {
        let prompt = if current.is_some() {
            format!("{label} [keep current]: ")
        } else {
            format!("{label}: ")
        };
        let typed = input.secret(&prompt).map_err(read_error)?;
        let typed = typed.trim();
        if !typed.is_empty() {
            return Ok(typed.to_string());
        }
    }
    current.ok_or_else(missing)
}

fn read_error(e: io::Error) -> CliError {
    CliError::env(format!("cannot read input: {}", e.kind()))
}

pub(crate) fn check_url(flag: &str, url: &str) -> CliResult<()> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    let authority = rest.map(|r| r.split('/').next().unwrap_or(""));
    match authority {
        Some(a) if !a.is_empty() && !url.chars().any(char::is_whitespace) => {
            if a.contains('@') {
                Err(CliError::usage(format!(
                    "{flag} must not contain credentials"
                )))
            } else if url.starts_with("http://") && !is_loopback(a) {
                // The token would cross the network in cleartext.
                Err(CliError::usage(format!(
                    "{flag} must use https (plain http is only accepted for localhost), got {url:?}"
                )))
            } else {
                Ok(())
            }
        }
        _ => Err(CliError::usage(format!(
            "{flag} must be an http(s) URL, got {url:?}"
        ))),
    }
}

/// `localhost`, `127.x.x.x` or `[::1]`, with or without a port.
fn is_loopback(authority: &str) -> bool {
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn verify_error(service: &str, error: VerifyError) -> CliError {
    match error {
        VerifyError::Rejected(status) => {
            CliError::usage(format!("{service} rejected the credentials ({status})"))
        }
        VerifyError::Http(status) => {
            CliError::usage(format!("{service} answered HTTP {status}")).with_hint("check the URL")
        }
        VerifyError::Unavailable(message) => {
            CliError::env(format!("cannot reach {service}: {message}"))
        }
    }
}

/// stdin / stdout of the process; the secret prompt has no echo (rpassword).
struct Terminal {
    interactive: bool,
}

impl Terminal {
    fn new() -> Self {
        Self {
            interactive: io::stdin().is_terminal(),
        }
    }
}

impl Input for Terminal {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn line(&mut self, prompt: &str) -> io::Result<String> {
        let mut out = io::stdout();
        write!(out, "{prompt}")?;
        out.flush()?;
        self.stdin_line()
    }

    fn secret(&mut self, prompt: &str) -> io::Result<String> {
        rpassword::prompt_password(prompt)
    }

    fn stdin_line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line)?;
        Ok(line)
    }
}

/// The real validation calls through the guard-tracker / guard-codehost
/// adapters.
struct Network;

impl Verifier for Network {
    fn verify(&self, request: &Verify) -> Result<String, VerifyError> {
        let (result, token) = crate::runtime::block_on(async {
            match request {
                Verify::Jira {
                    base_url,
                    email,
                    token,
                } => {
                    let config = guard_tracker::JiraConfig::for_credentials(base_url, email, token);
                    let result = match guard_tracker::JiraTracker::new(config) {
                        Ok(tracker) => tracker.myself().await.map_err(tracker_error),
                        Err(e) => Err(tracker_error(e)),
                    };
                    let shown = result.map(|account| match account.email {
                        Some(email) => format!("{} ({email})", account.display_name),
                        None => account.display_name,
                    });
                    (shown, token)
                }
                Verify::Github { api_url, token } => (
                    guard_codehost::whoami::github_user(api_url, token)
                        .await
                        .map_err(codehost_error),
                    token,
                ),
                Verify::Gitlab { api_url, token } => (
                    guard_codehost::whoami::gitlab_user(api_url, token)
                        .await
                        .map_err(codehost_error),
                    token,
                ),
            }
        });
        // Belt and braces: an adapter message never carries the token.
        result.map_err(|e| match e {
            VerifyError::Unavailable(m) => VerifyError::Unavailable(redact(&m, token)),
            other => other,
        })
    }
}

fn redact(message: &str, secret: &str) -> String {
    if secret.is_empty() {
        message.to_string()
    } else {
        message.replace(secret, "****")
    }
}

fn tracker_error(error: guard_core::ports::TrackerError) -> VerifyError {
    use guard_core::ports::TrackerError as E;
    match error {
        E::Unauthorized(status) => VerifyError::Rejected(status),
        E::NotFound(_) => VerifyError::Http(404),
        E::Rejected { status, .. } if (400..500).contains(&status) => VerifyError::Http(status),
        E::Rejected { status: 0, body } => VerifyError::Unavailable(body),
        E::Rejected { status, .. } => VerifyError::Unavailable(format!("HTTP {status}")),
        E::RateLimited => VerifyError::Unavailable("rate limited (HTTP 429)".into()),
        E::Transport(message) | E::Unconfigured(message) => VerifyError::Unavailable(message),
    }
}

fn codehost_error(error: guard_core::ports::CodeHostError) -> VerifyError {
    use guard_core::ports::CodeHostError as E;
    match error {
        E::Unauthorized(status) => VerifyError::Rejected(status),
        E::NotFound(_) => VerifyError::Http(404),
        E::Rejected { status, .. } if (400..500).contains(&status) => VerifyError::Http(status),
        E::Rejected { status, body } if (200..300).contains(&status) => {
            VerifyError::Unavailable(body)
        }
        E::Rejected { status, .. } => VerifyError::Unavailable(format!("HTTP {status}")),
        E::RateLimited => VerifyError::Unavailable("rate limited (HTTP 429)".into()),
        E::Transport(message) | E::Unconfigured(message) => VerifyError::Unavailable(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    #[test]
    fn plain_http_is_only_accepted_for_loopback_hosts() {
        for ok in [
            "https://acme.atlassian.net",
            "https://gitlab.corp/api/v4",
            "http://localhost:8080",
            "http://127.0.0.1:9000/api/v4",
            "http://[::1]:3000",
        ] {
            assert!(check_url("--api-url", ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://gitlab.corp/api/v4",
            "http://acme.atlassian.net",
            "http://localhost.evil.example",
        ] {
            let err = check_url("--api-url", bad).unwrap_err();
            assert!(
                err.messages[0].contains("https"),
                "{bad}: {:?}",
                err.messages
            );
        }
    }

    const CONFIG: &str = "# shop\nversion = 1\n\n[app]\nname = \"shop\"\nbase_url = \"http://localhost:3000\"\n\n[areas.shop]\ndescription = \"Shop\"\npaths = [\"src/**\"]\n";

    struct FakeInput {
        interactive: bool,
        lines: VecDeque<String>,
        prompts: Vec<String>,
    }

    impl FakeInput {
        fn piped(lines: &[&str]) -> Self {
            Self {
                interactive: false,
                lines: lines.iter().map(|l| l.to_string()).collect(),
                prompts: Vec::new(),
            }
        }

        fn tty(lines: &[&str]) -> Self {
            Self {
                interactive: true,
                ..Self::piped(lines)
            }
        }

        fn next(&mut self) -> String {
            self.lines.pop_front().unwrap_or_default()
        }
    }

    impl Input for FakeInput {
        fn interactive(&self) -> bool {
            self.interactive
        }
        fn line(&mut self, prompt: &str) -> io::Result<String> {
            self.prompts.push(prompt.to_string());
            Ok(self.next())
        }
        fn secret(&mut self, prompt: &str) -> io::Result<String> {
            self.prompts.push(format!("(hidden) {prompt}"));
            Ok(self.next())
        }
        fn stdin_line(&mut self) -> io::Result<String> {
            Ok(self.next())
        }
    }

    struct FakeVerifier {
        answer: Result<String, VerifyError>,
        seen: RefCell<Vec<Verify>>,
    }

    impl FakeVerifier {
        fn ok(identity: &str) -> Self {
            Self {
                answer: Ok(identity.to_string()),
                seen: RefCell::new(Vec::new()),
            }
        }
        fn err(error: VerifyError) -> Self {
            Self {
                answer: Err(error),
                seen: RefCell::new(Vec::new()),
            }
        }
    }

    impl Verifier for FakeVerifier {
        fn verify(&self, request: &Verify) -> Result<String, VerifyError> {
            self.seen.borrow_mut().push(request.clone());
            self.answer.clone()
        }
    }

    struct Repo {
        dir: tempfile::TempDir,
    }

    impl Repo {
        fn new(config: Option<&str>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            if let Some(config) = config {
                fs::write(dir.path().join("target.toml"), config).unwrap();
            }
            Self { dir }
        }
        fn config(&self) -> std::path::PathBuf {
            self.dir.path().join("target.toml")
        }
        fn creds(&self) -> std::path::PathBuf {
            self.dir.path().join("user").join("credentials.toml")
        }
        fn run(
            &self,
            service: ConnectService,
            env: &[(&str, &str)],
            input: &mut FakeInput,
            verifier: &FakeVerifier,
        ) -> (CliResult<u8>, String) {
            let env: Vec<(String, String)> = env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            let lookup =
                move |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
            let config = self.config();
            let creds = self.creds();
            let mut out = Vec::new();
            let result = connect(
                &service,
                &Paths {
                    config: &config,
                    config_shown: "target.toml",
                    credentials: &creds,
                },
                &lookup,
                input,
                verifier,
                &mut out,
            );
            (result, String::from_utf8(out).unwrap())
        }
        fn read(&self, path: &Path) -> String {
            fs::read_to_string(path).unwrap_or_default()
        }
    }

    fn jira(
        base: Option<&str>,
        email: Option<&str>,
        project: Option<&str>,
        stdin: bool,
    ) -> ConnectService {
        ConnectService::Jira {
            base_url: base.map(str::to_string),
            email: email.map(str::to_string),
            project: project.map(str::to_string),
            token_stdin: stdin,
        }
    }

    fn github(api: Option<&str>, stdin: bool) -> ConnectService {
        ConnectService::Github(ConnectHostArgs {
            api_url: api.map(str::to_string),
            token_stdin: stdin,
        })
    }

    #[test]
    fn jira_from_flags_and_stdin_validates_stores_and_updates_target_toml() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::ok("Ada Lovelace (ada@acme.test)");
        let mut input = FakeInput::piped(&["atlassian-token-9f3a\n"]);
        let (result, out) = repo.run(
            jira(
                Some("https://acme.atlassian.net/"),
                Some("ada@acme.test"),
                Some("SHOP"),
                true,
            ),
            &[],
            &mut input,
            &verifier,
        );
        assert_eq!(result.unwrap(), 0);
        assert_eq!(
            verifier.seen.borrow().as_slice(),
            &[Verify::Jira {
                base_url: "https://acme.atlassian.net".into(),
                email: "ada@acme.test".into(),
                token: "atlassian-token-9f3a".into(),
            }]
        );
        let creds_shown = repo.creds().display().to_string();
        assert_eq!(
            out,
            format!(
                "jira: authenticated as Ada Lovelace (ada@acme.test)\nsaved JIRA_BASE_URL, JIRA_EMAIL, JIRA_API_TOKEN to {creds_shown}\nupdated target.toml [tracker] kind = \"jira\", project = \"SHOP\"\n"
            )
        );
        let creds = repo.read(&repo.creds());
        assert!(
            creds.contains("JIRA_API_TOKEN = \"atlassian-token-9f3a\""),
            "{creds}"
        );
        let config = repo.read(&repo.config());
        assert!(config.starts_with(CONFIG), "{config}");
        assert!(
            config.ends_with("\n[tracker]\nkind = \"jira\"\nproject = \"SHOP\"\n"),
            "{config}"
        );
        assert!(!config.contains("atlassian-token"), "{config}");
        assert!(!out.contains("atlassian-token"), "{out}");
    }

    #[test]
    fn non_interactive_without_input_names_the_missing_flag_and_writes_nothing() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::ok("x");
        let (result, _) = repo.run(
            jira(None, None, None, false),
            &[],
            &mut FakeInput::piped(&[]),
            &verifier,
        );
        let error = result.unwrap_err();
        assert_eq!(error.code, 2);
        assert_eq!(
            error.messages,
            vec!["missing --base-url (or JIRA_BASE_URL)"]
        );
        // The secret: env var or --token-stdin.
        let (result, _) = repo.run(
            jira(Some("https://a.atlassian.net"), Some("a@b.c"), None, false),
            &[],
            &mut FakeInput::piped(&[]),
            &verifier,
        );
        let error = result.unwrap_err();
        assert_eq!(error.messages, vec!["missing JIRA_API_TOKEN"]);
        assert!(verifier.seen.borrow().is_empty());
        assert!(!repo.creds().exists());
        assert_eq!(repo.read(&repo.config()), CONFIG);
    }

    #[test]
    fn non_interactive_reads_fields_and_the_token_from_the_environment() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::ok("ada");
        let (result, out) = repo.run(
            github(None, false),
            &[("GITHUB_TOKEN", "ghp_from_env_b7c1")],
            &mut FakeInput::piped(&[]),
            &verifier,
        );
        assert_eq!(result.unwrap(), 0);
        assert_eq!(
            verifier.seen.borrow().as_slice(),
            &[Verify::Github {
                api_url: GITHUB_DEFAULT_API_URL.into(),
                token: "ghp_from_env_b7c1".into(),
            }]
        );
        assert!(
            out.starts_with("github: authenticated as ada\nsaved GITHUB_TOKEN to "),
            "{out}"
        );
        assert!(
            out.ends_with("updated target.toml [code_host] kind = \"github\"\n"),
            "{out}"
        );
        // The default API URL is not stored.
        assert!(!repo.read(&repo.creds()).contains("GITHUB_API_URL"));
    }

    #[test]
    fn interactive_prompts_with_defaults_and_hides_the_secret() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::ok("ada");
        // Enter accepts the GitLab default URL; then the hidden token.
        let mut input = FakeInput::tty(&["\n", "glpat-typed-1234\n"]);
        let (result, _) = repo.run(
            ConnectService::Gitlab(ConnectHostArgs {
                api_url: None,
                token_stdin: false,
            }),
            &[],
            &mut input,
            &verifier,
        );
        assert_eq!(result.unwrap(), 0);
        assert_eq!(
            input.prompts,
            vec![
                "GitLab API URL [https://gitlab.com/api/v4]: ".to_string(),
                "(hidden) GitLab token: ".to_string(),
            ]
        );
        assert_eq!(
            verifier.seen.borrow()[0],
            Verify::Gitlab {
                api_url: GITLAB_DEFAULT_API_URL.into(),
                token: "glpat-typed-1234".into(),
            }
        );
        let config = repo.read(&repo.config());
        assert!(
            config.contains("[code_host]\nkind = \"gitlab\"\n"),
            "{config}"
        );
    }

    #[test]
    fn interactive_typed_text_replaces_the_current_value() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::ok("ada");
        let mut input = FakeInput::tty(&["https://ghe.acme.test/api/v3/\n", "\n"]);
        let (result, out) = repo.run(
            github(None, false),
            &[("GITHUB_TOKEN", "ghp_current")],
            &mut input,
            &verifier,
        );
        assert_eq!(result.unwrap(), 0);
        assert_eq!(input.prompts[1], "(hidden) GitHub token [keep current]: ");
        assert_eq!(
            verifier.seen.borrow()[0],
            Verify::Github {
                api_url: "https://ghe.acme.test/api/v3".into(),
                token: "ghp_current".into(),
            }
        );
        assert!(
            out.contains("saved GITHUB_TOKEN, GITHUB_API_URL to "),
            "{out}"
        );
        assert!(
            repo.read(&repo.creds())
                .contains("GITHUB_API_URL = \"https://ghe.acme.test/api/v3\"")
        );
    }

    #[test]
    fn rejected_credentials_are_exit_2_and_nothing_is_written() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::err(VerifyError::Rejected(401));
        let (result, out) = repo.run(
            github(None, true),
            &[],
            &mut FakeInput::piped(&["ghp_bad_token\n"]),
            &verifier,
        );
        let error = result.unwrap_err();
        assert_eq!(error.code, 2);
        assert_eq!(
            error.messages,
            vec!["github rejected the credentials (401)"]
        );
        assert!(out.is_empty());
        assert!(!repo.creds().exists());
        assert_eq!(repo.read(&repo.config()), CONFIG);
    }

    #[test]
    fn an_unreachable_service_is_exit_3() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::err(VerifyError::Unavailable("connection refused".into()));
        let (result, _) = repo.run(
            github(None, true),
            &[],
            &mut FakeInput::piped(&["t\n"]),
            &verifier,
        );
        let error = result.unwrap_err();
        assert_eq!(error.code, 3);
        assert_eq!(
            error.messages,
            vec!["cannot reach github: connection refused"]
        );
        assert!(!repo.creds().exists());
    }

    #[test]
    fn a_missing_target_toml_is_exit_2_before_any_prompt() {
        let repo = Repo::new(None);
        let verifier = FakeVerifier::ok("x");
        let mut input = FakeInput::tty(&[]);
        let (result, _) = repo.run(github(None, false), &[], &mut input, &verifier);
        let error = result.unwrap_err();
        assert_eq!(error.code, 2);
        assert_eq!(error.hints, vec!["run guard init"]);
        assert!(input.prompts.is_empty());
    }

    #[test]
    fn existing_tables_keep_their_other_keys_and_other_credentials_survive() {
        let config =
            format!("{CONFIG}\n[tracker]\nkind = \"jira\"\nticket_pattern = \"SHOP-\\\\d+\"\n");
        let repo = Repo::new(Some(&config));
        fs::create_dir_all(repo.creds().parent().unwrap()).unwrap();
        fs::write(repo.creds(), "GITHUB_TOKEN = \"ghp_keep\"\n").unwrap();
        let verifier = FakeVerifier::ok("Ada");
        let (result, out) = repo.run(
            jira(
                Some("https://acme.atlassian.net"),
                Some("ada@acme.test"),
                None,
                true,
            ),
            &[],
            &mut FakeInput::piped(&["tok\n"]),
            &verifier,
        );
        assert_eq!(result.unwrap(), 0);
        assert!(
            out.ends_with("updated target.toml [tracker] kind = \"jira\"\n"),
            "{out}"
        );
        assert_eq!(repo.read(&repo.config()), config);
        let creds = repo.read(&repo.creds());
        assert!(creds.contains("GITHUB_TOKEN = \"ghp_keep\""), "{creds}");
        assert!(creds.contains("JIRA_API_TOKEN = \"tok\""), "{creds}");
    }

    #[test]
    fn invalid_inputs_are_usage_errors() {
        let repo = Repo::new(Some(CONFIG));
        let verifier = FakeVerifier::ok("x");
        for (service, expected) in [
            (
                jira(Some("acme.atlassian.net"), Some("a@b.c"), None, true),
                "--base-url must be an http(s) URL, got \"acme.atlassian.net\"",
            ),
            (
                jira(
                    Some("https://u:p@acme.atlassian.net"),
                    Some("a@b.c"),
                    None,
                    true,
                ),
                "--base-url must not contain credentials",
            ),
            (
                jira(Some("https://acme.atlassian.net"), Some("ada"), None, true),
                "--email must be an email address",
            ),
            (
                jira(
                    Some("https://acme.atlassian.net"),
                    Some("a@b.c"),
                    Some("shop"),
                    true,
                ),
                "--project \"shop\" must match ^[A-Z][A-Z0-9_]*$",
            ),
        ] {
            let (result, _) = repo.run(service, &[], &mut FakeInput::piped(&["t\n"]), &verifier);
            let error = result.unwrap_err();
            assert_eq!(error.code, 2);
            assert_eq!(error.messages, vec![expected.to_string()]);
        }
        assert!(verifier.seen.borrow().is_empty());
    }

    #[test]
    fn errors_from_the_network_never_carry_the_token() {
        assert_eq!(redact("bad ghp_x at host", "ghp_x"), "bad **** at host");
        assert_eq!(
            tracker_error(guard_core::ports::TrackerError::Unauthorized(403)),
            VerifyError::Rejected(403)
        );
        assert_eq!(
            codehost_error(guard_core::ports::CodeHostError::Rejected {
                status: 502,
                body: "secret echo".into()
            }),
            VerifyError::Unavailable("HTTP 502".into())
        );
    }

    #[test]
    fn list_masks_secrets() {
        let env = |key: &str| (key == "GITHUB_TOKEN").then(|| "ghp_abcdefb7c1".to_string());
        let mut out = Vec::new();
        list(&env, &|_| false, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("GITHUB_TOKEN    ****b7c1  environment"),
            "{text}"
        );
        assert!(!text.contains("ghp_abcdef"), "{text}");
    }
}
