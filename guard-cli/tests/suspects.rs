//! `guard suspects` end to end against real scratch repositories: an umbrella
//! with a submodule, a catalog and a Playwright report. Read-only throughout.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn git_at(dir: &Path, author: &str, at: u64, args: &[&str]) -> String {
    let name = format!("user.name={author}");
    let date = format!("{at} +0000");
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            &name,
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "core.autocrlf=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn commit(dir: &Path, author: &str, at: u64, message: &str) {
    git_at(dir, author, at, &["add", "-A"]);
    git_at(dir, author, at, &["commit", "-q", "-m", message]);
}

const CATALOG: &str = r#"{
  "version": 1,
  "journeys": [
    {"id": "orders.checkout", "area": "orders", "title": "Checkout pays the order",
     "impact_question": "q", "paths": ["api/src/orders/**", "web/checkout/**"],
     "smoke": false, "quarantined": false},
    {"id": "billing.pdf", "area": "billing", "title": "Invoice PDF",
     "impact_question": "q", "paths": ["web/billing/**"],
     "smoke": false, "quarantined": false}
  ]
}"#;

const SPEC: &str = "import { test } from '@playwright/test';\ntest('pays', async () => {\n  expect(total).toBe(10);\n});\n";

struct Fixture {
    _dirs: Vec<TempDir>,
    root: PathBuf,
}

/// main: web/billing/x.ts, the catalog, the spec (by Quinn) and the
/// submodule `api`. feature: api commits by Bob (two order files, then a
/// users file) and a superproject commit by Cara (one checkout file).
fn fixture() -> Fixture {
    fixture_with(false)
}

/// With `shipped`, main also records an api pointer past api's own
/// `origin/main`: Dan's order commit is already part of main.
fn fixture_with(shipped: bool) -> Fixture {
    let source = TempDir::new().unwrap();
    git_at(
        source.path(),
        "Ana",
        1_600_000_000,
        &["init", "-q", "-b", "main"],
    );
    write(source.path(), "src/orders/a.cs", "a\n");
    commit(source.path(), "Ana", 1_600_000_000, "api: init");

    let top = TempDir::new().unwrap();
    let root = top.path().to_path_buf();
    git_at(&root, "Ana", 1_600_000_000, &["init", "-q", "-b", "main"]);
    write(&root, "web/billing/x.ts", "x\n");
    write(&root, "web/checkout/c.ts", "c\n");
    write(&root, "e2e-playwright/catalog/journeys.json", CATALOG);
    commit(&root, "Ana", 1_600_000_000, "init");
    write(&root, "e2e-playwright/tests/checkout.spec.ts", SPEC);
    commit(&root, "Quinn", 1_650_000_000, "test: assert the total");
    let url = source.path().to_string_lossy().replace('\\', "/");
    git_at(
        &root,
        "Ana",
        1_600_000_000,
        &["submodule", "add", "-q", &url, "api"],
    );
    commit(&root, "Ana", 1_650_000_100, "add api");
    if shipped {
        let api = root.join("api");
        write(
            &api,
            "src/orders/s.cs",
            "shipped
",
        );
        commit(&api, "Dan", 1_660_000_000, "api: already shipped");
        commit(&root, "Ana", 1_660_000_100, "bump api");
    }

    git_at(
        &root,
        "Ana",
        1_600_000_000,
        &["checkout", "-q", "-b", "feature"],
    );
    let api = root.join("api");
    git_at(
        &api,
        "Ana",
        1_600_000_000,
        &["checkout", "-q", "-b", "feature"],
    );
    write(&api, "src/orders/a.cs", "changed\n");
    write(&api, "src/orders/b.cs", "new\n");
    commit(&api, "Bob", 1_700_000_000, "api: rewrite totals");
    write(&api, "src/users/u.cs", "u\n");
    commit(&api, "Bob", 1_700_000_100, "api: unrelated users change");
    write(&root, "web/checkout/c.ts", "changed\n");
    commit(&root, "Cara", 1_700_000_500, "web: checkout button");
    write_report(&root);
    Fixture {
        _dirs: vec![source, top],
        root,
    }
}

fn write_report(root: &Path) {
    let spec = root
        .join("e2e-playwright/tests/checkout.spec.ts")
        .to_string_lossy()
        .into_owned();
    let failing = |title: &str, journey: &str, location: Option<(&str, u32)>| {
        let mut error = serde_json::json!({"message": format!("{title} broke")});
        if let Some((file, line)) = location {
            error["location"] = serde_json::json!({"file": file, "line": line, "column": 3});
        }
        serde_json::json!({
            "title": title,
            "file": "tests/checkout.spec.ts",
            "line": 2,
            "tags": [format!("@journey:{journey}")],
            "tests": [{"projectName": "chromium", "status": "unexpected",
                       "results": [{"retry": 0, "status": "failed", "error": error}]}]
        })
    };
    let report = serde_json::json!({
        "suites": [{
            "title": "checkout.spec.ts",
            "specs": [
                failing("pays", "orders.checkout", Some((&spec, 3))),
                failing("prints", "billing.pdf", None),
                {"title": "ok", "file": "tests/checkout.spec.ts", "line": 9,
                 "tags": ["@journey:orders.checkout"],
                 "tests": [{"projectName": "chromium", "status": "expected",
                            "results": [{"retry": 0, "status": "passed"}]}]}
            ]
        }]
    });
    write(
        root,
        "e2e-playwright/results/report.json",
        &serde_json::to_string_pretty(&report).unwrap(),
    );
}

fn guard(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("guard").unwrap();
    cmd.current_dir(dir).env("NO_COLOR", "1");
    for var in [
        "GUARD_BASE",
        "GITHUB_BASE_REF",
        "GITHUB_EVENT_PATH",
        "CI_MERGE_REQUEST_DIFF_BASE_SHA",
        "CI_MERGE_REQUEST_TARGET_BRANCH_NAME",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn json(f: &Fixture, extra: &[&str]) -> serde_json::Value {
    let output = guard(&f.root)
        .args(["suspects", "--json"])
        .args(extra)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_str(&stdout(&output)).unwrap()
}

fn subjects(journey: &serde_json::Value) -> Vec<String> {
    journey["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn commits_touching_the_journey_paths_rank_by_matched_files_then_newest() {
    let f = fixture();
    let value = json(&f, &[]);
    let journeys = value["journeys"].as_array().unwrap();
    assert_eq!(journeys.len(), 2, "{value:#}");
    let checkout = &journeys[0];
    assert_eq!(checkout["journey"], "orders.checkout");
    assert_eq!(checkout["status"], "failed");
    // Bob's two order files outrank Cara's newer single file; the users
    // change touches no journey path.
    assert_eq!(
        subjects(checkout),
        ["api: rewrite totals", "web: checkout button"]
    );
    let top = &checkout["commits"][0];
    assert_eq!(top["repo"], "api");
    assert_eq!(top["author"], "Bob");
    assert_eq!(top["recent"], false);
    assert_eq!(top["date"], "2023-11-14T22:13:20Z");
    assert_eq!(
        top["files"],
        serde_json::json!(["api/src/orders/a.cs", "api/src/orders/b.cs"])
    );
    assert_eq!(checkout["commits"][1]["repo"], serde_json::Value::Null);
    assert!(top["short"].as_str().unwrap().len() >= 7);
}

#[test]
fn submodule_commits_already_recorded_by_the_base_are_not_suspects() {
    // api's own origin/main is behind the pointer main records, so its
    // default base would count Dan's shipped commit as this branch's.
    let f = fixture_with(true);
    let value = json(&f, &["--journey", "orders.checkout"]);
    assert_eq!(
        subjects(&value["journeys"][0]),
        ["api: rewrite totals", "web: checkout button"],
        "{value:#}"
    );
    let pointer = git_at(&f.root, "x", 1_600_000_000, &["rev-parse", "main:api"]);
    let api = value["bases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["repo"] == "api")
        .expect("api base");
    assert_eq!(api["merge_base"], pointer.as_str());
}

#[test]
fn the_spec_line_of_the_error_is_blamed() {
    let f = fixture();
    let value = json(&f, &["--journey", "orders.checkout"]);
    let journeys = value["journeys"].as_array().unwrap();
    assert_eq!(journeys.len(), 1);
    let blame = &journeys[0]["blame"][0];
    assert_eq!(blame["file"], "e2e-playwright/tests/checkout.spec.ts");
    assert_eq!(blame["line"], 3);
    assert_eq!(blame["author"], "Quinn");
    assert_eq!(blame["subject"], "test: assert the total");
    assert_eq!(blame["content"], "expect(total).toBe(10);");
}

#[test]
fn each_project_blames_the_line_it_failed_on() {
    let f = fixture();
    let spec = f
        .root
        .join("e2e-playwright/tests/checkout.spec.ts")
        .to_string_lossy()
        .into_owned();
    let run = |project: &str, line: u32| {
        serde_json::json!({"projectName": project, "status": "unexpected",
            "results": [{"retry": 0, "status": "failed", "error": {
                "message": "broke",
                "location": {"file": spec, "line": line, "column": 3}}}]})
    };
    let report = serde_json::json!({
        "suites": [{
            "title": "checkout.spec.ts",
            "specs": [{
                "title": "pays", "file": "tests/checkout.spec.ts", "line": 2,
                "tags": ["@journey:orders.checkout"],
                "tests": [run("CL", 3), run("PE", 4)]
            }]
        }]
    });
    write(
        &f.root,
        "e2e-playwright/results/report.json",
        &serde_json::to_string_pretty(&report).unwrap(),
    );
    let value = json(&f, &["--journey", "orders.checkout"]);
    let lines: Vec<u64> = value["journeys"][0]["blame"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, [3, 4], "{value:#}");
}

#[test]
fn without_an_error_location_the_spec_declaration_line_is_blamed() {
    let f = fixture();
    let value = json(&f, &["--journey", "billing.pdf"]);
    let blame = &value["journeys"][0]["blame"][0];
    assert_eq!(blame["file"], "e2e-playwright/tests/checkout.spec.ts");
    assert_eq!(blame["line"], 2);
}

#[test]
fn a_journey_without_branch_commits_lists_recent_ones_labelled() {
    let f = fixture();
    let value = json(&f, &["--journey", "billing.pdf"]);
    let billing = &value["journeys"][0];
    assert_eq!(subjects(billing), ["init"]);
    assert_eq!(billing["commits"][0]["recent"], true);
    let text = stdout(
        &guard(&f.root)
            .args(["suspects", "--journey", "billing.pdf"])
            .output()
            .unwrap(),
    );
    assert!(text.contains("recent, not on this branch"), "{text}");
}

#[test]
fn uncommitted_changes_on_journey_paths_are_their_own_entry() {
    let f = fixture();
    write(&f.root, "web/checkout/d.ts", "new\n");
    write(&f.root.join("api"), "src/orders/a.cs", "dirty\n");
    write(&f.root, "web/elsewhere.ts", "x\n");
    let value = json(&f, &["--journey", "orders.checkout"]);
    assert_eq!(
        value["journeys"][0]["uncommitted"],
        serde_json::json!(["web/checkout/d.ts", "api/src/orders/a.cs"])
    );
    let text = stdout(
        &guard(&f.root)
            .args(["suspects", "--journey", "orders.checkout"])
            .output()
            .unwrap(),
    );
    assert!(text.contains("Uncommitted changes"), "{text}");
}

#[test]
fn guard_base_and_limit_narrow_the_list() {
    let f = fixture();
    let output = guard(&f.root)
        .args(["suspects", "--json", "--journey", "orders.checkout"])
        .env("GUARD_BASE", "HEAD")
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    // Nothing past GUARD_BASE in either repository: HEAD records the api
    // pointer already, so only recent commits are listed.
    let commits = value["journeys"][0]["commits"].as_array().unwrap();
    assert!(!commits.is_empty(), "{value:#}");
    assert!(commits.iter().all(|c| c["recent"] == true), "{value:#}");
    let limited = json(&f, &["--journey", "orders.checkout", "--limit", "1"]);
    assert_eq!(subjects(&limited["journeys"][0]), ["api: rewrite totals"]);
}

#[test]
fn text_output_names_sha_author_and_subject() {
    let f = fixture();
    let output = guard(&f.root).arg("suspects").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = stdout(&output);
    assert!(text.contains("orders.checkout"), "{text}");
    assert!(text.contains("Possibly related changes"), "{text}");
    assert!(text.contains("Bob"), "{text}");
    assert!(text.contains("api: rewrite totals"), "{text}");
    assert!(text.contains("2023-11-14"), "{text}");
    assert!(text.contains("Spec line last changed"), "{text}");
    assert!(text.contains("Quinn"), "{text}");
}

#[test]
fn suspects_never_changes_the_repositories() {
    let f = fixture();
    let status = |dir: &Path| {
        git_at(
            dir,
            "x",
            1_600_000_000,
            &["status", "--porcelain=v1", "-uall"],
        )
    };
    let heads = |dir: &Path| git_at(dir, "x", 1_600_000_000, &["for-each-ref"]);
    let before = (
        status(&f.root),
        heads(&f.root),
        status(&f.root.join("api")),
        heads(&f.root.join("api")),
    );
    guard(&f.root)
        .args(["suspects", "--json"])
        .assert()
        .success();
    let after = (
        status(&f.root),
        heads(&f.root),
        status(&f.root.join("api")),
        heads(&f.root.join("api")),
    );
    assert_eq!(before, after);
}

#[test]
fn outside_git_it_warns_and_still_lists_the_failures() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "e2e-playwright/catalog/journeys.json", CATALOG);
    write_report(dir.path());
    let output = guard(dir.path())
        .args(["suspects", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    assert_eq!(value["journeys"].as_array().unwrap().len(), 2);
    assert!(
        value["journeys"][0]["commits"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let warnings = value["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("not a git repository")),
        "{value:#}"
    );
}

#[test]
fn a_green_report_has_no_suspects() {
    let f = fixture();
    let output = guard(&f.root)
        .args(["suspects", "--journey", "nope.none"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        stdout(&output).contains("no failing or flaky"),
        "{output:?}"
    );
}

#[test]
fn a_missing_report_is_an_environment_error() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path()).arg("suspects").output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{output:?}");
}
