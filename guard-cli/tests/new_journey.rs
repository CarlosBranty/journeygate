//! `guard new journey` end to end (cli.md).

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A credentials file that never exists: the developer's own
/// (`guard connect`) must not leak into tests.
const NO_CREDENTIALS: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/no-credentials.toml");

fn guard(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("guard").unwrap();
    cmd.current_dir(dir)
        .env("NO_COLOR", "1")
        .env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS);
    for var in [
        "RUN_ID",
        "GUARD_NPX",
        "GITHUB_TOKEN",
        "GITLAB_TOKEN",
        "JIRA_API_TOKEN",
    ] {
        cmd.env_remove(var);
    }
    cmd
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

/// A plain directory (not a git repo) with the fixture target.toml and catalog.
fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    write(
        root,
        "target.toml",
        &fs::read_to_string(fixture("target.toml")).unwrap(),
    );
    write(
        root,
        "e2e-playwright/catalog/journeys.json",
        &fs::read_to_string(fixture("journeys.json")).unwrap(),
    );
    dir
}

const CATALOG: &str = "e2e-playwright/catalog/journeys.json";

#[test]
fn new_journey_appends_to_the_catalog_and_writes_the_spec() {
    let dir = project();
    let root = dir.path();
    let before = read(root, CATALOG);
    let output = guard(root)
        .args([
            "new",
            "journey",
            "checkout.gift-card",
            "--title",
            "Customer Pays With A Gift Card",
            "--path",
            "web/src/checkout/**",
            "--path",
            "services/gift/**",
            "--smoke",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "added journey checkout.gift-card to e2e-playwright/catalog/journeys.json\n\
         created e2e-playwright/tests/journeys/checkout/gift-card.spec.ts\n\
         next: guard test --journey checkout.gift-card\n"
    );

    let after = read(root, CATALOG);
    let kept = before.trim_end().strip_suffix('}').unwrap().trim_end();
    let kept = kept.strip_suffix(']').unwrap().trim_end();
    assert!(
        after.starts_with(kept),
        "existing entries changed:\n{after}"
    );
    assert!(after.ends_with("\n  ]\n}\n"), "{after}");
    let catalog: serde_json::Value = serde_json::from_str(&after).unwrap();
    let journeys = catalog["journeys"].as_array().unwrap();
    assert_eq!(journeys.len(), 4);
    assert_eq!(
        journeys[3],
        serde_json::json!({
            "id": "checkout.gift-card",
            "area": "checkout",
            "title": "Customer Pays With A Gift Card",
            "impact_question": "This diff can directly or indirectly change how customer pays with a gift card",
            "paths": ["web/src/checkout/**", "services/gift/**"],
            "smoke": true,
            "quarantined": false
        })
    );

    let spec = read(
        root,
        "e2e-playwright/tests/journeys/checkout/gift-card.spec.ts",
    );
    assert!(spec.starts_with("import { expect, test } from '../../../src/fixtures';\n"));
    assert!(
        spec.contains("{ tag: ['@journey:checkout.gift-card', '@area:checkout', '@smoke'] }"),
        "{spec}"
    );
    assert!(spec.contains("test.fixme("), "{spec}");

    // The result still validates, schema and cross-file rules included.
    let parsed = guard_core::catalog::parse_catalog(&after).unwrap();
    let config = guard_core::config::parse_target(&read(root, "target.toml")).unwrap();
    assert!(guard_core::catalog::check_against_config(&parsed, &config).is_empty());
}

#[test]
fn new_journey_defaults_area_and_title_from_the_id() {
    let dir = project();
    let root = dir.path();
    let output = guard(root)
        .args(["new", "journey", "billing.credit-note"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let catalog: serde_json::Value = serde_json::from_str(&read(root, CATALOG)).unwrap();
    let added = &catalog["journeys"][3];
    assert_eq!(added["area"], "billing");
    assert_eq!(added["title"], "Credit note");
    assert_eq!(added["paths"], serde_json::json!([]));
    assert_eq!(added["smoke"], false);
    let spec = read(
        root,
        "e2e-playwright/tests/journeys/billing/credit-note.spec.ts",
    );
    assert!(
        spec.contains("test.describe('Credit note', { tag: ['@journey:billing.credit-note', '@area:billing'] }"),
        "{spec}"
    );
}

#[test]
fn new_journey_refuses_duplicates_unknown_areas_and_bad_input_without_writing() {
    let dir = project();
    let root = dir.path();
    let before = read(root, CATALOG);
    let cases: &[(&[&str], &str)] = &[
        (&["checkout.customer-card"], "already in the catalog"),
        (&["search.by-name"], "area search is not a key of [areas]"),
        (
            &["checkout.x", "--area", "nope"],
            "area nope is not a key of [areas]",
        ),
        (&["Checkout"], "must match"),
        (&["checkout.x", "--path", "web/["], "not a valid glob"),
        (&["checkout.x", "--title", " "], "--title must not be empty"),
    ];
    for (args, message) in cases {
        let output = guard(root)
            .args(["new", "journey"])
            .args(*args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(
            stderr(&output).contains(message),
            "{args:?}: {}",
            stderr(&output)
        );
    }
    let unknown = guard(root)
        .args(["new", "journey", "search.by-name"])
        .output()
        .unwrap();
    assert!(
        stderr(&unknown).contains("hint: areas: billing, checkout"),
        "{}",
        stderr(&unknown)
    );
    assert_eq!(read(root, CATALOG), before);
    assert!(!root.join("e2e-playwright/tests").exists());
}

#[test]
fn new_journey_never_overwrites_an_existing_spec() {
    let dir = project();
    let root = dir.path();
    let before = read(root, CATALOG);
    write(
        root,
        "e2e-playwright/tests/journeys/checkout/gift-card.spec.ts",
        "// mine\n",
    );
    let output = guard(root)
        .args(["new", "journey", "checkout.gift-card"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("already exists"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        read(
            root,
            "e2e-playwright/tests/journeys/checkout/gift-card.spec.ts"
        ),
        "// mine\n"
    );
    assert_eq!(read(root, CATALOG), before);
}

#[test]
fn new_journey_with_an_invalid_catalog_is_a_usage_error() {
    let dir = project();
    let root = dir.path();
    write(root, CATALOG, "{ not json");
    let output = guard(root)
        .args(["new", "journey", "checkout.gift-card"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}
