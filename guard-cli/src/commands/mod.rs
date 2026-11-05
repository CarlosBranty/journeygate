//! One module per command; `run` dispatches and returns the exit code.

mod affected;
mod agents;
mod connect;
mod doctor;
mod explain;
mod finalize;
mod gate;
mod init;
mod integrations;
mod mcp;
mod memory;
mod new_journey;
mod report;
mod repro;
mod serve;
mod status;
mod suspects;
mod test;
mod updown;

use crate::cli::{AgentsCommand, Cli, Command, GateCommand, MemoryCommand, NewCommand};
use crate::context::Context;
use crate::error::CliResult;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn run(cli: Cli) -> CliResult<u8> {
    let ctx = Context::resolve(&cli)?;
    match cli.command {
        Command::Init(args) => init::run(&ctx, &args),
        Command::Doctor => doctor::run(&ctx),
        Command::Up { timeout } => updown::up(&ctx, timeout),
        Command::Down => updown::down(&ctx),
        Command::Status {
            json,
            integrations: true,
        } => integrations::run(&ctx, json),
        Command::Status { json, .. } => status::run(&ctx, json),
        Command::Connect(args) => connect::run(&ctx, &args),
        Command::New { what } => match what {
            NewCommand::Journey(args) => new_journey::run(&ctx, &args),
        },
        Command::Test(args) => test::run(&ctx, &args),
        Command::Affected {
            base,
            json,
            thresholds,
        } => affected::run(&ctx, base.as_deref(), json, thresholds.as_deref()),
        Command::Gate { step } => match step {
            GateCommand::Plan { shards, out } => gate::plan(&ctx, shards, &absolute(&out)),
            GateCommand::Shard { index, plan } => gate::shard(&ctx, index, &absolute(&plan)),
            GateCommand::Finalize(args) => finalize::run(&ctx, &args),
        },
        Command::Report => report::run(&ctx),
        Command::Explain(args) => explain::run(&ctx, &args),
        Command::Suspects(args) => suspects::run(&ctx, &args),
        Command::Serve(args) => serve::run(&ctx, &args),
        Command::Mcp => mcp::run(&ctx),
        Command::Repro(args) => repro::run(&ctx, &args),
        Command::Agents { command } => match command {
            AgentsCommand::Check => agents::check(&ctx),
            AgentsCommand::Sync { check, runtimes } => agents::sync(&ctx, check, &runtimes),
            AgentsCommand::Run(args) => agents::run(&ctx, &args),
            AgentsCommand::Status { ticket, json } => agents::status(&ctx, ticket.as_deref(), json),
            AgentsCommand::AbReport { experiment, out } => {
                agents::ab_report(&experiment, out.as_deref().map(absolute).as_deref())
            }
        },
        Command::Memory { command } => match command {
            MemoryCommand::Export { engram } => memory::export(&ctx, &absolute(&engram)),
        },
    }
}

/// Resolves a user-supplied path against the current dir.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `RUN_ID` from the environment, else `r<unix-seconds>-<pid>`. An inherited
/// value outside `^[A-Za-z0-9_.-]{1,64}$` (runner-env.md) is a usage error
/// (exit 2): it would break the per-test data namespace inside Playwright.
fn run_id() -> CliResult<String> {
    checked_run_id(std::env::var("RUN_ID").ok())
}

fn checked_run_id(inherited: Option<String>) -> CliResult<String> {
    let id = guard_core::runner_env::resolve_run_id(inherited, unix_now(), std::process::id());
    if guard_core::runner_env::is_valid_run_id(&id) {
        Ok(id)
    } else {
        Err(crate::error::CliError::usage(format!(
            "RUN_ID {id:?} must match ^[A-Za-z0-9_.-]{{1,64}}$"
        )))
    }
}

/// Seconds with one decimal, rounded half up from milliseconds.
fn seconds(ms: u64) -> String {
    let tenths = (ms + 50) / 100;
    format!("{}.{}s", tenths / 10, tenths % 10)
}

/// Value of the variable named by `[testability].token_env`.
fn token(config: &guard_core::config::TargetConfig) -> Option<String> {
    std::env::var(&config.testability.token_env)
        .ok()
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_round_half_up_to_one_decimal() {
        assert_eq!(seconds(0), "0.0s");
        assert_eq!(seconds(12_450), "12.5s");
        assert_eq!(seconds(9_849), "9.8s");
        assert_eq!(seconds(38_412), "38.4s");
    }

    #[test]
    fn run_id_forwards_a_valid_inherited_value_and_rejects_an_invalid_one() {
        assert_eq!(checked_run_id(Some("12345-1".into())).unwrap(), "12345-1");
        assert!(checked_run_id(None).unwrap().starts_with('r'));
        assert!(
            checked_run_id(Some(String::new()))
                .unwrap()
                .starts_with('r')
        );
        let error = checked_run_id(Some("a b".into())).unwrap_err();
        assert_eq!(error.code, crate::error::EXIT_USAGE);
        assert!(checked_run_id(Some("x".repeat(65))).is_err());
    }
}
