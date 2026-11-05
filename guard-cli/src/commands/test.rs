//! `guard test`.

use super::affected::{self, tag_lines};
use super::{run_id, seconds, token};
use crate::cli::TestArgs;
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_ENV, EXIT_FAILED, EXIT_OK, hint, warning};
use crate::http;
use crate::playwright::{self, Runtime};
use crate::process;
use guard_core::routing::{select_all, select_journeys};
use guard_core::runner_env::{RunnerEnvInput, runner_env};
use guard_core::verdict::{
    Classification, RunOutcome, TestOutcome, classify, interpret_exit, local_exit_code,
    parse_report,
};
use std::fs;
use std::time::Duration;

pub fn run(ctx: &Context, args: &TestArgs) -> CliResult<u8> {
    let config = ctx.load_config()?;
    let catalog = ctx.load_catalog(&config)?;

    let selection = if args.all {
        let selection = select_all(&catalog);
        print!("{}", tag_lines(&selection, None));
        selection
    } else if !args.journey.is_empty() {
        let selection =
            select_journeys(&catalog, &args.journey).map_err(|e| CliError::usage(e.to_string()))?;
        print!("{}", tag_lines(&selection, None));
        selection
    } else {
        let thresholds = crate::settings::load_thresholds(ctx, args.thresholds.as_deref())?;
        let affected =
            affected::compute(ctx, &config, &catalog, &thresholds, args.base.as_deref())?;
        if let crate::impact::ImpactAnswer::Unavailable { reason, .. } = &affected.answer {
            warning(format!("impact: {reason}"));
        }
        print!("{}", affected::text(&affected));
        affected.selection
    };

    let mut pw_args = Vec::new();
    if args.headed {
        pw_args.push("--headed".into());
    } else if args.ui {
        pw_args.push("--ui".into());
    } else if args.debug {
        pw_args.push("--debug".into());
    }
    if let Some(shard) = args.shard {
        pw_args.push(format!("--shard={}/{}", shard.index, shard.total));
    }
    let run = Execution {
        e2e_tags: selection.e2e_tags(),
        extra_args: pw_args,
        docker: args.docker,
        interactive: args.ui || args.debug,
        run_id: run_id()?,
    };
    let (exit, report) = execute(ctx, &config, &run)?;

    if run.interactive {
        return Ok(match exit {
            Some(0) => EXIT_OK,
            Some(1) => EXIT_FAILED,
            _ => EXIT_ENV,
        });
    }
    let report_path = ctx.e2e_dir.join("results").join("report.json");
    if let RunOutcome::Infrastructure(reason) = interpret_exit(exit, report.as_ref()) {
        return Err(CliError::env(reason).with_hint(format!(
            "see Playwright output above and {}",
            ctx.display(&report_path)
        )));
    }
    let report = report.expect("interpret_exit requires a report");
    let tests = classify(&report);
    print!("{}", summary(&tests, report.duration_ms()));
    if tests.iter().any(|t| t.status == Classification::Failed) {
        hint("open the HTML report with guard report");
    }
    Ok(local_exit_code(&tests) as u8)
}

/// One Playwright run of `guard test` (also used by `guard repro`).
pub struct Execution {
    /// `E2E_TAGS`; empty = no tag filter.
    pub e2e_tags: String,
    /// Appended after `playwright test` (flags, spec files).
    pub extra_args: Vec<String>,
    pub docker: bool,
    /// `--ui` / `--debug`: no JSON report is read.
    pub interactive: bool,
    pub run_id: String,
}

/// Checks readiness, builds the runner env and runs Playwright. Returns the
/// exit code and the parsed `results/report.json` (when not interactive).
pub fn execute(
    ctx: &Context,
    config: &guard_core::config::TargetConfig,
    run: &Execution,
) -> CliResult<(Option<i32>, Option<guard_core::verdict::PwReport>)> {
    let url = config.ready_url();
    if let Err(reason) = http::is_ready(&url, Duration::from_secs(2)) {
        let error = CliError::env(format!("app not ready: GET {url} -> {reason}"));
        // Self mode: `guard up` cannot start anything, so say what to start.
        return Err(if config.is_self_mode() {
            super::updown::with_self_mode_hints(ctx, error)
        } else {
            error.with_hint("run guard up")
        });
    }
    let token = token(config);
    if !config.testability.roles.is_empty() && token.is_none() {
        warning(format!(
            "{} is not set; login and seed calls will get 401",
            config.testability.token_env
        ));
    }

    let version = if run.docker {
        if process::probe("docker", &["version", "--format", "{{.Server.Version}}"]).is_none() {
            return Err(CliError::env("docker is not available")
                .with_hint("start Docker or run without --docker"));
        }
        Some(playwright::playwright_version(&ctx.e2e_dir).ok_or_else(|| {
            CliError::env("cannot determine the @playwright/test version from the Playwright dir")
        })?)
    } else {
        None
    };

    let mut env = runner_env(&RunnerEnvInput {
        config,
        e2e_tags: &run.e2e_tags,
        run_id: &run.run_id,
        token: token.as_deref(),
        docker: run.docker,
    });
    if run.docker {
        if let Ok(ci) = std::env::var("CI") {
            env.push(("CI".into(), ci));
        }
    }

    let mut pw_args = vec!["playwright".to_string(), "test".to_string()];
    pw_args.extend(run.extra_args.iter().cloned());

    let report_path = ctx.e2e_dir.join("results").join("report.json");
    if !run.interactive {
        let _ = fs::remove_file(&report_path);
        let _ = fs::create_dir_all(ctx.e2e_dir.join("results"));
    }
    let runtime = match &version {
        Some(version) => Runtime::Docker {
            root: &ctx.root,
            version,
        },
        None => Runtime::Local,
    };
    let exit = playwright::run(&ctx.e2e_dir, &pw_args, &env, &runtime)?;
    if run.interactive {
        return Ok((exit, None));
    }
    let report = fs::read_to_string(&report_path)
        .ok()
        .and_then(|t| parse_report(&t).ok());
    Ok((exit, report))
}

/// The summary block printed after a local run.
pub fn summary(tests: &[TestOutcome], duration_ms: u64) -> String {
    let counts = guard_core::verdict::count(tests);
    let mut out = format!(
        "passed {}  flaky {}  failed {}  skipped {}  ({})\n",
        counts.passed,
        counts.flaky,
        counts.failed,
        counts.skipped,
        seconds(duration_ms)
    );
    for test in tests.iter().filter(|t| t.status == Classification::Failed) {
        let tag = test
            .journey
            .as_ref()
            .map_or("-".to_string(), |id| format!("@journey:{id}"));
        let title = test.title.rsplit(" > ").next().unwrap_or(&test.title);
        out.push_str(&format!(
            "FAILED {tag}  {}:{}  {title}\n",
            test.file, test.line
        ));
        if let Some(error) = &test.error {
            out.push_str(&format!("  Error: {error}\n"));
        }
    }
    out.push_str("report: guard report\n");
    out
}
