//! `guard init` end to end: detection, plan, scaffold and the optional extras
//! against temporary repositories (cli.md "guard init").

use assert_cmd::Command;
use guard_core::config::parse_target;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use tempfile::TempDir;

/// A credentials file that never exists: the developer's own
/// (`guard connect`) must not leak into tests.
const NO_CREDENTIALS: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/no-credentials.toml");

fn guard(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("guard").unwrap();
    cmd.current_dir(dir)
        .env("NO_COLOR", "1")
        .env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS);
    for var in [
        "GUARD_NPX",
        "GUARD_NPM",
        "RUN_ID",
        "GITHUB_TOKEN",
        "GITLAB_TOKEN",
        "JIRA_API_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn git(dir: &Path, args: &[&str]) {
    let out = StdCommand::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .replace("\r\n", "\n")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

/// Every file under `root` with its bytes, by forward-slash relative path.
fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A single Next.js app at the root of a git repository named `shop`.
struct Repo {
    _dir: TempDir,
    root: std::path::PathBuf,
}

impl Repo {
    fn path(&self) -> &Path {
        &self.root
    }
}

fn next_app() -> Repo {
    let dir = TempDir::new().unwrap();
    let root_buf = dir.path().join("shop");
    fs::create_dir_all(&root_buf).unwrap();
    let root = root_buf.as_path();
    git(root, &["init", "-q", "-b", "main"]);
    write(
        root,
        "package.json",
        r#"{"name":"shop","scripts":{"dev":"next dev -p 3100","build":"next build"},"dependencies":{"next":"16.0.0","react":"19.2.0"}}"#,
    );
    write(root, "package-lock.json", "{}\n");
    write(
        root,
        "app/page.tsx",
        "export default function Page() { return null; }\n",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "init"]);
    Repo {
        _dir: dir,
        root: root_buf,
    }
}

/// A reduced copy of a multi-service SaaS layout, at a root that is not a git repository.
fn multi_service() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "docker-compose.yml",
        "services:\n  postgres:\n    image: postgres:16-alpine\n    ports:\n      - \"5433:5432\"\n  redis:\n    image: redis:7-alpine\n    ports:\n      - \"6380:6379\"\n  laravel:\n    build:\n      context: ./billing\n  nginx:\n    image: nginx:alpine\n    ports:\n      - \"8500:80\"\n    depends_on:\n      - laravel\n  mailpit:\n    image: axllent/mailpit\n",
    );
    // A dotenv file that must never be read: its PORT would win otherwise.
    write(root, "api/.env", "PORT=9999\n");
    write(
        root,
        "api/package.json",
        r#"{"name":"api","scripts":{"dev":"bun run --watch src/index.ts"},"dependencies":{"elysia":"latest"}}"#,
    );
    write(root, "api/bun.lock", "{}\n");
    write(root, "api/src/index.ts", "import './server';\n");
    write(
        root,
        "api/src/server.ts",
        "import { Elysia } from 'elysia';\nnew Elysia()\n  .get('/health', () => 'ok')\n  .listen(Number(process.env.PORT || 3001));\n",
    );
    write(
        root,
        "api/docker-compose.dokploy.yml",
        "services:\n  api:\n    build: .\n",
    );
    write(
        root,
        "web/package.json",
        r#"{"name":"web","scripts":{"dev":"next dev"},"dependencies":{"next":"16.0.0"}}"#,
    );
    write(root, "web/bun.lock", "{}\n");
    write(root, "web/package-lock.json", "{}\n");
    write(root, "web/app/page.tsx", "export default 1;\n");
    write(root, "billing/artisan", "#!/usr/bin/env php\n");
    write(
        root,
        "billing/composer.json",
        r#"{"require":{"laravel/framework":"^12.0"}}"#,
    );
    write(
        root,
        "billing/routes/api.php",
        "<?php\nRoute::get('/health', fn () => 'ok');\n",
    );
    dir
}

const SCAFFOLD: &[&str] = &[
    "e2e-playwright/.gitignore",
    "e2e-playwright/.npmrc",
    "e2e-playwright/AGENTS.md",
    "e2e-playwright/catalog/journeys.json",
    "e2e-playwright/eslint.config.mjs",
    "e2e-playwright/package.json",
    "e2e-playwright/playwright.config.ts",
    "e2e-playwright/scripts/catalog-lint.ts",
    "e2e-playwright/scripts/lib/catalog-lint-run.ts",
    "e2e-playwright/scripts/lib/catalog-rules.ts",
    "e2e-playwright/src/config/env.ts",
    "e2e-playwright/src/fixtures/auth.ts",
    "e2e-playwright/src/fixtures/blocked-domains.ts",
    "e2e-playwright/src/fixtures/data.ts",
    "e2e-playwright/src/fixtures/index.ts",
    "e2e-playwright/src/fixtures/namespace.ts",
    "e2e-playwright/src/fixtures/seeded-random.ts",
    "e2e-playwright/tests/setup/auth.setup.ts",
    "e2e-playwright/tests/smoke/home.spec.ts",
    "e2e-playwright/tsconfig.json",
];

#[test]
fn a_single_next_app_gets_its_dev_command_as_the_app_start_and_the_scaffold() {
    let dir = next_app();
    let root = dir.path();
    let output = guard(root).args(["init"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);

    assert!(out.starts_with("plan:\n"), "{out}");
    assert!(
        out.contains("  service    shop  cwd=.  next/npm  start=\"npm run dev\"  ready=http://localhost:3100/  (high)\n"),
        "{out}"
    );
    assert!(
        out.contains("package.json scripts.dev: next dev -p 3100"),
        "{out}"
    );
    assert!(out.contains("created target.toml\n"), "{out}");
    assert!(
        out.contains(&format!(
            "created e2e-playwright/ ({} files)\n",
            SCAFFOLD.len()
        )),
        "{out}"
    );
    assert!(
        out.contains("detected start command: npm run dev\n"),
        "{out}"
    );
    assert!(out.contains("next steps:\n"), "{out}");
    assert!(
        out.contains("  2. cd e2e-playwright && npm install && npx playwright install chromium   (or guard init --install)\n"),
        "{out}"
    );

    let target = read(root, "target.toml");
    let config = parse_target(&target).unwrap_or_else(|e| panic!("{e}\n{target}"));
    assert_eq!(config.app.name, config.app.name.to_lowercase());
    assert_eq!(config.app.start.as_deref(), Some("npm run dev"));
    assert_eq!(config.app.stop, None);
    assert_eq!(config.app.base_url, "http://localhost:3100");
    assert_eq!(config.app.ready_path, "/");
    assert!(config.services.is_empty());
    assert!(!target.contains("[[services]]"), "{target}");

    let written: Vec<String> = tree(root)
        .into_keys()
        .filter(|p| p.starts_with("e2e-playwright/"))
        .collect();
    assert_eq!(written, SCAFFOLD);
    assert_eq!(
        read(root, "e2e-playwright/catalog/journeys.json"),
        "{\n  \"version\": 1,\n  \"journeys\": []\n}\n"
    );
    assert!(read(root, "e2e-playwright/tests/setup/auth.setup.ts").contains("GUARD_LOGIN_PATH"));
    assert!(read(root, "e2e-playwright/src/fixtures/index.ts").contains("GUARD_SEED_PATH"));
}

#[test]
fn a_multi_service_root_without_git_gets_compose_infra_and_services() {
    let dir = multi_service();
    let root = dir.path();
    let output = guard(root).args(["init", "--yes"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    let err = stderr(&output);
    assert!(
        err.contains(
            "warning: not a git repository: --affected, the hook and the ticket pattern need git"
        ),
        "{err}"
    );
    assert!(
        out.contains(
            "  infra      docker compose -f docker-compose.yml up -d --wait postgres redis mailpit"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "note: ignored api/docker-compose.dokploy.yml (production compose file (dokploy))"
        ),
        "{out}"
    );
    assert!(
        out.contains("note: web: bun.lock and package-lock.json; using bun"),
        "{out}"
    );
    assert!(
        out.contains(
            "note: billing is also available in compose (laravel); using the native dev command"
        ),
        "{out}"
    );

    let target = read(root, "target.toml");
    let config = parse_target(&target).unwrap_or_else(|e| panic!("{e}\n{target}"));
    assert_eq!(
        config.app.start.as_deref(),
        Some("docker compose -f docker-compose.yml up -d --wait postgres redis mailpit")
    );
    assert_eq!(
        config.app.stop.as_deref(),
        Some("docker compose -f docker-compose.yml down")
    );
    assert_eq!(config.app.base_url, "http://localhost:3000");
    assert_eq!(config.app.ready_path, "/");
    let services: Vec<(&str, &str, &str, Option<&str>)> = config
        .services
        .iter()
        .map(|s| {
            (
                s.name.as_str(),
                s.cwd.as_str(),
                s.start.as_str(),
                s.ready_url.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        services,
        vec![
            (
                "api",
                "api",
                "bun run dev",
                Some("http://localhost:3001/health")
            ),
            (
                "billing",
                "billing",
                "php artisan serve --port 8000",
                Some("http://localhost:8000/api/health")
            ),
            ("web", "web", "bun run dev", Some("http://localhost:3000/")),
        ]
    );
    assert!(
        !target.contains("9999"),
        "the .env file was read:\n{target}"
    );
    assert!(root.join("e2e-playwright/playwright.config.ts").is_file());
}

#[test]
fn dry_run_prints_the_plan_and_the_target_and_writes_nothing() {
    let dir = multi_service();
    let root = dir.path();
    let before = tree(root);
    let output = guard(root)
        .args([
            "init",
            "--dry-run",
            "--mcp",
            "claude-code",
            "--ci",
            "github",
            "--install",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.starts_with("plan:\n"), "{out}");
    assert!(out.contains("would write target.toml:\n"), "{out}");
    assert!(out.contains("[[services]]\nname = \"api\"\n"), "{out}");
    assert!(
        out.contains(&format!(
            "would create e2e-playwright/ ({} files)\n",
            SCAFFOLD.len()
        )),
        "{out}"
    );
    assert!(out.contains("would write .mcp.json\n"), "{out}");
    assert!(
        out.contains("would create .github/workflows/guard.yml\n"),
        "{out}"
    );
    assert!(
        out.contains(
            "would run npm install && npx playwright install chromium in e2e-playwright\n"
        ),
        "{out}"
    );
    assert!(!out.contains("created "), "{out}");
    assert_eq!(tree(root), before, "dry run changed files");
}

#[test]
fn existing_files_are_kept_without_force() {
    let dir = next_app();
    let root = dir.path();
    write(root, "e2e-playwright/playwright.config.ts", "// mine\n");
    write(root, ".github/workflows/guard.yml", "name: mine\n");
    let output = guard(root)
        .args(["init", "--ci", "github"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("note: e2e-playwright/ exists, scaffold skipped\n"),
        "{out}"
    );
    assert!(
        out.contains("created e2e-playwright/catalog/journeys.json\n"),
        "{out}"
    );
    assert!(
        out.contains("note: .github/workflows/guard.yml exists, kept (use --force to overwrite)\n"),
        "{out}"
    );
    assert_eq!(
        read(root, "e2e-playwright/playwright.config.ts"),
        "// mine\n"
    );
    assert_eq!(read(root, ".github/workflows/guard.yml"), "name: mine\n");
    assert!(!root.join("e2e-playwright/package.json").exists());

    // target.toml now exists: a second init refuses and changes nothing.
    let target = read(root, "target.toml");
    let again = guard(root).args(["init"]).output().unwrap();
    assert_eq!(again.status.code(), Some(2));
    assert!(
        stderr(&again).contains("target.toml already exists"),
        "{}",
        stderr(&again)
    );
    assert_eq!(read(root, "target.toml"), target);

    // --force overwrites target.toml and the CI file, never the existing scaffold.
    let forced = guard(root)
        .args(["init", "--force", "--ci", "github"])
        .output()
        .unwrap();
    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    assert!(stdout(&forced).contains("overwrote target.toml\n"));
    assert!(read(root, ".github/workflows/guard.yml").contains("guard gate finalize"));
    assert_eq!(
        read(root, "e2e-playwright/playwright.config.ts"),
        "// mine\n"
    );
}

#[test]
fn mcp_hook_and_ci_extras_are_written() {
    let dir = next_app();
    let root = dir.path();
    let output = guard(root)
        .args([
            "init",
            "--mcp",
            "claude-code,codex",
            "--hook",
            "--ci",
            "gitlab",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("wrote .mcp.json\n"), "{out}");
    assert!(
        out.contains("wrote .guard/agents/mcp/codex.toml\n"),
        "{out}"
    );
    assert!(out.contains("created .git/hooks/pre-push\n"), "{out}");
    assert!(out.contains("created .gitlab-ci.yml\n"), "{out}");

    let mcp: serde_json::Value = serde_json::from_str(&read(root, ".mcp.json")).unwrap();
    assert_eq!(mcp["mcpServers"]["guard"]["command"], "guard");
    assert!(read(root, ".guard/agents/mcp/codex.toml").contains("[mcp_servers.guard]"));
    assert_eq!(
        read(root, ".git/hooks/pre-push"),
        "#!/bin/sh\nexec guard test\n"
    );
    let gate = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ci/gitlab/templates/gate.yml"),
    )
    .unwrap()
    .replace("\r\n", "\n");
    assert_eq!(read(root, ".gitlab-ci.yml"), gate);
}

#[test]
fn ci_github_uses_the_action_ref() {
    let dir = next_app();
    let root = dir.path();
    guard(root)
        .args([
            "init",
            "--ci",
            "github",
            "--action-ref",
            "acme/guard/ci/github-action@v1",
        ])
        .assert()
        .success();
    let workflow = read(root, ".github/workflows/guard.yml");
    assert_eq!(
        workflow
            .matches("uses: acme/guard/ci/github-action@v1")
            .count(),
        3,
        "{workflow}"
    );
    assert!(!workflow.contains("<action-ref>"), "{workflow}");

    let default = next_app();
    guard(default.path())
        .args(["init", "--ci", "github"])
        .assert()
        .success();
    assert!(
        read(default.path(), ".github/workflows/guard.yml")
            .contains("uses: CarlosBranty/journeygate/ci/github-action@main")
    );
}

#[test]
fn an_unknown_mcp_runtime_is_a_usage_error_and_writes_nothing() {
    let dir = next_app();
    let root = dir.path();
    let before = tree(root);
    let output = guard(root)
        .args(["init", "--mcp", "claude-code,vim"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("unknown --mcp runtime \"vim\""),
        "{}",
        stderr(&output)
    );
    assert_eq!(tree(root), before);
}

#[test]
fn an_invalid_mcp_json_is_a_usage_error_and_writes_nothing() {
    let dir = next_app();
    let root = dir.path();
    std::fs::write(root.join(".mcp.json"), "[]").unwrap();
    let before = tree(root);
    let output = guard(root)
        .args(["init", "--yes", "--mcp", "claude-code"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("not a JSON object"),
        "{}",
        stderr(&output)
    );
    assert_eq!(tree(root), before, "nothing was written");
}

#[test]
fn hook_outside_git_is_skipped_with_a_warning() {
    let dir = multi_service();
    let output = guard(dir.path()).args(["init", "--hook"]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("warning: --hook needs a git repository; skipped"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn a_failing_install_is_exit_3_after_everything_else_was_written() {
    let dir = next_app();
    let root = dir.path();
    let output = guard(root)
        .args(["init", "--install"])
        .env("GUARD_NPM", "guard-test-no-such-npm")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("error: cannot run guard-test-no-such-npm install"),
        "{}",
        stderr(&output)
    );
    assert!(root.join("target.toml").is_file());
    assert!(root.join("e2e-playwright/package.json").is_file());
}

#[test]
fn runner_self_writes_the_urls_and_comments_the_detected_commands() {
    let dir = next_app();
    let root = dir.path();
    let output = guard(root)
        .args(["init", "--runner", "self", "--ci", "github"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.contains("detected start command: none\nnote: guard does not start the app (--runner self); detected commands are commented in target.toml\n"),
        "{out}"
    );
    assert!(
        out.contains(
            "  3. start your app, then guard up && guard status   (guard up only waits for it)\n"
        ),
        "{out}"
    );
    let err = stderr(&output);
    assert!(
        err.contains("warning: CI needs start commands: guard does not start the app with --runner self; uncomment the detected commands in target.toml before CI runs\n"),
        "{err}"
    );
    assert!(root.join(".github/workflows/guard.yml").is_file());

    let target = read(root, "target.toml");
    let config = parse_target(&target).unwrap_or_else(|e| panic!("{e}\n{target}"));
    assert!(config.is_self_mode(), "{target}");
    assert_eq!(config.app.base_url, "http://localhost:3100");
    assert!(
        target.contains("ready_path = \"/\"\n\n# guard does not start the app: you run it yourself (guard init --runner self).\n# Detected commands, uncomment to let guard start it (needed in CI):\n# start = \"npm run dev\"\n"),
        "{target}"
    );
}

#[test]
fn dry_run_with_runner_self_prints_the_self_mode_file() {
    let dir = next_app();
    let root = dir.path();
    let before = tree(root);
    let output = guard(root)
        .args(["init", "--dry-run", "--runner", "self"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(out.contains("\n# start = \"npm run dev\"\n"), "{out}");
    assert!(!out.contains("\nstart = "), "{out}");
    assert_eq!(tree(root), before);
}

#[test]
fn an_unknown_runner_is_a_usage_error_and_writes_nothing() {
    let dir = next_app();
    let root = dir.path();
    let before = tree(root);
    let output = guard(root)
        .args(["init", "--runner", "ci"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert_eq!(tree(root), before);
}
