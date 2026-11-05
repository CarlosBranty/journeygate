use super::*;

#[test]
fn a_layout_saved_before_the_run_mode_existed_loads_as_headless() {
    let old = r#"{"view":"Tests","bottom_open":true,"bottom_tab":"Log","up_timeout":120}"#;
    let layout: Layout = serde_json::from_str(old).unwrap();
    assert_eq!(layout.run_mode, RunMode::Headless);
    assert_eq!(layout.up_timeout, 120);
}

#[test]
fn the_run_mode_is_kept_with_the_layout() {
    let layout = Layout {
        run_mode: RunMode::Headed,
        ..Layout::default()
    };
    let json = serde_json::to_string(&layout).unwrap();
    let back: Layout = serde_json::from_str(&json).unwrap();
    assert_eq!(back.run_mode, RunMode::Headed);
}

fn captured(code: Option<i32>, stderr: &str) -> Captured {
    Captured {
        code,
        stdout: String::new(),
        stderr: stderr.into(),
    }
}

#[test]
fn an_export_that_exits_zero_and_wrote_its_file_is_done() {
    let path = PathBuf::from("repo/.guard/explain/explain-20261105-101500.md");
    let out = Ok(captured(
        Some(0),
        "wrote repo/.guard/explain/explain-20261105-101500.md",
    ));
    assert_eq!(parse_export(out, &path, |_| true), Ok(path.clone()));
}

#[test]
fn an_export_that_failed_or_wrote_nothing_is_an_error() {
    let path = PathBuf::from("repo/.guard/explain/x.xlsx");
    let failed = parse_export(
        Ok(captured(Some(2), "error: --format xlsx needs --out\n")),
        &path,
        |_| true,
    );
    assert_eq!(
        failed,
        Err("guard exited 2: error: --format xlsx needs --out".into())
    );
    let missing = parse_export(Ok(captured(Some(0), "")), &path, |_| false);
    assert!(missing.unwrap_err().contains("x.xlsx"));
    let spawn = parse_export(Err("cannot start".into()), &path, |_| true);
    assert_eq!(spawn, Err("cannot start".into()));
}

/// `guard explain --json` of a structured reply: `summary` is the Markdown
/// table the CLI renders from `rows` and `verdict`.
const STRUCTURED_EXPLAIN: &str = r##"{"model":"m","rows":[{"tests":"Login (2 tests)","journeys":["auth.login"],"what_happened":"Timeout | 30s","probable_cause":"Selector changed. Detail","kind":"suite_maintenance"}],"verdict":"Suite maintenance only.","summary":"# guard explain — 2026-11-05 · m\n\n| # | Test | Qué pasó | Causa probable | Tipo |\n| --- | --- | --- | --- | --- |\n| 1 | Login (2 tests) | Timeout \\| 30s<br>at login.spec | **Selector changed.** Detail | Mantenimiento del suite |\n\n**Veredicto:** Suite maintenance only.\n\nJourneys:\n\n1. `auth.login`\n","journeys":[]}"##;

#[test]
fn a_structured_explain_summary_renders_as_a_table() {
    let out = Ok(Captured {
        code: Some(0),
        stdout: STRUCTURED_EXPLAIN.into(),
        stderr: String::new(),
    });
    let summary = parse_explain(out).unwrap();
    let blocks = crate::markdown::blocks(&summary);
    let Some(crate::markdown::Block::Table { header, rows }) = blocks
        .iter()
        .find(|b| matches!(b, crate::markdown::Block::Table { .. }))
    else {
        panic!("no table in {blocks:?}");
    };
    assert_eq!(header.len(), 5);
    assert_eq!(rows.len(), 1);
    let text = |spans: &[crate::markdown::Span]| -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    };
    assert_eq!(text(&rows[0][2]), "Timeout | 30s\nat login.spec");
    assert_eq!(text(&rows[0][4]), "Mantenimiento del suite");
    // No raw table syntax leaks into the other blocks.
    let leaked = blocks.iter().any(|b| match b {
        crate::markdown::Block::Paragraph(spans) => text(spans).contains("---"),
        _ => false,
    });
    assert!(!leaked, "{blocks:?}");
}
