//! `guard memory export --engram <file>`: the repo's memory facts as an
//! Engram import file (docs/contracts/memory.md, "Engram export"). Reads
//! Postgres (`DATABASE_URL`); never writes memory.

use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::runtime::block_on;
use std::path::Path;

pub fn export(ctx: &Context, file: &Path) -> CliResult<u8> {
    let target = ctx.load_config()?;
    let repo = crate::agent_lane::repo_slug(ctx, &target);
    let url = std::env::var(guard_store::DATABASE_URL_ENV)
        .ok()
        .filter(|u| u.starts_with("postgres"))
        .ok_or_else(|| CliError::env("guard memory export needs DATABASE_URL (Postgres)"))?;
    let export = block_on(async {
        let store = guard_store::PgStore::connect(&url)
            .await
            .map_err(|e| CliError::env(format!("database: {e}")))?;
        let facts = guard_store::PgMemoryFacts::new(store.pool().clone())
            .stored_facts(&repo)
            .await
            .map_err(|e| CliError::env(format!("memory facts: {e}")))?;
        let now = guard_agents::runs::rfc3339(super::unix_now());
        Ok::<_, CliError>(guard_store::memory_export::engram_export(
            &repo, &facts, &now,
        ))
    })?;
    let text = serde_json::to_string_pretty(&export)
        .map_err(|e| CliError::env(format!("engram export: {e}")))?;
    std::fs::write(file, format!("{text}\n"))
        .map_err(|e| CliError::env(format!("{}: {e}", file.display())))?;
    println!("memory: exported {repo} to {}", file.display());
    Ok(EXIT_OK)
}
