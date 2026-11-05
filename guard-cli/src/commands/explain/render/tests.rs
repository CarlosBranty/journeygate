use super::*;
use std::io::Read;

fn sample() -> Explanation {
    Explanation {
        rows: vec![
            Row {
                tests: "UI ES y MX: el usuario logueado ve la lista de pedidos (2 tests)".into(),
                journeys: vec!["orders.list".into(), "orders.list-mx".into()],
                what_happened: "Timeout waiting for locator('table') | 30000ms\nsecond line".into(),
                probable_cause: "La lista no carga. El endpoint de pedidos responde 500.".into(),
                kind: "app_bug".into(),
            },
            Row {
                tests: "Login".into(),
                journeys: vec![],
                what_happened: "Selector changed".into(),
                probable_cause: "**Mantenimiento.** El selector cambió.".into(),
                kind: "suite_maintenance".into(),
            },
        ],
        verdict: "La fila 1 es un bug real; la 2 es mantenimiento.".into(),
    }
}

#[test]
fn parses_a_fenced_json_reply() {
    let text = "```json\n{\"rows\":[{\"tests\":\"t\",\"journeys\":[\"j\"],\"what_happened\":\"w\",\"probable_cause\":\"c\",\"kind\":\"test_data\"}],\"verdict\":\"v\"}\n```";
    let e = parse_structured(text).unwrap();
    assert_eq!(e.rows[0].kind, "test_data");
    assert_eq!(e.rows[0].journeys, ["j"]);
    assert_eq!(e.verdict, "v");
}

#[test]
fn parses_json_surrounded_by_prose_and_normalises_unknown_kinds() {
    let text = "Here it is:\n{\"rows\":[{\"tests\":\"t\",\"kind\":\"Flaky infra\"}],\"verdict\":\"v\"}\nThanks";
    let e = parse_structured(text).unwrap();
    assert_eq!(e.rows[0].kind, "unknown");
    assert!(e.rows[0].journeys.is_empty());
}

#[test]
fn free_text_is_not_structured() {
    assert!(parse_structured("## checkout\nThe total is wrong.").is_none());
    assert!(parse_structured("{not json}").is_none());
    assert!(parse_structured("{\"verdict\":\"no rows key\"}").is_none());
}

#[test]
fn nulls_are_tolerated_as_empty_values() {
    let text = "{\"rows\":[{\"tests\":null,\"journeys\":null,\"what_happened\":\"w\",\"probable_cause\":null,\"kind\":null}],\"verdict\":null}";
    let e = parse_structured(text).unwrap();
    assert_eq!(e.rows[0].tests, "");
    assert!(e.rows[0].journeys.is_empty());
    assert_eq!(e.rows[0].what_happened, "w");
    assert_eq!(e.rows[0].probable_cause, "");
    assert_eq!(e.rows[0].kind, "unknown");
    assert_eq!(e.verdict, "");
    assert!(parse_structured("{\"rows\":null,\"verdict\":\"v\"}").is_some());
}

#[test]
fn interpret_keeps_rows_as_structured() {
    let text = "{\"rows\":[{\"tests\":\"t\"}],\"verdict\":\"v\"}";
    assert!(matches!(interpret(text), Reply::Structured(e) if e.rows.len() == 1));
}

#[test]
fn interpret_shows_the_verdict_of_an_object_without_rows() {
    assert_eq!(
        interpret("```json\n{\"rows\":[],\"verdict\":\"All failures are flaky infra.\"}\n```"),
        Reply::Text("All failures are flaky infra.".into())
    );
    // Nothing to show at all: keep the reply rather than print a blank.
    assert_eq!(
        interpret("{\"rows\":null,\"verdict\":null}"),
        Reply::Text("{\"rows\":null,\"verdict\":null}".into())
    );
}

#[test]
fn interpret_keeps_free_text_as_is() {
    assert_eq!(
        interpret("## checkout\nThe total is wrong."),
        Reply::Text("## checkout\nThe total is wrong.".into())
    );
}

#[test]
fn table_uses_box_characters_and_localized_headers() {
    let table = render_table(&sample(), "es", 160);
    for c in ['┌', '┬', '┐', '│', '├', '┼', '┤', '└', '┴', '┘'] {
        assert!(table.contains(c), "missing {c}: {table}");
    }
    assert!(table.contains("Qué pasó"), "{table}");
    assert!(table.contains("Causa probable"), "{table}");
    assert!(table.contains("Veredicto: La fila 1"), "{table}");
    // Bold markers are stripped in the terminal.
    assert!(!table.contains("**"), "{table}");
    let english = render_table(&sample(), "en", 160);
    assert!(english.contains("What happened"), "{english}");
    assert!(english.contains("Probable cause"), "{english}");
}

#[test]
fn table_wraps_to_the_width_and_keeps_every_line_aligned() {
    let table = render_table(&sample(), "es", 70);
    let widths: Vec<usize> = table
        .lines()
        .take_while(|l| !l.is_empty())
        .map(|l| l.chars().count())
        .collect();
    assert!(widths.iter().all(|&w| w == widths[0]), "{table}");
    assert!(widths[0] <= 70, "{table}");
    // The long test label was wrapped onto several lines.
    assert!(table.contains("UI ES y MX"), "{table}");
    assert!(
        !table.contains("UI ES y MX: el usuario logueado ve la lista de pedidos (2 tests)"),
        "{table}"
    );
}

#[test]
fn table_keeps_short_columns_at_their_natural_width() {
    let e = Explanation {
        rows: vec![Row {
            tests: "t".into(),
            journeys: vec![],
            what_happened: "w".into(),
            probable_cause: "c".into(),
            kind: "unknown".into(),
        }],
        verdict: String::new(),
    };
    let table = render_table(&e, "en", 160);
    let first = table.lines().next().unwrap().chars().count();
    assert!(first < 60, "{table}");
    assert!(!table.contains("Verdict"), "{table}");
}

#[test]
fn wrap_splits_words_and_long_tokens() {
    assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
    assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
    assert_eq!(wrap("one\ntwo", 10), ["one", "two"]);
    assert_eq!(wrap("", 4), [""]);
}

#[test]
fn md_escapes_pipes_and_newlines() {
    assert_eq!(md_cell("a | b\nc"), "a \\| b<br>c");
    let md = render_md(&sample(), "es", "claude-haiku-5-5", "2026-11-05");
    assert!(
        md.starts_with("# guard explain — 2026-11-05 · claude-haiku-5-5\n"),
        "{md}"
    );
    assert!(
        md.contains("| # | Test | Qué pasó | Causa probable | Tipo |"),
        "{md}"
    );
    assert!(
        md.contains("locator('table') \\| 30000ms<br>second line"),
        "{md}"
    );
    assert!(md.contains("**La lista no carga.** El endpoint"), "{md}");
    // A cause the model already bolded is left alone.
    assert!(md.contains("**Mantenimiento.** El selector"), "{md}");
    assert!(md.contains("Bug de la app"), "{md}");
    assert!(md.contains("Mantenimiento del suite"), "{md}");
    assert!(md.contains("**Veredicto:** La fila 1"), "{md}");
    assert!(
        md.contains("1. `orders.list`, `orders.list-mx`"),
        "{md}"
    );
    assert!(md.contains("2. -"), "{md}");
}

#[test]
fn md_headers_and_kinds_in_english() {
    let md = render_md(&sample(), "en", "m", "d");
    assert!(
        md.contains("| # | Test | What happened | Probable cause | Kind |"),
        "{md}"
    );
    assert!(md.contains("App bug"), "{md}");
    assert!(md.contains("**Verdict:**"), "{md}");
}

#[test]
fn kind_labels_cover_every_kind() {
    assert_eq!(kind_label("app_bug", "es"), "Bug de la app");
    assert_eq!(
        kind_label("suite_maintenance", "es"),
        "Mantenimiento del suite"
    );
    assert_eq!(kind_label("test_data", "es"), "Datos de prueba");
    assert_eq!(kind_label("environment", "es"), "Ambiente");
    assert_eq!(kind_label("unknown", "es"), "Sin determinar");
    assert_eq!(kind_label("whatever", "en"), "Unknown");
}

#[test]
fn json_keeps_the_back_compat_keys() {
    let e = sample();
    let json = render_json("m", "md summary", Some(&e), &[]);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["model"], "m");
    assert_eq!(value["summary"], "md summary");
    assert_eq!(value["journeys"], serde_json::json!([]));
    assert_eq!(value["rows"][0]["kind"], "app_bug");
    assert_eq!(value["rows"][1]["tests"], "Login");
    assert_eq!(value["verdict"], e.verdict.as_str());

    let free = render_json("m", "free text", None, &[]);
    let value: serde_json::Value = serde_json::from_str(&free).unwrap();
    assert_eq!(value["rows"], serde_json::json!([]));
    assert!(value["verdict"].is_null());
    assert_eq!(value["summary"], "free text");
}

#[test]
fn xlsx_is_a_zip_with_both_sheets() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("explain.xlsx");
    write_xlsx(&path, &sample(), "es", "m", "2026-11-05").unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..2], b"PK");
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut workbook = String::new();
    archive
        .by_name("xl/workbook.xml")
        .unwrap()
        .read_to_string(&mut workbook)
        .unwrap();
    assert!(workbook.contains("name=\"Explain\""), "{workbook}");
    assert!(workbook.contains("name=\"Resumen\""), "{workbook}");
}

#[test]
fn civil_date_converts_days_since_the_epoch() {
    assert_eq!(civil_date(0), "1970-01-01");
    assert_eq!(civil_date(20_397), "2025-11-05");
    assert_eq!(civil_date(11_016), "2000-02-29");
}
