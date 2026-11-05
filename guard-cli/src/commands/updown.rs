//! `guard up` and `guard down`.

use super::seconds;
use crate::app::{self, ServiceState};
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::http;
use guard_core::config::TargetConfig;
use std::io::Write;
use std::time::{Duration, Instant};

/// The self-mode hint (cli.md "Self-mode hint"), read from the commented
/// block of `target.toml`.
pub fn self_mode_hints(ctx: &Context) -> Vec<String> {
    let text = std::fs::read_to_string(&ctx.config_path).unwrap_or_default();
    guard_core::self_mode::hint_lines(&text)
}

/// `error` with the self-mode hint lines appended.
pub fn with_self_mode_hints(ctx: &Context, error: CliError) -> CliError {
    self_mode_hints(ctx)
        .into_iter()
        .fold(error, |e, line| e.with_hint(line))
}

pub fn up(ctx: &Context, timeout: u64) -> CliResult<u8> {
    let config = ctx.load_config()?;
    if config.is_self_mode() {
        return wait_for_own_app(ctx, &config, Duration::from_secs(timeout));
    }
    let url = config.ready_url();
    let outcome = app::up(ctx, &config, Duration::from_secs(timeout))?;
    for service in &outcome.services {
        let name = &service.name;
        match &service.state {
            ServiceState::AlreadyReady => println!("up: {name} already ready"),
            ServiceState::Ready { url, elapsed } => println!(
                "up: {name} ready at {url} in {}",
                seconds(elapsed.as_millis() as u64)
            ),
            ServiceState::Started => println!("up: {name} started"),
            ServiceState::Running => println!("up: {name} already running"),
        }
    }
    if outcome.already_ready {
        println!("up: already ready at {url}");
    } else {
        println!(
            "up: ready at {url} in {}",
            seconds(outcome.elapsed.as_millis() as u64)
        );
    }
    Ok(EXIT_OK)
}

/// Self mode: never spawns anything, only waits for the developer's app.
fn wait_for_own_app(ctx: &Context, config: &TargetConfig, timeout: Duration) -> CliResult<u8> {
    let url = config.ready_url();
    if http::is_ready(&url, Duration::from_secs(2)).is_ok() {
        println!("up: already ready at {url}");
        return Ok(EXIT_OK);
    }
    println!("up: waiting for your app at {url} (start it yourself; guard does not start it)");
    let _ = std::io::stdout().flush();
    let begin = Instant::now();
    match http::wait_ready(&url, begin + timeout, || None) {
        Ok(()) => {
            println!(
                "up: ready at {url} in {}",
                seconds(begin.elapsed().as_millis() as u64)
            );
            Ok(EXIT_OK)
        }
        Err(reason) => Err(with_self_mode_hints(
            ctx,
            CliError::env(format!(
                "app not ready at {url} after {}s: {reason}",
                timeout.as_secs()
            )),
        )),
    }
}

pub fn down(ctx: &Context) -> CliResult<u8> {
    let config = ctx.load_config()?;
    let outcome = app::down(ctx, &config)?;
    println!(
        "{}",
        if outcome.stopped {
            "down: stopped"
        } else if config.is_self_mode() {
            "down: nothing to stop: guard did not start the app"
        } else {
            "down: nothing to stop"
        }
    );
    Ok(EXIT_OK)
}
