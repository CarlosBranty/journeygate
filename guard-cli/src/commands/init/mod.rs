//! `guard init`: detect how the app runs, confirm the plan, write
//! `target.toml` and scaffold the Playwright project (cli.md "guard init").

mod detect_io;
mod plan;
mod prompt;
mod scaffold;

use crate::cli::{CiHost, InitArgs, RunnerMode};
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK, warning};
use crate::git;
use guard_agents::config::RuntimeKind;
use guard_agents::sync::{MCP_RUNTIMES, sync_mcp};
use guard_core::detect::{
    EMPTY_CATALOG, InitPlan, app_plan, detect_app, detect_code_host, detect_languages,
    detect_start_stop, detect_ticket_project, normalize_app_name, propose_areas_with_warnings,
};
use prompt::{Plain, Prompter};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// Runs a program to completion; `Ok(None)` = killed by a signal.
pub trait Runner {
    fn run(&self, program: &OsString, args: &[&str], cwd: &Path) -> io::Result<Option<i32>>;
}

pub(crate) struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, program: &OsString, args: &[&str], cwd: &Path) -> io::Result<Option<i32>> {
        std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .status()
            .map(|s| s.code())
    }
}

/// The npm executable. `GUARD_NPM` overrides it (a stub in tests).
fn npm_program() -> OsString {
    match std::env::var_os("GUARD_NPM") {
        Some(program) if !program.is_empty() => program,
        _ if cfg!(windows) => "npm.cmd".into(),
        _ => "npm".into(),
    }
}

/// What `init` talks to besides the file system.
pub struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    /// Ask before writing (stdin and stdout are terminals, no `--yes`).
    pub interactive: bool,
    pub runner: &'a dyn Runner,
}

pub fn run(ctx: &Context, args: &InitArgs) -> CliResult<u8> {
    let stdin = io::stdin();
    let (stdin_tty, stdout_tty) = (stdin.is_terminal(), io::stdout().is_terminal());
    let no_color = std::env::var_os("NO_COLOR").is_some();
    let terminals = prompt::Terminals::detect();
    if prompt::use_styled(terminals, args.yes, no_color, args.dry_run) {
        return run_flow(ctx, args, &mut prompt::Styled::default(), &SystemRunner);
    }
    let interactive = !args.yes && stdin_tty && stdout_tty;
    let mut input = stdin.lock();
    let mut out = io::stdout().lock();
    run_with(
        ctx,
        args,
        &mut Io {
            input: &mut input,
            out: &mut out,
            interactive,
            runner: &SystemRunner,
        },
    )
}

/// `init` with the plain prompter on `io`.
pub fn run_with(ctx: &Context, args: &InitArgs, io: &mut Io<'_>) -> CliResult<u8> {
    let mut prompter = Plain {
        input: &mut *io.input,
        out: &mut *io.out,
        interactive: io.interactive,
    };
    run_flow(ctx, args, &mut prompter, io.runner)
}

/// A prompt failed: Ctrl+C / Esc ends `init` with exit 130 after `cancelled`;
/// anything else means the terminal cannot be read (exit 3).
fn asked<T>(p: &mut dyn Prompter, result: io::Result<T>, cancelled: &str) -> CliResult<T> {
    result.map_err(|e| {
        if e.kind() == io::ErrorKind::Interrupted {
            let _ = p.outro_cancel(cancelled);
            CliError::interrupted()
        } else {
            CliError::env(format!("cannot read the answer from the terminal: {e}"))
                .with_hint("set NO_COLOR=1 or pass --yes to use the plain prompts")
        }
    })
}

const NOTHING_WRITTEN: &str = "init cancelled, nothing written";

/// The runner options in the order shown (cli.md "Runner"); `guard` is the default.
const RUNNER_CHOICES: [(RunnerMode, &str); 2] = [
    (
        RunnerMode::SelfRun,
        "I start it myself (guard only needs the URLs)",
    ),
    (
        RunnerMode::Guard,
        "guard starts it (start commands, also needed for CI)",
    ),
];

/// `--runner`, else the styled select, else `guard`.
fn choose_runner(args: &InitArgs, p: &mut dyn Prompter) -> io::Result<RunnerMode> {
    if let Some(runner) = args.runner {
        return Ok(runner);
    }
    if !p.styled() {
        return Ok(RunnerMode::Guard);
    }
    let labels: Vec<&str> = RUNNER_CHOICES.iter().map(|(_, l)| *l).collect();
    let default = RUNNER_CHOICES
        .iter()
        .position(|(r, _)| *r == RunnerMode::Guard)
        .unwrap_or_default();
    let chosen = p.select("How will you run the app locally?", &labels, default)?;
    Ok(RUNNER_CHOICES
        .get(chosen)
        .map_or(RunnerMode::Guard, |(r, _)| *r))
}

fn stdout_err(e: io::Error) -> CliError {
    CliError::env(format!("cannot write to stdout: {e}"))
}

fn write_err(path: &Path, e: io::Error) -> CliError {
    CliError::env(format!("cannot write {}: {e}", path.display()))
}

fn write_file(path: &Path, body: &str) -> CliResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| write_err(parent, e))?;
    }
    fs::write(path, body).map_err(|e| write_err(path, e))
}

fn mcp_runtimes(names: &[String]) -> CliResult<BTreeSet<RuntimeKind>> {
    names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|name| {
            RuntimeKind::from_name(name.trim())
                .filter(|r| MCP_RUNTIMES.contains(r))
                .ok_or_else(|| {
                    let known: Vec<&str> = MCP_RUNTIMES.iter().map(|r| r.as_str()).collect();
                    CliError::usage(format!(
                        "unknown --mcp runtime {name:?} (expected {})",
                        known.join(", ")
                    ))
                })
        })
        .collect()
}

/// A file `init` may create: kept when it exists unless `--force`.
struct Extra {
    path: PathBuf,
    body: String,
    executable: bool,
}

fn extras(ctx: &Context, args: &InitArgs) -> CliResult<Vec<Extra>> {
    let mut out = Vec::new();
    if args.hook {
        if ctx.in_git {
            let hooks = git::hooks_dir(&ctx.root)
                .map_err(|e| CliError::env(format!("cannot find the git hooks dir: {e}")))?;
            out.push(Extra {
                path: hooks.join("pre-push"),
                body: scaffold::PRE_PUSH_HOOK.into(),
                executable: true,
            });
        } else {
            warning("--hook needs a git repository; skipped");
        }
    }
    match args.ci {
        Some(CiHost::Github) => out.push(Extra {
            path: ctx.root.join(".github/workflows/guard.yml"),
            body: scaffold::github_workflow(&args.action_ref),
            executable: false,
        }),
        Some(CiHost::Gitlab) => out.push(Extra {
            path: ctx.root.join(".gitlab-ci.yml"),
            body: scaffold::lf(scaffold::GITLAB_CI),
            executable: false,
        }),
        None => {}
    }
    Ok(out)
}

fn make_executable(path: &Path) -> CliResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| write_err(path, e))?;
    }
    let _ = path;
    Ok(())
}

/// `--runner`, else `guard` (cli.md "Runner"; the plain prompter never asks).
fn runner_mode(args: &InitArgs) -> RunnerMode {
    args.runner.unwrap_or_default()
}

/// The stderr warning for `--ci` with runner `self`.
fn ci_warning(ci: Option<CiHost>, runner: RunnerMode) -> Option<String> {
    (ci.is_some() && runner == RunnerMode::SelfRun).then(|| {
        "CI needs start commands: guard does not start the app with --runner self; \
         uncomment the detected commands in target.toml before CI runs"
            .to_string()
    })
}

/// What step 7 writes (cli.md "guard init").
struct Files<'a> {
    existed: bool,
    scaffold_exists: bool,
    e2e_label: &'a str,
    toml: &'a str,
    runtimes: &'a BTreeSet<RuntimeKind>,
    extras: &'a [Extra],
}

/// Step 7: writes the files and reports each one as it goes.
fn write_files(
    ctx: &Context,
    args: &InitArgs,
    p: &mut dyn Prompter,
    files: &Files<'_>,
) -> CliResult<()> {
    write_file(&ctx.config_path, files.toml)?;
    let verb = if files.existed {
        "overwrote"
    } else {
        "created"
    };
    p.line(&format!("{verb} {}", ctx.display(&ctx.config_path)))
        .map_err(stdout_err)?;

    if files.scaffold_exists {
        p.line(&format!(
            "note: {} exists, scaffold skipped",
            files.e2e_label
        ))
        .map_err(stdout_err)?;
        let catalog = ctx.catalog_path();
        if catalog.exists() {
            p.line(&format!("kept existing {}", ctx.display(&catalog)))
                .map_err(stdout_err)?;
        } else {
            write_file(&catalog, EMPTY_CATALOG)?;
            p.line(&format!("created {}", ctx.display(&catalog)))
                .map_err(stdout_err)?;
        }
    } else {
        for (rel, body) in scaffold::E2E_TEMPLATE {
            write_file(&ctx.e2e_dir.join(rel), &scaffold::lf(body))?;
        }
        p.line(&format!(
            "created {} ({} files)",
            files.e2e_label,
            scaffold::E2E_TEMPLATE.len()
        ))
        .map_err(stdout_err)?;
    }

    if !files.runtimes.is_empty() {
        let report = sync_mcp(&ctx.root, files.runtimes, false)
            .map_err(|e| CliError::usage(format!("--mcp: {e}")))?;
        for path in &report.written {
            p.line(&format!("wrote {path}")).map_err(stdout_err)?;
        }
        for path in &report.unchanged {
            p.line(&format!("kept {path} (unchanged)"))
                .map_err(stdout_err)?;
        }
    }

    for extra in files.extras {
        let shown = ctx.display(&extra.path);
        let present = extra.path.exists();
        if present && !args.force {
            p.line(&format!(
                "note: {shown} exists, kept (use --force to overwrite)"
            ))
            .map_err(stdout_err)?;
            continue;
        }
        write_file(&extra.path, &extra.body)?;
        if extra.executable {
            make_executable(&extra.path)?;
        }
        let verb = if present { "overwrote" } else { "created" };
        p.line(&format!("{verb} {shown}")).map_err(stdout_err)?;
    }
    Ok(())
}

/// Detection plus the previous `init` heuristics, as one plan.
fn build_plan(
    ctx: &Context,
) -> (
    guard_core::detect::DetectionReport,
    InitPlan,
    Vec<String>,
    Vec<String>,
) {
    let (input, io_notes) = detect_io::detect_input(ctx);
    let report = detect_app(&input);
    let app = app_plan(&report);

    let tracked = if ctx.in_git {
        git::tracked_files(&ctx.root).unwrap_or_else(|_| detect_io::walk(&ctx.root))
    } else {
        detect_io::walk(&ctx.root)
    };
    let (start, stop) = if app.start.is_none() && app.services.is_empty() {
        // Nothing new detected: the previous rules (root compose file, `npm start`).
        let root_entries: Vec<String> = fs::read_dir(&ctx.root)
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let package_json = input.contents.get("package.json").map(String::as_str);
        let previous = detect_start_stop(&root_entries, package_json);
        (previous.start, previous.stop)
    } else {
        (app.start, app.stop)
    };
    let (code_host, ticket_project) = if ctx.in_git {
        (
            git::origin_url(&ctx.root).and_then(|url| detect_code_host(&url)),
            detect_ticket_project(&git::branches(&ctx.root)),
        )
    } else {
        (None, None)
    };
    let (areas, area_warnings) = propose_areas_with_warnings(&tracked);
    let plan = InitPlan {
        name: normalize_app_name(&input.repo_name),
        languages: detect_languages(&tracked),
        start,
        stop,
        code_host,
        ticket_project,
        base_url: app.base_url,
        ready_path: app.ready_path,
        services: app.services,
        areas,
    };
    (report, plan, io_notes, area_warnings)
}

pub fn run_flow(
    ctx: &Context,
    args: &InitArgs,
    p: &mut dyn Prompter,
    runner_exec: &dyn Runner,
) -> CliResult<u8> {
    let existed = ctx.config_path.exists();
    if existed && !args.force {
        return Err(CliError::usage(format!(
            "{} already exists (use --force to overwrite)",
            ctx.display(&ctx.config_path)
        )));
    }
    let runtimes = mcp_runtimes(&args.mcp)?;
    p.intro("guard init").map_err(stdout_err)?;
    if !ctx.in_git {
        p.warning("not a git repository: --affected, the hook and the ticket pattern need git")
            .map_err(stdout_err)?;
    }

    p.spinner_start("Detecting your app").map_err(stdout_err)?;
    let (report, mut plan, io_notes, area_warnings) = build_plan(ctx);
    p.spinner_stop("Detected your app").map_err(stdout_err)?;
    for message in area_warnings {
        p.warning(&message).map_err(stdout_err)?;
    }
    p.plan(&plan::plan_lines(&report, &plan, &io_notes))
        .map_err(stdout_err)?;

    let e2e_label = format!("{}/", ctx.display(&ctx.e2e_dir));
    let scaffold_exists = ctx.e2e_dir.exists();
    let extras = extras(ctx, args)?;

    if args.dry_run {
        let rendered = plan::render(&plan, runner_mode(args));
        return dry_run(ctx, args, p, &rendered, &runtimes, &extras, scaffold_exists);
    }

    let chosen = choose_runner(args, p);
    let runner = asked(p, chosen, NOTHING_WRITTEN)?;
    if p.interactive() {
        let confirmed = plan::confirm_with(&mut plan, runner, p);
        asked(p, confirmed, NOTHING_WRITTEN)?;
    }
    let toml = plan::render(&plan, runner);
    if let Some(message) = plan::invalid(&plan, runner) {
        return Err(CliError::usage(format!(
            "the generated {} does not validate: {message}",
            ctx.display(&ctx.config_path)
        )));
    }
    if p.styled() {
        let write = p.confirm("Write target.toml and the Playwright scaffold?", true);
        if !asked(p, write, NOTHING_WRITTEN)? {
            p.outro_cancel("init: nothing written")
                .map_err(stdout_err)?;
            return Ok(EXIT_OK);
        }
    }

    // A conflicting MCP config (e.g. a .mcp.json that is not an object) is a
    // usage error: find it before anything is written.
    if !runtimes.is_empty() {
        sync_mcp(&ctx.root, &runtimes, true).map_err(|e| CliError::usage(format!("--mcp: {e}")))?;
    }

    p.spinner_start("Writing files").map_err(stdout_err)?;
    let files = Files {
        existed,
        scaffold_exists,
        e2e_label: &e2e_label,
        toml: &toml,
        runtimes: &runtimes,
        extras: &extras,
    };
    if let Err(e) = write_files(ctx, args, p, &files) {
        let _ = p.spinner_fail("Writing files failed");
        return Err(e);
    }
    p.spinner_stop("Files written").map_err(stdout_err)?;
    if let Some(message) = ci_warning(args.ci, runner) {
        p.warning(&message).map_err(stdout_err)?;
    }

    for line in summary_lines(&plan, runner) {
        p.line(&line).map_err(stdout_err)?;
    }

    let install = if args.install {
        true
    } else if p.interactive() {
        let answer = p.confirm("Install dependencies now?", false);
        asked(p, answer, "init cancelled, dependencies not installed")?
    } else {
        false
    };
    if install {
        install_deps(ctx, runner_exec)?;
    }
    p.next_steps(&next_steps(ctx, install, runner))
        .map_err(stdout_err)?;
    Ok(EXIT_OK)
}

/// The `detected ...` / `proposed areas` lines after the files are written.
fn summary_lines(plan: &InitPlan, runner: RunnerMode) -> Vec<String> {
    let or_none = |v: Option<&str>| v.unwrap_or("none").to_string();
    let languages = if plan.languages.is_empty() {
        "none".to_string()
    } else {
        plan.languages.join(", ")
    };
    let mut lines = vec![
        format!("detected languages: {languages}"),
        format!(
            "detected code host: {}",
            or_none(plan.code_host.map(|c| c.as_str()))
        ),
    ];
    let active_start = plan
        .start
        .as_deref()
        .filter(|_| runner == RunnerMode::Guard);
    lines.push(format!("detected start command: {}", or_none(active_start)));
    if runner == RunnerMode::SelfRun {
        lines.push(
            "note: guard does not start the app (--runner self); detected commands are commented in target.toml"
                .into(),
        );
    }
    if let Some(project) = &plan.ticket_project {
        lines.push(format!("detected ticket pattern: {project}-\\d+"));
    }
    let areas: Vec<&str> = plan.areas.iter().map(|a| a.key.as_str()).collect();
    lines.push(format!(
        "proposed areas: {} (edit their descriptions)",
        areas.join(", ")
    ));
    lines
}

/// The two install commands, in order.
fn install_commands() -> [(OsString, &'static [&'static str]); 2] {
    [
        (npm_program(), &["install"]),
        (
            crate::playwright::npx_program(),
            &["playwright", "install", "chromium"],
        ),
    ]
}

fn install_deps(ctx: &Context, runner: &dyn Runner) -> CliResult<()> {
    for (program, args) in install_commands() {
        let shown = format!("{} {}", program.to_string_lossy(), args.join(" "));
        let where_ = ctx.display(&ctx.e2e_dir);
        match runner.run(&program, args, &ctx.e2e_dir) {
            Ok(Some(0)) => {}
            Ok(code) => {
                return Err(CliError::env(format!(
                    "{shown} failed in {where_} (exit {})",
                    code.map_or("signal".to_string(), |c| c.to_string())
                )));
            }
            Err(e) => return Err(CliError::env(format!("cannot run {shown}: {e}"))),
        }
    }
    Ok(())
}

fn next_steps(ctx: &Context, installed: bool, runner: RunnerMode) -> Vec<String> {
    let e2e = ctx.display(&ctx.e2e_dir);
    let mut steps = vec![format!(
        "review {} (area descriptions start with TODO)",
        ctx.display(&ctx.config_path)
    )];
    if !installed {
        steps.push(format!(
            "cd {e2e} && npm install && npx playwright install chromium   (or guard init --install)"
        ));
    }
    steps.push(match runner {
        RunnerMode::Guard => "guard up && guard status".into(),
        RunnerMode::SelfRun => {
            "start your app, then guard up && guard status   (guard up only waits for it)".into()
        }
    });
    steps.push("guard test --all".into());
    steps.push("guard new journey <area>.<name> --title \"...\"".into());
    steps
}

fn dry_run(
    ctx: &Context,
    args: &InitArgs,
    p: &mut dyn Prompter,
    rendered: &str,
    runtimes: &BTreeSet<RuntimeKind>,
    extras: &[Extra],
    scaffold_exists: bool,
) -> CliResult<u8> {
    let shown = ctx.display(&ctx.config_path);
    p.text(&format!("would write {shown}:\n{rendered}"))
        .map_err(stdout_err)?;
    let e2e_label = format!("{}/", ctx.display(&ctx.e2e_dir));
    if scaffold_exists {
        p.line(&format!("note: {e2e_label} exists, scaffold skipped"))
            .map_err(stdout_err)?;
    } else {
        p.line(&format!(
            "would create {e2e_label} ({} files)",
            scaffold::E2E_TEMPLATE.len()
        ))
        .map_err(stdout_err)?;
    }
    if !runtimes.is_empty() {
        let report = sync_mcp(&ctx.root, runtimes, true)
            .map_err(|e| CliError::usage(format!("--mcp: {e}")))?;
        for path in &report.written {
            p.line(&format!("would write {path}")).map_err(stdout_err)?;
        }
    }
    for extra in extras {
        let shown = ctx.display(&extra.path);
        if extra.path.exists() && !args.force {
            p.line(&format!(
                "note: {shown} exists, kept (use --force to overwrite)"
            ))
            .map_err(stdout_err)?;
        } else {
            p.line(&format!("would create {shown}"))
                .map_err(stdout_err)?;
        }
    }
    if args.install {
        p.line(&format!(
            "would run npm install && npx playwright install chromium in {}",
            ctx.display(&ctx.e2e_dir)
        ))
        .map_err(stdout_err)?;
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::prompt::scripted::{Scripted, Step};
    use super::*;
    use crate::cli::RunnerMode;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeRunner {
        calls: RefCell<Vec<(String, Vec<String>, PathBuf)>>,
        exit: Option<i32>,
    }

    impl Runner for FakeRunner {
        fn run(&self, program: &OsString, args: &[&str], cwd: &Path) -> io::Result<Option<i32>> {
            self.calls.borrow_mut().push((
                program.to_string_lossy().into_owned(),
                args.iter().map(|a| a.to_string()).collect(),
                cwd.to_path_buf(),
            ));
            Ok(self.exit)
        }
    }

    fn args() -> InitArgs {
        InitArgs {
            yes: false,
            dry_run: false,
            force: false,
            mcp: vec![],
            hook: false,
            ci: None,
            action_ref: crate::cli::DEFAULT_ACTION_REF.into(),
            install: false,
            runner: None,
        }
    }

    #[test]
    fn self_mode_asks_only_the_urls_and_writes_no_start_config() {
        let (_dir, ctx) = app_dir();
        let runner = FakeRunner::default();
        let mut a = args();
        a.runner = Some(RunnerMode::SelfRun);
        // base_url, ready_path, then "install now?" = no.
        let (result, out) = run_init(&ctx, &a, "http://localhost:5200\n\nn\n", &runner);
        assert_eq!(result.unwrap(), EXIT_OK, "{out}");
        assert!(!out.contains("start command ["), "{out}");
        assert!(out.contains("base_url [http://localhost:5173]: "), "{out}");
        let config = parse(&ctx);
        assert!(config.is_self_mode());
        assert_eq!(config.app.base_url, "http://localhost:5200");
        let text = fs::read_to_string(&ctx.config_path).unwrap();
        assert!(text.contains("\n# start = \"npm run dev\"\n"), "{text}");
        assert!(
            out.contains(
                "detected start command: none\nnote: guard does not start the app (--runner self); detected commands are commented in target.toml\n"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "  3. start your app, then guard up && guard status   (guard up only waits for it)\n"
            ),
            "{out}"
        );
        assert!(runner.calls.borrow().is_empty());
    }

    #[test]
    fn the_runner_defaults_to_guard() {
        assert_eq!(runner_mode(&args()), RunnerMode::Guard);
        let mut a = args();
        a.runner = Some(RunnerMode::SelfRun);
        assert_eq!(runner_mode(&a), RunnerMode::SelfRun);
    }

    #[test]
    fn ci_with_runner_self_warns_that_ci_needs_start_commands() {
        assert_eq!(ci_warning(Some(CiHost::Github), RunnerMode::Guard), None);
        assert_eq!(ci_warning(None, RunnerMode::SelfRun), None);
        assert_eq!(
            ci_warning(Some(CiHost::Gitlab), RunnerMode::SelfRun).as_deref(),
            Some(
                "CI needs start commands: guard does not start the app with --runner self; uncomment the detected commands in target.toml before CI runs"
            )
        );
    }

    fn app_dir() -> (tempfile::TempDir, Context) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("shop");
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"6"}}"#,
        )
        .unwrap();
        let ctx = Context {
            in_git: false,
            config_path: root.join("target.toml"),
            e2e_dir: root.join("e2e-playwright"),
            root,
        };
        (dir, ctx)
    }

    fn run_init(
        ctx: &Context,
        args: &InitArgs,
        answers: &str,
        runner: &FakeRunner,
    ) -> (CliResult<u8>, String) {
        let mut input = answers.as_bytes();
        let mut out = Vec::new();
        let result = run_with(
            ctx,
            args,
            &mut Io {
                input: &mut input,
                out: &mut out,
                interactive: true,
                runner,
            },
        );
        (result, String::from_utf8(out).unwrap())
    }

    #[test]
    fn interactive_answers_shape_the_written_target() {
        let (_dir, ctx) = app_dir();
        let runner = FakeRunner {
            exit: Some(0),
            ..Default::default()
        };
        // start, base_url, ready_path, then "install now?" = no.
        let (result, out) = run_init(
            &ctx,
            &args(),
            "npm run dev -- --port 5200\nhttp://localhost:5200\n\nn\n",
            &runner,
        );
        assert_eq!(result.unwrap(), EXIT_OK, "{out}");
        assert!(out.contains("start command [npm run dev]: "), "{out}");
        assert!(out.contains("Install dependencies now? [y/N]: "), "{out}");
        let config = parse(&ctx);
        assert_eq!(
            config.app.start.as_deref(),
            Some("npm run dev -- --port 5200")
        );
        assert_eq!(config.app.base_url, "http://localhost:5200");
        assert!(runner.calls.borrow().is_empty());
    }

    fn parse(ctx: &Context) -> guard_core::config::TargetConfig {
        guard_core::config::parse_target(&fs::read_to_string(&ctx.config_path).unwrap()).unwrap()
    }

    #[test]
    fn answering_yes_installs_with_npm_then_playwright_in_the_e2e_dir() {
        let (_dir, ctx) = app_dir();
        let runner = FakeRunner {
            exit: Some(0),
            ..Default::default()
        };
        let (result, out) = run_init(&ctx, &args(), "\n\n\ny\n", &runner);
        assert_eq!(result.unwrap(), EXIT_OK, "{out}");
        let calls = runner.calls.borrow();
        let commands: Vec<Vec<String>> = calls.iter().map(|(_, a, _)| a.clone()).collect();
        assert_eq!(
            commands,
            vec![
                vec!["install".to_string()],
                vec!["playwright".into(), "install".into(), "chromium".into()]
            ]
        );
        assert!(calls[0].0.starts_with("npm"), "{}", calls[0].0);
        assert!(calls.iter().all(|(_, _, cwd)| cwd == &ctx.e2e_dir));
        assert!(!out.contains("(or guard init --install)"), "{out}");
    }

    fn run_styled(ctx: &Context, args: &InitArgs, steps: Vec<Step>) -> (CliResult<u8>, Scripted) {
        let mut prompter = Scripted::new(steps);
        let runner = FakeRunner {
            exit: Some(0),
            ..Default::default()
        };
        let result = run_flow(ctx, args, &mut prompter, &runner);
        (result, prompter)
    }

    const WRITE: &str = "confirm: Write target.toml and the Playwright scaffold? default=true";

    #[test]
    fn styled_init_asks_the_runner_first_and_self_asks_only_the_urls() {
        let (_dir, ctx) = app_dir();
        let steps = vec![
            Step::Select(0),
            Step::Input("http://localhost:5200"),
            Step::Input("/"),
            Step::Confirm(true),
            Step::Confirm(false),
        ];
        let (result, p) = run_styled(&ctx, &args(), steps);
        assert_eq!(result.unwrap(), EXIT_OK, "{:#?}", p.shown);
        let flow: Vec<&String> = p
            .shown
            .iter()
            .filter(|s| !s.starts_with("warning: "))
            .collect();
        assert_eq!(flow[0], "intro: guard init");
        assert_eq!(flow[1], "spinner: Detecting your app");
        assert!(flow[2].starts_with("spinner done: "), "{flow:#?}");
        assert!(flow[3].starts_with("note: "), "{flow:#?}");
        assert!(
            flow[3].contains("base_url   http://localhost:5173"),
            "{flow:#?}"
        );
        let asked: Vec<&String> = p
            .shown
            .iter()
            .filter(|s| {
                s.starts_with("select: ") || s.starts_with("input: ") || s.starts_with("confirm: ")
            })
            .collect();
        assert_eq!(
            asked,
            vec![
                "select: How will you run the app locally? [\"I start it myself (guard only needs the URLs)\", \"guard starts it (start commands, also needed for CI)\"] default=1",
                "input: base_url [http://localhost:5173]",
                "input: ready_path [/]",
                WRITE,
                "confirm: Install dependencies now? default=false",
            ],
            "{:#?}",
            p.shown
        );
        assert!(p.shown.contains(&"spinner: Writing files".to_string()));
        let config = parse(&ctx);
        assert!(config.is_self_mode());
        assert_eq!(config.app.base_url, "http://localhost:5200");
        let outro = p.shown.last().unwrap();
        assert!(outro.starts_with("outro: "), "{outro}");
        assert!(outro.contains("start your app, then guard up"), "{outro}");
    }

    #[test]
    fn styled_init_with_runner_guard_keeps_the_per_item_flow() {
        let (_dir, ctx) = app_dir();
        let steps = vec![
            Step::Select(1),
            Step::Input("npm run dev -- --port 5200"),
            Step::Input(""),
            Step::Input("-"),
            Step::Input(""),
            Step::Confirm(true),
            Step::Confirm(false),
        ];
        let (result, p) = run_styled(&ctx, &args(), steps);
        assert_eq!(result.unwrap(), EXIT_OK, "{:#?}", p.shown);
        // `-` on a required item is refused and asked again.
        assert!(
            p.shown
                .contains(&"error: ready_path is required".to_string()),
            "{:#?}",
            p.shown
        );
        let config = parse(&ctx);
        assert_eq!(
            config.app.start.as_deref(),
            Some("npm run dev -- --port 5200")
        );
    }

    #[test]
    fn the_runner_flag_skips_the_select() {
        let (_dir, ctx) = app_dir();
        let mut a = args();
        a.runner = Some(RunnerMode::SelfRun);
        let steps = vec![
            Step::Input(""),
            Step::Input(""),
            Step::Confirm(true),
            Step::Confirm(false),
        ];
        let (result, p) = run_styled(&ctx, &a, steps);
        assert_eq!(result.unwrap(), EXIT_OK, "{:#?}", p.shown);
        assert!(p.shown("select: ").is_empty());
        assert!(parse(&ctx).is_self_mode());
    }

    fn assert_cancelled_before_writing(steps: Vec<Step>) {
        let (_dir, ctx) = app_dir();
        let (result, p) = run_styled(&ctx, &args(), steps);
        let error = result.unwrap_err();
        assert_eq!(error.code, crate::error::EXIT_INTERRUPTED);
        assert!(error.messages.is_empty(), "{:?}", error.messages);
        assert_eq!(
            p.shown.last().unwrap(),
            "cancel: init cancelled, nothing written"
        );
        assert!(!ctx.config_path.exists());
        assert!(!ctx.e2e_dir.exists());
    }

    #[test]
    fn ctrl_c_at_the_runner_select_writes_nothing_and_exits_130() {
        assert_cancelled_before_writing(vec![Step::Interrupt]);
    }

    #[test]
    fn ctrl_c_at_an_item_writes_nothing_and_exits_130() {
        assert_cancelled_before_writing(vec![Step::Select(1), Step::Input(""), Step::Interrupt]);
    }

    #[test]
    fn ctrl_c_at_the_final_confirm_writes_nothing_and_exits_130() {
        assert_cancelled_before_writing(vec![
            Step::Select(0),
            Step::Input(""),
            Step::Input(""),
            Step::Interrupt,
        ]);
    }

    #[test]
    fn declining_the_final_confirm_writes_nothing_and_exits_0() {
        let (_dir, ctx) = app_dir();
        let steps = vec![
            Step::Select(0),
            Step::Input(""),
            Step::Input(""),
            Step::Confirm(false),
        ];
        let (result, p) = run_styled(&ctx, &args(), steps);
        assert_eq!(result.unwrap(), EXIT_OK);
        assert_eq!(p.shown.last().unwrap(), "cancel: init: nothing written");
        assert!(!ctx.config_path.exists());
        assert!(!ctx.e2e_dir.exists());
    }

    #[test]
    fn a_prompt_that_cannot_read_the_terminal_says_so_and_how_to_go_plain() {
        let mut p = Scripted::default();
        let failed: io::Result<()> = Err(io::ErrorKind::NotConnected.into());
        let error = asked(&mut p, failed, NOTHING_WRITTEN).unwrap_err();
        assert_eq!(error.code, 3);
        assert_eq!(error.messages.len(), 1);
        assert!(
            error.messages[0].starts_with("cannot read the answer from the terminal: "),
            "{:?}",
            error.messages
        );
        assert_eq!(
            error.hints,
            ["set NO_COLOR=1 or pass --yes to use the plain prompts"]
        );
        assert!(p.shown.is_empty(), "{:?}", p.shown);
    }

    #[test]
    fn a_failed_write_ends_the_spinner_after_the_lines_already_written() {
        let (_dir, ctx) = app_dir();
        // `.github` is a file, so the workflow cannot be written.
        fs::write(ctx.root.join(".github"), "").unwrap();
        let mut a = args();
        a.runner = Some(RunnerMode::SelfRun);
        a.ci = Some(CiHost::Github);
        let steps = vec![Step::Input(""), Step::Input(""), Step::Confirm(true)];
        let (result, p) = run_styled(&ctx, &a, steps);
        assert_eq!(result.unwrap_err().code, 3);
        let tail: Vec<&str> = p
            .shown
            .iter()
            .rev()
            .take(3)
            .rev()
            .map(String::as_str)
            .collect();
        assert_eq!(
            tail,
            [
                "line: created target.toml",
                "line: created e2e-playwright/ (20 files)",
                "spinner failed: Writing files failed",
            ],
            "{:#?}",
            p.shown
        );
    }

    #[test]
    fn ctrl_c_at_the_install_prompt_keeps_the_files_and_exits_130() {
        let (_dir, ctx) = app_dir();
        let steps = vec![
            Step::Select(0),
            Step::Input(""),
            Step::Input(""),
            Step::Confirm(true),
            Step::Interrupt,
        ];
        let (result, p) = run_styled(&ctx, &args(), steps);
        assert_eq!(result.unwrap_err().code, crate::error::EXIT_INTERRUPTED);
        assert_eq!(
            p.shown.last().unwrap(),
            "cancel: init cancelled, dependencies not installed"
        );
        assert!(ctx.config_path.is_file());
        assert!(ctx.e2e_dir.is_dir());
    }

    #[test]
    fn a_failing_install_step_is_an_environment_error() {
        let (_dir, ctx) = app_dir();
        let runner = FakeRunner {
            exit: Some(1),
            ..Default::default()
        };
        let mut a = args();
        a.install = true;
        let (result, _) = run_init(&ctx, &a, "", &runner);
        let error = result.unwrap_err();
        assert_eq!(error.code, crate::error::EXIT_ENV);
        assert!(
            error.messages[0].contains("failed in e2e-playwright (exit 1)"),
            "{:?}",
            error.messages
        );
        assert_eq!(
            runner.calls.borrow().len(),
            1,
            "playwright is not installed after npm failed"
        );
        assert!(ctx.config_path.is_file());
    }
}
