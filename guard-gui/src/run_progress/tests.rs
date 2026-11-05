use super::*;

fn fed(lines: &[&str]) -> RunProgress {
    let mut p = RunProgress::default();
    for line in lines {
        p.feed(line);
    }
    p
}

#[test]
fn nothing_is_known_before_playwright_starts() {
    let p = fed(&[
        "$ guard test --affected",
        "affected vs origin/main: 5 journeys",
    ]);
    assert_eq!(p, RunProgress::default());
    assert_eq!(p.fraction(), None);
    assert_eq!(p.summary(), None);
}

#[test]
fn the_list_reporter_lines_are_counted() {
    let p = fed(&[
        "Running 8 tests using 2 workers",
        "  \u{2713}  1 [chromium] > account/login.spec.ts > Customer signs in (4.2s)",
        "  \u{2713}  2 [chromium] > catalog/search.spec.ts > Search (3.3s)",
        "  \u{2718}  3 [chromium] > checkout/guest.spec.ts > Guest checkout (11.0s)",
        "  -  4 [chromium] > checkout/later.spec.ts > Later",
    ]);
    assert_eq!(p.total, Some(8));
    assert_eq!((p.passed, p.failed, p.skipped), (2, 1, 1));
    assert_eq!(p.done(), 4);
    assert_eq!(p.fraction(), Some(0.5));
    assert_eq!(
        p.summary().as_deref(),
        Some("4/8 journeys · 2 ok · 1 fallaron")
    );
}

#[test]
fn ascii_symbols_of_a_plain_console_are_counted_too() {
    let p = fed(&[
        "Running 1 test using 1 worker",
        "  ok 1 [chromium] > a.spec.ts > A (1.0s)",
        "  x  2 [chromium] > b.spec.ts > B (1.0s)",
    ]);
    assert_eq!(p.total, Some(1));
    assert_eq!((p.passed, p.failed), (1, 1));
    // Never more than the announced total.
    assert_eq!(p.fraction(), Some(1.0));
}

#[test]
fn a_retry_that_passes_turns_a_failure_into_a_flaky_pass() {
    let p = fed(&[
        "Running 2 tests using 1 worker",
        "  \u{2718}  1 [chromium] > a.spec.ts > A (1.0s)",
        "  \u{2713}  2 [chromium] > a.spec.ts > A (retry #1) (1.0s)",
        "  \u{2718}  3 [chromium] > b.spec.ts > B (1.0s)",
        "  \u{2718}  4 [chromium] > b.spec.ts > B (retry #1) (1.0s)",
    ]);
    assert_eq!((p.passed, p.failed, p.flaky), (1, 1, 1));
    assert_eq!(p.done(), 2);
}

#[test]
fn summary_lines_and_other_output_are_ignored() {
    let p = fed(&[
        "Running 2 tests using 1 worker",
        "  1 failed",
        "  1 passed (2.1s)",
        "    Error: expect(received).toBe(expected) \u{2718}",
    ]);
    assert_eq!(p.done(), 0);
}
