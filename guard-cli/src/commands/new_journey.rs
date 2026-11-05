//! `guard new journey`: one catalog entry plus its spec skeleton (cli.md).

use super::init::{Runner, SystemRunner};
use crate::cli::NewJourneyArgs;
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK, warning};
use guard_core::catalog::new_journey::{self, NewJourney, NewJourneyError};
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};

pub fn run(ctx: &Context, args: &NewJourneyArgs) -> CliResult<u8> {
    run_with(ctx, args, &mut io::stdout().lock(), &SystemRunner)
}

fn usage(error: NewJourneyError) -> CliError {
    CliError::usage(error.to_string())
}

fn out_err(e: io::Error) -> CliError {
    CliError::env(format!("cannot write to stdout: {e}"))
}

/// `npx playwright codegen <base_url>`: the program and its arguments.
pub(crate) fn codegen_command(base_url: &str) -> (OsString, Vec<String>) {
    (
        crate::playwright::npx_program(),
        vec!["playwright".into(), "codegen".into(), base_url.into()],
    )
}

pub(crate) fn run_with(
    ctx: &Context,
    args: &NewJourneyArgs,
    out: &mut dyn Write,
    runner: &dyn Runner,
) -> CliResult<u8> {
    let journey = new_journey::build(&NewJourney {
        id: args.id.clone(),
        area: args.area.clone(),
        title: args.title.clone(),
        paths: args.paths.clone(),
        smoke: args.smoke,
    })
    .map_err(usage)?;
    let config = ctx.load_config()?;
    if !config.areas.contains_key(&journey.area) {
        let keys: Vec<&str> = config.areas.keys().map(String::as_str).collect();
        return Err(CliError::usage(format!(
            "area {} is not a key of [areas] in {}",
            journey.area,
            ctx.display(&ctx.config_path)
        ))
        .with_hint(format!("areas: {}", keys.join(", "))));
    }
    ctx.load_catalog(&config)?;
    let catalog_path = ctx.catalog_path();
    let text = fs::read_to_string(&catalog_path)
        .map_err(|e| CliError::usage(format!("cannot read {}: {e}", ctx.display(&catalog_path))))?;
    let updated = new_journey::append_journey(&text, &journey).map_err(usage)?;

    let spec = ctx.e2e_dir.join(new_journey::spec_path(&journey));
    let spec_shown = ctx.display(&spec);
    if spec.exists() {
        return Err(CliError::usage(format!(
            "{spec_shown} already exists; nothing was written"
        )));
    }
    if let Some(parent) = spec.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| CliError::env(format!("cannot create {}: {e}", ctx.display(parent))))?;
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&spec)
        .and_then(|mut file| file.write_all(new_journey::render_spec(&journey).as_bytes()))
        .map_err(|e| CliError::env(format!("cannot write {spec_shown}: {e}")))?;
    if let Err(e) = fs::write(&catalog_path, updated) {
        // Keep the pair consistent: no spec without its catalog entry.
        let _ = fs::remove_file(&spec);
        return Err(CliError::env(format!(
            "cannot write {}: {e}",
            ctx.display(&catalog_path)
        )));
    }

    writeln!(
        out,
        "added journey {} to {}",
        journey.id,
        ctx.display(&catalog_path)
    )
    .map_err(out_err)?;
    writeln!(out, "created {spec_shown}").map_err(out_err)?;
    writeln!(out, "next: guard test --journey {}", journey.id).map_err(out_err)?;

    if args.record {
        out.flush().map_err(out_err)?;
        let (program, codegen) = codegen_command(&config.app.base_url);
        let codegen: Vec<&str> = codegen.iter().map(String::as_str).collect();
        let shown = format!("{} {}", program.to_string_lossy(), codegen.join(" "));
        match runner.run(&program, &codegen, &ctx.e2e_dir) {
            Ok(Some(0)) => {}
            Ok(code) => warning(format!(
                "{shown} exited with {}; the journey files are written",
                code.map_or("a signal".to_string(), |c| c.to_string())
            )),
            Err(e) => {
                return Err(CliError::env(format!(
                    "cannot run {shown}: {e}; the journey files are written"
                )));
            }
        }
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    struct Recorder {
        calls: RefCell<Vec<(OsString, Vec<String>, PathBuf)>>,
        result: fn() -> io::Result<Option<i32>>,
    }

    impl Runner for Recorder {
        fn run(&self, program: &OsString, args: &[&str], cwd: &Path) -> io::Result<Option<i32>> {
            self.calls.borrow_mut().push((
                program.clone(),
                args.iter().map(ToString::to_string).collect(),
                cwd.to_path_buf(),
            ));
            (self.result)()
        }
    }

    fn project() -> (tempfile::TempDir, Context) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(
            root.join("target.toml"),
            "version = 1\n[app]\nname = \"a\"\nbase_url = \"http://127.0.0.1:4321\"\n\
             [areas.home]\ndescription = \"Home\"\npaths = [\"web/**\"]\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("e2e-playwright/catalog")).unwrap();
        fs::write(
            root.join("e2e-playwright/catalog/journeys.json"),
            "{\n  \"version\": 1,\n  \"journeys\": []\n}\n",
        )
        .unwrap();
        let ctx = Context::at(root);
        (dir, ctx)
    }

    fn args(record: bool) -> NewJourneyArgs {
        NewJourneyArgs {
            id: "home.landing".into(),
            area: None,
            title: None,
            paths: Vec::new(),
            smoke: false,
            record,
        }
    }

    #[test]
    fn record_runs_playwright_codegen_on_the_base_url_in_the_e2e_dir() {
        let (_dir, ctx) = project();
        let runner = Recorder {
            calls: RefCell::new(Vec::new()),
            result: || Ok(Some(0)),
        };
        let mut out = Vec::new();
        assert_eq!(run_with(&ctx, &args(true), &mut out, &runner).unwrap(), 0);
        let calls = runner.calls.into_inner();
        assert_eq!(calls.len(), 1);
        let (program, argv, cwd) = &calls[0];
        assert_eq!(program, &crate::playwright::npx_program());
        assert_eq!(argv, &["playwright", "codegen", "http://127.0.0.1:4321"]);
        assert_eq!(cwd, &ctx.e2e_dir);
    }

    #[test]
    fn without_record_nothing_runs_and_a_failing_codegen_keeps_exit_zero() {
        let (_dir, ctx) = project();
        let runner = Recorder {
            calls: RefCell::new(Vec::new()),
            result: || Ok(Some(1)),
        };
        let mut out = Vec::new();
        run_with(&ctx, &args(false), &mut out, &runner).unwrap();
        assert!(runner.calls.borrow().is_empty());
        let mut second = args(true);
        second.id = "home.search".into();
        assert_eq!(run_with(&ctx, &second, &mut out, &runner).unwrap(), 0);
        assert_eq!(runner.calls.borrow().len(), 1);
    }

    #[test]
    fn a_missing_npx_is_an_environment_error_after_the_files_are_written() {
        let (_dir, ctx) = project();
        let runner = Recorder {
            calls: RefCell::new(Vec::new()),
            result: || Err(io::Error::new(io::ErrorKind::NotFound, "not found")),
        };
        let mut out = Vec::new();
        let error = run_with(&ctx, &args(true), &mut out, &runner).unwrap_err();
        assert_eq!(error.code, crate::error::EXIT_ENV);
        assert!(
            ctx.e2e_dir
                .join("tests/journeys/home/landing.spec.ts")
                .is_file()
        );
    }
}
