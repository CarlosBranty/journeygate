//! `guard report`.

use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::playwright::{self, Runtime};

pub fn run(ctx: &Context) -> CliResult<u8> {
    let index = ctx.e2e_dir.join("playwright-report").join("index.html");
    if !index.exists() {
        return Err(
            CliError::env(format!("no HTML report at {}", ctx.display(&index)))
                .with_hint("run guard test first"),
        );
    }
    let args = ["playwright", "show-report", "playwright-report"].map(String::from);
    playwright::run(&ctx.e2e_dir, &args, &[], &Runtime::Local)?;
    Ok(EXIT_OK)
}
