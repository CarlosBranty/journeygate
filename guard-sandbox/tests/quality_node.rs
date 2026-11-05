//! `NodeSpecQuality` against the real `assets/spec-quality.mjs`.
//!
//! Needs `node` on PATH and `pnpm install` in `guard-sandbox/assets`
//! (TypeScript). Without them the tests fail (never a vacuous `ok`), unless
//! `GUARD_ALLOW_SKIPPED_LIVE_TESTS=1` turns that into a skip.

use guard_sandbox::harness::quality::{NodeSpecQuality, SpecQuality};
use std::path::Path;

fn checker() -> Option<NodeSpecQuality> {
    let ts = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/node_modules/typescript");
    let node_ok = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !node_ok || !ts.exists() {
        missing_live_env("node + guard-sandbox/assets/node_modules (pnpm install)");
        return None;
    }
    Some(NodeSpecQuality::bundled(vec!["/api/".into()]))
}

#[tokio::test]
async fn a_clean_spec_passes() {
    let Some(q) = checker() else { return };
    let spec = br#"import { test, expect } from '@playwright/test';
test('adds an item @ticket:SHOP-1', async ({ page }) => {
  await page.goto('/cart');
  await expect(page.getByTestId('total')).toHaveText('2');
});
"#;
    let v = q
        .check("SHOP-1", "tests/features/SHOP-1.spec.ts", spec)
        .await
        .unwrap();
    assert!(v.is_empty(), "{v:?}");
}

#[tokio::test]
async fn focus_sleep_and_self_mock_are_reported_with_stable_ids() {
    let Some(q) = checker() else { return };
    let spec = br#"import { test, expect } from '@playwright/test';
test.only('adds @ticket:SHOP-1', async ({ page }) => {
  await page.route('**/api/cart', (r) => r.fulfill({ body: '{"n":2}' }));
  await page.waitForTimeout(1000);
  expect(true).toBe(true);
});
"#;
    let v = q
        .check("SHOP-1", "tests/features/SHOP-1.spec.ts", spec)
        .await
        .unwrap();
    let ids: Vec<&str> = v.iter().map(|x| x.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "no_focus",
            "no_self_mock",
            "no_wait_for_timeout",
            "no_trivial_expect"
        ],
        "{v:?}"
    );
    assert_eq!(v[0].line, 2);
}

#[tokio::test]
async fn a_missing_script_is_a_quality_error() {
    let Some(mut q) = checker() else { return };
    q.script = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/missing.mjs");
    assert!(q.check("SHOP-1", "x.spec.ts", b"").await.is_err());
}

/// A live test was asked to run (`--include-ignored`) without `var`: fail,
/// so a CI run that forgot it never reports a vacuous `ok`.
/// `GUARD_ALLOW_SKIPPED_LIVE_TESTS=1` turns that into a skip.
fn missing_live_env(var: &str) {
    if std::env::var("GUARD_ALLOW_SKIPPED_LIVE_TESTS").as_deref() == Ok("1") {
        eprintln!("skipped: {var} not set (GUARD_ALLOW_SKIPPED_LIVE_TESTS=1)");
        return;
    }
    panic!("{var} is not set; set it, or GUARD_ALLOW_SKIPPED_LIVE_TESTS=1 to skip live tests");
}
