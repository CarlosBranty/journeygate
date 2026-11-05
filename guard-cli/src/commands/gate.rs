//! `guard gate plan | shard | finalize`.

use super::{run_id, seconds, token};
use crate::app;
use crate::context::{Context, slash};
use crate::error::{CliError, CliResult, EXIT_OK, warning};
use crate::playwright::{self, Runtime};
use guard_core::durations::{DurationHistory, is_planned_spec, parse_history};
use guard_core::routing::select_all;
use guard_core::runner_env::{RunnerEnvInput, runner_env};
use guard_core::shard_plan::plan_shards;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::time::Duration;

/// `.guard/durations.json`; missing = empty history, malformed = exit 2.
fn load_history(ctx: &Context) -> CliResult<DurationHistory> {
    match fs::read_to_string(ctx.durations_path()) {
        Ok(text) => parse_history(&text)
            .map_err(|e| CliError::usage(format!("{}: {e}", ctx.display(&ctx.durations_path())))),
        Err(_) => Ok(DurationHistory::default()),
    }
}

/// Every `tests/**/*.spec.ts` except `tests/setup/**`, relative to the Playwright dir.
pub fn spec_files(e2e_dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if entry.file_name() != "node_modules" {
                    walk(&path, base, out);
                }
            } else if let Ok(rel) = path.strip_prefix(base) {
                let rel = slash(rel);
                if is_planned_spec(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    let mut files = Vec::new();
    walk(&e2e_dir.join("tests"), e2e_dir, &mut files);
    files.sort();
    files
}

pub fn plan(ctx: &Context, shards: usize, out: &Path) -> CliResult<u8> {
    if shards == 0 {
        return Err(CliError::usage("--shards must be at least 1"));
    }
    let history = load_history(ctx)?;
    let files = spec_files(&ctx.e2e_dir);
    let plan = plan_shards(&files, &history.planned_durations(), shards)
        .map_err(|e| CliError::usage(e.to_string()))?;
    fs::create_dir_all(out)
        .map_err(|e| CliError::env(format!("cannot create {}: {e}", out.display())))?;
    for (i, shard) in plan.iter().enumerate() {
        let path = out.join(format!("shard-{}.json", i + 1));
        fs::write(&path, shard.plan_json() + "\n")
            .map_err(|e| CliError::env(format!("cannot write {}: {e}", path.display())))?;
    }
    for (i, shard) in plan.iter().enumerate() {
        let noun = if shard.files.len() == 1 {
            "file"
        } else {
            "files"
        };
        println!(
            "shard {}/{shards}: {} {noun}, est {}",
            i + 1,
            shard.files.len(),
            seconds(shard.estimated_ms)
        );
    }
    Ok(EXIT_OK)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanFile {
    files: Vec<String>,
}

pub fn shard(ctx: &Context, index: usize, plan_dir: &Path) -> CliResult<u8> {
    let plan_path = plan_dir.join(format!("shard-{index}.json"));
    let text = fs::read_to_string(&plan_path)
        .map_err(|e| CliError::usage(format!("cannot read plan {}: {e}", plan_path.display())))?;
    let plan: PlanFile = serde_json::from_str(&text)
        .map_err(|e| CliError::usage(format!("invalid plan {}: {e}", plan_path.display())))?;
    if plan.files.is_empty() {
        println!("shard {index}: nothing to run");
        return Ok(EXIT_OK);
    }

    let run_id = run_id()?;
    let config = ctx.load_config()?;
    let catalog = ctx.load_catalog(&config)?;
    // `app::up` starts nothing when the app and every service are already ready.
    let outcome = app::up(ctx, &config, Duration::from_secs(120))?;

    let e2e_tags = select_all(&catalog).e2e_tags();
    let token = token(&config);
    let mut env = runner_env(&RunnerEnvInput {
        config: &config,
        e2e_tags: &e2e_tags,
        run_id: &run_id,
        token: token.as_deref(),
        docker: false,
    });
    env.push(("CI".into(), "1".into()));
    env.push(("GUARD_SHARD_INDEX".into(), index.to_string()));
    env.push(("GUARD_SHARD_PLAN".into(), slash(&plan_path)));

    let blob = ctx
        .e2e_dir
        .join("blob-report")
        .join(format!("report-{index}.zip"));
    let _ = fs::remove_file(&blob);
    let exit = playwright::run(
        &ctx.e2e_dir,
        &["playwright".into(), "test".into()],
        &env,
        &Runtime::Local,
    );

    // Only what this shard started is stopped; processes it found running are left alone.
    if outcome.started {
        if let Err(e) = app::down_started(ctx, &config, &outcome) {
            warning(format!("could not stop the app: {}", e.messages.join("; ")));
        }
    }
    let exit = exit?;
    if !matches!(exit, Some(0 | 1)) {
        let shown = exit.map_or("a signal".to_string(), |c| format!("code {c}"));
        return Err(CliError::env(format!("Playwright exited with {shown}")));
    }
    if !blob.exists() {
        return Err(CliError::env(format!(
            "no blob report produced at {}",
            ctx.display(&blob)
        )));
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_files_skip_setup_and_non_specs() {
        let dir = tempfile::TempDir::new().unwrap();
        for f in [
            "tests/a/x.spec.ts",
            "tests/setup/s.spec.ts",
            "tests/b.ts",
            "tests/node_modules/n.spec.ts",
            "other/o.spec.ts",
        ] {
            let path = dir.path().join(f);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        assert_eq!(spec_files(dir.path()), vec!["tests/a/x.spec.ts"]);
    }
}
