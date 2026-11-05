use super::*;

#[test]
fn keeps_lines_in_order() {
    let mut log = LogBuffer::new(10);
    log.push("one".into());
    log.push("two".into());
    assert_eq!(log.len(), 2);
    assert_eq!(log.get(0), Some("one"));
    assert_eq!(log.get(1), Some("two"));
    assert_eq!(log.get(2), None);
}

#[test]
fn drops_the_oldest_lines_beyond_the_cap() {
    let mut log = LogBuffer::new(3);
    for i in 0..5 {
        log.push(format!("line {i}"));
    }
    assert_eq!(log.len(), 3);
    assert_eq!(log.get(0), Some("line 2"));
    assert_eq!(log.dropped(), 2);
}

#[test]
fn clear_resets_lines_and_the_dropped_count() {
    let mut log = LogBuffer::new(1);
    log.push("a".into());
    log.push("b".into());
    log.clear();
    assert!(log.is_empty());
    assert_eq!(log.dropped(), 0);
}

#[test]
fn pushed_lines_lose_terminal_colors_and_carriage_returns() {
    let mut log = LogBuffer::new(5);
    log.push("\u{1b}[32mok\u{1b}[0m\r".into());
    assert_eq!(log.get(0), Some("ok"));
}

#[test]
fn ansi_escapes_are_stripped() {
    assert_eq!(strip_ansi("\u{1b}[31mred\u{1b}[39m plain"), "red plain");
    assert_eq!(strip_ansi("\u{1b}[2K\u{1b}[1Gx"), "x");
    assert_eq!(strip_ansi("no escapes"), "no escapes");
}

#[test]
fn line_kinds_colour_the_log() {
    assert_eq!(line_kind("$ guard test --all"), LineKind::Command);
    assert_eq!(line_kind("[exit 0]"), LineKind::Exit);
    assert_eq!(line_kind("error: no target.toml"), LineKind::Error);
    assert_eq!(
        line_kind("  \u{2718}  1 [chromium] > checkout.spec.ts"),
        LineKind::Error
    );
    assert_eq!(line_kind("  1 failed"), LineKind::Error);
    assert_eq!(
        line_kind("  \u{2713}  2 [chromium] > login.spec.ts (1.2s)"),
        LineKind::Pass
    );
    assert_eq!(line_kind("  12 passed (40s)"), LineKind::Pass);
    assert_eq!(
        line_kind("Running 14 tests using 2 workers"),
        LineKind::Plain
    );
}
