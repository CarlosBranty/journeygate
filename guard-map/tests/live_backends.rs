//! Live: both backends over a real checkout with a pre-built `.codegraph/`
//! index (the benchmark's hono clone works). Read-only; set
//! `GUARD_CODEGRAPH_TEST_REPO` and run with `--include-ignored`.

use guard_core::config::parse_target;
use guard_map::discrepancy::{compare_affected, compare_impact};
use guard_map::{
    CodeGraphSource, GraphSource, GuardImpactSource, HidePaths, MapRequest, ProcessCli, build_map,
    estimate_tokens,
};
use std::path::Path;

#[test]
#[ignore = "needs the codegraph CLI and GUARD_CODEGRAPH_TEST_REPO"]
fn both_backends_map_within_budget_and_report_discrepancies() {
    let Ok(repo) = std::env::var("GUARD_CODEGRAPH_TEST_REPO") else {
        missing_live_env("GUARD_CODEGRAPH_TEST_REPO");
        return;
    };
    let config = parse_target(
        "version = 1\n[app]\nname = \"live\"\nlanguages = []\nbase_url = \"http://localhost\"\n\
         [areas.all]\ndescription = \"All\"\npaths = [\"**\"]\n",
    )
    .unwrap();
    let gi = GuardImpactSource::build(Path::new(&repo), &config, "live").unwrap();
    let scope = "src/preset/";
    let cg = CodeGraphSource::new(Box::new(ProcessCli::default()), &repo, "live")
        .with_scope(scope)
        .with_concurrency(4);

    let focus: Vec<String> = cg.files().unwrap();
    assert!(!focus.is_empty());
    for budget in [128, 1024, 4096] {
        let request = MapRequest {
            focus_files: focus.clone(),
            mentioned: vec!["Hono".into()],
            token_budget: budget,
        };
        for source in [&gi as &dyn GraphSource, &cg] {
            let r = build_map(&HidePaths::for_coder(source), &request).unwrap();
            let wire = serde_json::to_string(&r).unwrap();
            assert!(
                estimate_tokens(&wire) <= budget,
                "{}: over budget",
                r.backend
            );
            assert!(!r.entries.is_empty());
            eprintln!(
                "{} budget={budget} entries={} omitted={} tokens={} top={}",
                r.backend,
                r.entries.len(),
                r.omitted,
                r.estimated_tokens,
                r.entries[0].path
            );
        }
    }

    let truncated = cg.truncated().unwrap();
    let impact = compare_impact(&gi, &cg, &focus, 3, &truncated).unwrap();
    let affected = compare_affected(&gi, &cg, &focus, 3).unwrap();
    eprintln!(
        "{}{}",
        guard_map::discrepancy::to_jsonl(&impact),
        guard_map::discrepancy::to_jsonl(&affected)
    );
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
