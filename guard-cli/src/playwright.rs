//! Launching `npx playwright ...`, locally or inside the official Docker image.

use crate::context::slash;
use crate::error::{CliError, CliResult};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

pub const DOCKER_IMAGE: &str = "mcr.microsoft.com/playwright";

/// The npx executable. `GUARD_NPX` overrides it (for example `pnpm`, or a stub in tests).
pub fn npx_program() -> OsString {
    match std::env::var_os("GUARD_NPX") {
        Some(program) if !program.is_empty() => program,
        _ if cfg!(windows) => "npx.cmd".into(),
        _ => "npx".into(),
    }
}

/// Where Playwright runs.
pub enum Runtime<'a> {
    Local,
    /// `docker run` with the repo root mounted at `/work`.
    Docker {
        root: &'a Path,
        version: &'a str,
    },
}

/// Runs `npx <args>` in `e2e_dir` with `env` set on top of guard's environment.
/// Returns the exit code (`None` when terminated by a signal).
pub fn run(
    e2e_dir: &Path,
    args: &[String],
    env: &[(String, String)],
    runtime: &Runtime<'_>,
) -> CliResult<Option<i32>> {
    let mut cmd = match runtime {
        Runtime::Local => {
            let mut cmd = Command::new(npx_program());
            cmd.args(args)
                .current_dir(e2e_dir)
                .envs(env.iter().map(|(k, v)| (k, v)));
            cmd
        }
        Runtime::Docker { root, version } => {
            let workdir = docker_workdir(root, e2e_dir)?;
            let mut cmd = Command::new("docker");
            cmd.args(docker_args(root, &workdir, version, env, args))
                .current_dir(root)
                .envs(env.iter().map(|(k, v)| (k, v)));
            cmd
        }
    };
    let status = cmd.status().map_err(|e| {
        let program = match runtime {
            Runtime::Local => npx_program().to_string_lossy().into_owned(),
            Runtime::Docker { .. } => "docker".into(),
        };
        CliError::env(format!("cannot run {program}: {e}"))
            .with_hint("install Node.js (npx) and run guard doctor")
    })?;
    Ok(status.code())
}

/// Spawns `cmd` and waits for it until `deadline`; past it the process tree
/// is terminated and the result is `Err("timed out")`.
pub fn wait_with_deadline(
    mut cmd: Command,
    deadline: std::time::Instant,
) -> Result<Option<i32>, String> {
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", cmd.get_program().to_string_lossy()))?;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status.code());
        }
        if std::time::Instant::now() >= deadline {
            crate::process::kill_tree(child.id());
            let _ = child.wait();
            return Err("timed out".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// `/work/<e2e dir relative to the root>`; the Playwright dir must live in the repo.
pub fn docker_workdir(root: &Path, e2e_dir: &Path) -> CliResult<String> {
    let rel = e2e_dir
        .strip_prefix(root)
        .map_err(|_| CliError::usage("--docker needs the Playwright dir inside the repository"))?;
    let rel = slash(rel);
    Ok(if rel.is_empty() {
        "/work".into()
    } else {
        format!("/work/{rel}")
    })
}

pub fn docker_args(
    root: &Path,
    workdir: &str,
    version: &str,
    env: &[(String, String)],
    args: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--ipc=host".into(),
        "--add-host=host.docker.internal:host-gateway".into(),
        "-v".into(),
        format!("{}:/work", root.display()),
        "-w".into(),
        workdir.into(),
    ];
    // `-e KEY` without a value: docker copies it from its own environment, so
    // secrets such as GUARD_TEST_TOKEN never show up in the process list.
    for (key, _) in env {
        out.push("-e".into());
        out.push(key.clone());
    }
    out.push(format!("{DOCKER_IMAGE}:v{version}-noble"));
    out.push("npx".into());
    out.extend(args.iter().cloned());
    out
}

/// The `@playwright/test` version: installed one first, else the exact pin in package.json.
pub fn playwright_version(e2e_dir: &Path) -> Option<String> {
    let installed = e2e_dir.join("node_modules/@playwright/test/package.json");
    if let Some(version) = std::fs::read_to_string(installed)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["version"].as_str().map(str::to_string))
    {
        return Some(version);
    }
    let manifest = std::fs::read_to_string(e2e_dir.join("package.json")).ok()?;
    version_from_package_json(&manifest)
}

/// Reads `@playwright/test` from dependencies or devDependencies, without `^`, `~`, `=` or `v`.
pub fn version_from_package_json(text: &str) -> Option<String> {
    let pkg: serde_json::Value = serde_json::from_str(text).ok()?;
    let raw = ["devDependencies", "dependencies"]
        .iter()
        .find_map(|k| pkg[k]["@playwright/test"].as_str())?;
    let version = raw.trim().trim_start_matches(['^', '~', '=', 'v']);
    version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        .then(|| version.to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn reads_the_version_pin() {
        let pkg = r#"{"devDependencies": {"@playwright/test": "1.64.1"}}"#;
        assert_eq!(version_from_package_json(pkg), Some("1.64.1".into()));
        let pkg = r#"{"dependencies": {"@playwright/test": "^1.50.0"}}"#;
        assert_eq!(version_from_package_json(pkg), Some("1.50.0".into()));
        assert_eq!(
            version_from_package_json(r#"{"devDependencies": {"@playwright/test": ">=1 <2"}}"#),
            None
        );
        assert_eq!(version_from_package_json("{}"), None);
    }

    #[test]
    fn docker_args_mount_the_root_and_pass_every_variable_by_name_only() {
        let root = PathBuf::from("/repo");
        let env = vec![
            ("E2E_TAGS".to_string(), "@smoke".to_string()),
            ("GUARD_TEST_TOKEN".to_string(), "s3cret".to_string()),
        ];
        let args = docker_args(
            &root,
            "/work/e2e-playwright",
            "1.64.1",
            &env,
            &["playwright".into(), "test".into()],
        );
        assert_eq!(
            args,
            vec![
                "run",
                "--rm",
                "--ipc=host",
                "--add-host=host.docker.internal:host-gateway",
                "-v",
                "/repo:/work",
                "-w",
                "/work/e2e-playwright",
                "-e",
                "E2E_TAGS",
                "-e",
                "GUARD_TEST_TOKEN",
                "mcr.microsoft.com/playwright:v1.64.1-noble",
                "npx",
                "playwright",
                "test",
            ]
        );
        // Values (the test token among them) never reach the docker argv.
        assert!(args.iter().all(|a| !a.contains("s3cret")));
    }

    #[test]
    fn deadlines_end_long_runs() {
        let quick = crate::process::shell("exit 3");
        let far = std::time::Instant::now() + std::time::Duration::from_secs(30);
        assert_eq!(wait_with_deadline(quick, far), Ok(Some(3)));
        let slow = if cfg!(windows) {
            crate::process::shell("ping -n 30 127.0.0.1 >NUL")
        } else {
            crate::process::shell("sleep 30")
        };
        let soon = std::time::Instant::now() + std::time::Duration::from_millis(300);
        assert_eq!(wait_with_deadline(slow, soon), Err("timed out".to_string()));
    }

    #[test]
    fn docker_workdir_is_relative_to_the_root() {
        let root = PathBuf::from("/repo");
        assert_eq!(
            docker_workdir(&root, &root.join("e2e-playwright")).unwrap(),
            "/work/e2e-playwright"
        );
        assert!(docker_workdir(&root, Path::new("/elsewhere")).is_err());
    }
}
