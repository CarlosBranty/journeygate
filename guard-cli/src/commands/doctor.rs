//! `guard doctor`: runs every check and never stops at the first failure.

use super::token;
use crate::context::Context;
use crate::error::CliResult;
use crate::error::{EXIT_ENV, EXIT_OK, EXIT_USAGE, hint};
use crate::git;
use crate::http;
use crate::playwright::npx_program;
use crate::process;
use guard_core::catalog::{self, check_against_config};
use guard_core::config;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    ConfigError,
    EnvError,
}

#[derive(Default)]
struct Report {
    lines: Vec<(Status, String)>,
}

impl Report {
    fn add(&mut self, status: Status, message: impl Into<String>) {
        let message = message.into();
        let prefix = match status {
            Status::Ok => "ok  ",
            Status::Warn => "warn",
            Status::ConfigError | Status::EnvError => "fail",
        };
        println!("{prefix}  {message}");
        self.lines.push((status, message));
    }

    fn count(&self, wanted: &[Status]) -> usize {
        self.lines
            .iter()
            .filter(|(s, _)| wanted.contains(s))
            .count()
    }

    fn exit_code(&self) -> u8 {
        if self.count(&[Status::ConfigError]) > 0 {
            EXIT_USAGE
        } else if self.count(&[Status::EnvError]) > 0 {
            EXIT_ENV
        } else {
            EXIT_OK
        }
    }
}

pub fn run(ctx: &Context) -> CliResult<u8> {
    let mut r = Report::default();

    let config = match ctx.read_config() {
        Ok(config) => {
            r.add(
                Status::Ok,
                format!("{} valid", ctx.display(&ctx.config_path)),
            );
            Some(config)
        }
        Err(messages) => {
            for m in messages {
                r.add(
                    Status::ConfigError,
                    format!("{} invalid: {m}", ctx.display(&ctx.config_path)),
                );
            }
            None
        }
    };
    let catalog = match ctx.read_catalog() {
        Ok(catalog) => {
            r.add(
                Status::Ok,
                format!(
                    "catalog valid ({} journeys, {} quarantined)",
                    catalog.journeys.len(),
                    catalog.quarantined_count()
                ),
            );
            Some(catalog)
        }
        Err(messages) => {
            for m in messages {
                r.add(Status::ConfigError, format!("catalog invalid: {m}"));
            }
            None
        }
    };
    if let (Some(config), Some(catalog)) = (&config, &catalog) {
        let issues = check_against_config(catalog, config);
        if issues.is_empty() {
            r.add(Status::Ok, "catalog areas exist in target.toml");
        }
        for issue in issues {
            r.add(Status::ConfigError, issue.to_string());
        }
    }
    if let Some(config) = &config {
        for (i, service) in config.services.iter().enumerate() {
            if !ctx.root.join(&service.cwd).is_dir() {
                r.add(
                    Status::ConfigError,
                    format!(
                        "services[{i}].cwd: directory {} does not exist",
                        service.cwd
                    ),
                );
            }
        }
        if config
            .tracker
            .as_ref()
            .is_some_and(|t| t.ticket_pattern.is_some())
        {
            match config.ticket_regex() {
                Ok(_) => r.add(Status::Ok, "tracker.ticket_pattern compiles"),
                Err(e) => r.add(Status::ConfigError, e.to_string()),
            }
        }
    }

    // Glob lint needs the tracked files.
    if config.is_some() || catalog.is_some() {
        match ctx.in_git.then(|| git::tracked_files(&ctx.root)) {
            Some(Ok(tracked)) => {
                let mut warnings = Vec::new();
                if let Some(config) = &config {
                    warnings.extend(config::lint(config, &tracked));
                }
                if let Some(catalog) = &catalog {
                    warnings.extend(catalog::lint(catalog, &tracked));
                }
                for w in warnings {
                    r.add(Status::Warn, w.to_string());
                }
            }
            _ => r.add(Status::Warn, "not a git repository: glob checks skipped"),
        }
    }

    match process::probe("node", &["--version"]) {
        Some(version) => r.add(Status::Ok, format!("node {version}")),
        None => r.add(Status::EnvError, "node not found on PATH"),
    }
    match process::probe(npx_program(), &["--version"]) {
        Some(_) => r.add(Status::Ok, "npx"),
        None => r.add(Status::EnvError, "npx not found on PATH"),
    }
    match git::version() {
        Some(version) => r.add(Status::Ok, format!("git {version}")),
        None => r.add(Status::EnvError, "git not found on PATH"),
    }
    let (status, message) = docker_check(process::probe(
        "docker",
        &["info", "--format", "{{.ServerVersion}}"],
    ));
    r.add(status, message);

    if let Some(config) = &config {
        let roles = &config.testability.roles;
        if !roles.is_empty() {
            let name = &config.testability.token_env;
            if token(config).is_some() {
                r.add(Status::Ok, format!("{name} set"));
            } else {
                r.add(
                    Status::Warn,
                    format!("{name} is not set (needed by roles: {})", roles.join(", ")),
                );
            }
        }
        let url = config.ready_url();
        match http::is_ready(&url, Duration::from_secs(5)) {
            Ok(()) => r.add(Status::Ok, format!("app ready: GET {url} -> 200")),
            Err(reason) => {
                r.add(
                    Status::Warn,
                    format!("app not ready: GET {url} -> {reason}"),
                );
                hint("run guard up");
            }
        }
    }

    println!(
        "doctor: {} ok, {} warnings, {} errors",
        r.count(&[Status::Ok]),
        r.count(&[Status::Warn]),
        r.count(&[Status::ConfigError, Status::EnvError])
    );
    Ok(r.exit_code())
}

/// `docker info` succeeds with a server version only when the daemon answers;
/// an empty version (daemon down, the client still exits 0) is a warning.
fn docker_check(server_version: Option<String>) -> (Status, String) {
    match server_version {
        Some(version) if !version.is_empty() => (Status::Ok, format!("docker {version}")),
        _ => (
            Status::Warn,
            "docker not available (only needed for --docker)".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_with_a_server_version_is_ok() {
        assert_eq!(
            docker_check(Some("24.0.7".into())),
            (Status::Ok, "docker 24.0.7".to_string())
        );
    }

    #[test]
    fn docker_without_a_daemon_is_a_warning() {
        // `docker info --format` exits 0 with an empty version when the daemon is down.
        let warn = (
            Status::Warn,
            "docker not available (only needed for --docker)".to_string(),
        );
        assert_eq!(docker_check(Some(String::new())), warn);
        assert_eq!(docker_check(None), warn);
    }
}
