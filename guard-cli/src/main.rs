//! `guard`: runs the E2E journey suite locally and in CI (contract: docs/contracts/cli.md).

mod agent_lane;
mod app;
mod cli;
mod commands;
mod context;
mod credentials;
mod error;
mod fake;
mod git;
mod http;
mod impact;
mod playwright;
mod process;
mod runtime;
mod server_client;
mod settings;

use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    // Before anything else (and before any thread): the user credentials
    // file fills in the variables the environment does not set (cli.md).
    credentials::load_at_startup();
    match commands::run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            error.report();
            ExitCode::from(error.code)
        }
    }
}
