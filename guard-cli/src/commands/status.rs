//! `guard status [--json]`: what runs where, from target.toml only (cli.md).
//!
//! Read-only: it never starts, stops or signals anything.

use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::{http, process};
use guard_core::config::TargetConfig;
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    App,
    Service,
}

/// One row: `[app]` or one `[[services]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    pub kind: Kind,
    pub name: String,
    pub cwd: String,
    pub start: Option<String>,
    pub ready_url: Option<String>,
    pub pid: Option<u32>,
    pub alive: Option<bool>,
    pub ready: Option<bool>,
    pub log: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub base_url: String,
    pub ready: bool,
    pub entries: Vec<Entry>,
}

pub fn run(ctx: &Context, json: bool) -> CliResult<u8> {
    let config = ctx.load_config()?;
    let status = collect(ctx, &config, &|url| {
        http::get_status(url, Duration::from_secs(2)) == Ok(200)
    });
    let mut out = io::stdout().lock();
    let written = if json {
        serde_json::to_string_pretty(&status)
            .map_err(io::Error::other)
            .and_then(|text| writeln!(out, "{text}"))
    } else {
        write!(out, "{}", render(&status))
    };
    written.map_err(|e| CliError::env(format!("cannot write to stdout: {e}")))?;
    Ok(EXIT_OK)
}

/// The PID recorded in `<dir>/<stem>.pid` and whether that process is still
/// the one `guard up` started (`<stem>.identity`; without one, whether it exists).
fn recorded(dir: &Path, stem: &str) -> (Option<u32>, Option<bool>) {
    let pid_file: PathBuf = dir.join(format!("{stem}.pid"));
    let Some(pid) = fs::read_to_string(pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
    else {
        return (None, None);
    };
    let identity = fs::read_to_string(dir.join(format!("{stem}.identity"))).ok();
    let current = process::start_identity(pid);
    let alive = current.is_some()
        && process::same_process(identity.as_deref().map(str::trim), current.as_deref());
    (Some(pid), Some(alive))
}

pub(crate) fn collect(
    ctx: &Context,
    config: &TargetConfig,
    ready: &dyn Fn(&str) -> bool,
) -> Status {
    let guard_dir = ctx.guard_dir();
    let services_dir = guard_dir.join("services");
    let app_url = config.ready_url();
    let app_ready = ready(&app_url);
    let (pid, alive) = recorded(&guard_dir, "app");
    let mut entries = vec![Entry {
        kind: Kind::App,
        name: config.app.name.clone(),
        cwd: ".".into(),
        start: config.app.start.clone(),
        ready_url: Some(app_url),
        pid,
        alive,
        ready: Some(app_ready),
        log: ".guard/app.log".into(),
    }];
    for service in &config.services {
        let (pid, alive) = recorded(&services_dir, &service.name);
        entries.push(Entry {
            kind: Kind::Service,
            name: service.name.clone(),
            cwd: service.cwd.clone(),
            start: Some(service.start.clone()),
            ready_url: service.ready_url.clone(),
            pid,
            alive,
            ready: service.ready_url.as_deref().map(ready),
            log: format!(".guard/services/{}.log", service.name),
        });
    }
    Status {
        base_url: config.app.base_url.clone(),
        ready: app_ready,
        entries,
    }
}

fn yes_no(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    }
}

/// The text table: columns padded to their widest cell plus two spaces.
pub(crate) fn render(status: &Status) -> String {
    let header = ["NAME", "CWD", "PID", "ALIVE", "READY", "URL", "LOG"].map(String::from);
    let mut rows = vec![header.to_vec()];
    for e in &status.entries {
        rows.push(vec![
            e.name.clone(),
            e.cwd.clone(),
            e.pid.map_or("-".into(), |p| p.to_string()),
            yes_no(e.alive).into(),
            yes_no(e.ready).into(),
            e.ready_url.clone().unwrap_or_else(|| "-".into()),
            e.log.clone(),
        ]);
    }
    let columns = header.len();
    let widths: Vec<usize> = (0..columns)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let mut line = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c + 1 == columns {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}", width = widths[c] + 2));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.push_str(&format!(
        "base_url: {} {}\n",
        status.base_url,
        if status.ready { "ready" } else { "not ready" }
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> (tempfile::TempDir, Context) {
        let dir = tempfile::TempDir::new().unwrap();
        let ctx = Context::at(dir.path());
        (dir, ctx)
    }

    fn config(services: &str) -> TargetConfig {
        guard_core::config::parse_target(&format!(
            "version = 1\n[app]\nname = \"shop\"\nbase_url = \"http://127.0.0.1:3000\"\nready_path = \"/\"\n\
             {services}\n[areas.web]\ndescription = \"Web\"\npaths = [\"web/**\"]\n"
        ))
        .unwrap()
    }

    #[test]
    fn a_recorded_pid_is_alive_only_while_its_identity_matches() {
        let (_dir, ctx) = context();
        let dir = ctx.guard_dir();
        fs::create_dir_all(&dir).unwrap();
        // This test's own process: only read, never signalled.
        let own = std::process::id();
        fs::write(dir.join("app.pid"), own.to_string()).unwrap();
        assert_eq!(recorded(&dir, "app"), (Some(own), Some(true)));
        let identity = process::start_identity(own).expect("own start identity");
        fs::write(dir.join("app.identity"), format!("{identity}\n")).unwrap();
        assert_eq!(recorded(&dir, "app"), (Some(own), Some(true)));
        fs::write(dir.join("app.identity"), "another process").unwrap();
        assert_eq!(recorded(&dir, "app"), (Some(own), Some(false)));
        fs::write(dir.join("app.pid"), "garbage").unwrap();
        assert_eq!(recorded(&dir, "app"), (None, None));
    }

    #[test]
    fn entries_follow_file_order_and_probe_only_ready_urls() {
        let (_dir, ctx) = context();
        let config = config(
            "[[services]]\nname = \"api\"\ncwd = \"api\"\nstart = \"bun run dev\"\nready_url = \"http://127.0.0.1:3001/health\"\n\
             [[services]]\nname = \"worker\"\nstart = \"node w.js\"\n",
        );
        let probed = std::cell::RefCell::new(Vec::new());
        let status = collect(&ctx, &config, &|url| {
            probed.borrow_mut().push(url.to_string());
            url.ends_with("/health")
        });
        assert_eq!(
            probed.into_inner(),
            ["http://127.0.0.1:3000/", "http://127.0.0.1:3001/health"]
        );
        assert!(!status.ready);
        let names: Vec<&str> = status.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["shop", "api", "worker"]);
        assert_eq!(status.entries[1].ready, Some(true));
        assert_eq!(status.entries[2].ready, None);
        assert_eq!(
            render(&status),
            "NAME    CWD  PID  ALIVE  READY  URL                           LOG\n\
             shop    .    -    -      no     http://127.0.0.1:3000/        .guard/app.log\n\
             api     api  -    -      yes    http://127.0.0.1:3001/health  .guard/services/api.log\n\
             worker  .    -    -      -      -                             .guard/services/worker.log\n\
             base_url: http://127.0.0.1:3000 not ready\n"
        );
    }
}
