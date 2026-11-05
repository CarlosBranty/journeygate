//! Spec runs on a clean checkout: Playwright JSON report parsing, the
//! red-first verdict (qa-coder-protocol.md step 4), the gate verdict (step 8)
//! and the feedback handed to the next attempt.

use serde_json::Value;

/// One failing test of a [`SpecRun`], with its message already sanitized:
/// no ANSI codes, no source snippet lines, no stack frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedTest {
    pub title: String,
    /// Playwright result status: `failed`, `timedOut`, `interrupted`.
    pub status: String,
    pub message: String,
    pub assertion: bool,
}

/// The outcome of running a spec selection with the Playwright JSON reporter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpecRun {
    pub passed: u32,
    /// Results with status `failed` (assertion or thrown error).
    pub failed: u32,
    /// Results with status `timedOut` or `interrupted`.
    pub timed_out: u32,
    pub skipped: u32,
    /// Failed results whose error comes from `expect(...)`.
    pub assertion_failures: u32,
    /// Top-level errors: syntax/compile errors, config errors, no tests found.
    pub load_errors: Vec<String>,
    pub failures: Vec<FailedTest>,
    /// The whole run exceeded its wall clock (set by the runner).
    pub process_timed_out: bool,
}

/// Red-first verdict on the base commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedFirst {
    /// Failed with at least one assertion failure: the spec proves something new.
    Red,
    /// Every test passed on the base commit: `red_first_passed`.
    Passed,
    /// Failed, but not by an assertion (compile error, thrown error, timeout).
    NotAnAssertionFailure(String),
}

const MAX_MESSAGE_BYTES: usize = 600;

impl SpecRun {
    /// Parses a Playwright `--reporter=json` report. Uses the last result of
    /// each test (the final retry).
    pub fn from_playwright_json(bytes: &[u8]) -> Result<SpecRun, String> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("playwright report: {e}"))?;
        let mut run = SpecRun::default();
        if let Some(errors) = root.get("errors").and_then(Value::as_array) {
            for e in errors {
                let msg = e.get("message").and_then(Value::as_str).unwrap_or("error");
                run.load_errors.push(sanitize(msg));
            }
        }
        let suites = root
            .get("suites")
            .and_then(Value::as_array)
            .ok_or("playwright report has no suites array")?;
        for s in suites {
            walk_suite(s, &mut Vec::new(), &mut run);
        }
        Ok(run)
    }

    pub fn total(&self) -> u32 {
        self.passed + self.failed + self.timed_out
    }

    /// qa-coder-protocol.md step 4.
    pub fn red_first(&self) -> RedFirst {
        if self.process_timed_out {
            return RedFirst::NotAnAssertionFailure("the run timed out".into());
        }
        if let Some(e) = self.load_errors.first() {
            return RedFirst::NotAnAssertionFailure(format!("the spec did not load: {e}"));
        }
        if self.total() == 0 {
            return RedFirst::NotAnAssertionFailure("no test ran".into());
        }
        if self.failed == 0 && self.timed_out == 0 {
            return RedFirst::Passed;
        }
        if self.assertion_failures > 0 {
            return RedFirst::Red;
        }
        let first = self
            .failures
            .first()
            .map(|f| format!("{} ({}): {}", f.title, f.status, f.message))
            .unwrap_or_default();
        RedFirst::NotAnAssertionFailure(format!("no assertion failed: {first}"))
    }

    /// qa-coder-protocol.md step 8: everything selected ran and passed.
    pub fn gate_passed(&self) -> bool {
        !self.process_timed_out
            && self.load_errors.is_empty()
            && self.failed == 0
            && self.timed_out == 0
            && self.passed > 0
    }

    /// Feedback for the next attempt: failing tests only (title, status,
    /// sanitized message), never test sources. At most `max_bytes`.
    pub fn failure_context(&self, max_bytes: usize) -> String {
        let mut out = String::new();
        if self.process_timed_out {
            out.push_str("The test run timed out.\n");
        }
        for e in &self.load_errors {
            out.push_str(&format!("Load error: {e}\n"));
        }
        if !self.failures.is_empty() {
            out.push_str(&format!(
                "{} failing test(s) of {}:\n",
                self.failures.len(),
                self.total()
            ));
        }
        for f in &self.failures {
            out.push_str(&format!("- {} [{}]: {}\n", f.title, f.status, f.message));
        }
        if out.is_empty() {
            out.push_str("No test passed.\n");
        }
        truncate_utf8(&out, max_bytes)
    }
}

fn walk_suite(suite: &Value, titles: &mut Vec<String>, run: &mut SpecRun) {
    let title = suite.get("title").and_then(Value::as_str).unwrap_or("");
    // The file-level suite title is the file name; keep describe titles only.
    let pushed = !title.is_empty() && !title.ends_with(".ts") && !title.ends_with(".js");
    if pushed {
        titles.push(title.to_string());
    }
    for spec in suite
        .get("specs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let spec_title = spec.get("title").and_then(Value::as_str).unwrap_or("");
        let full = titles
            .iter()
            .map(String::as_str)
            .chain([spec_title])
            .collect::<Vec<_>>()
            .join(" > ");
        for test in spec
            .get("tests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(result) = test
                .get("results")
                .and_then(Value::as_array)
                .and_then(|r| r.last())
            else {
                continue;
            };
            let status = result.get("status").and_then(Value::as_str).unwrap_or("");
            match status {
                "passed" => run.passed += 1,
                "skipped" => run.skipped += 1,
                _ => {
                    let messages: Vec<String> = result
                        .get("errors")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|e| e.get("message").and_then(Value::as_str))
                        .map(strip_ansi)
                        .collect();
                    let assertion =
                        status == "failed" && messages.iter().any(|m| is_assertion_message(m));
                    if status == "failed" {
                        run.failed += 1;
                    } else {
                        run.timed_out += 1;
                    }
                    if assertion {
                        run.assertion_failures += 1;
                    }
                    run.failures.push(FailedTest {
                        title: full.clone(),
                        status: status.to_string(),
                        message: sanitize(&messages.join("\n")),
                        assertion,
                    });
                }
            }
        }
    }
    for child in suite
        .get("suites")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk_suite(child, titles, run);
    }
    if pushed {
        titles.pop();
    }
}

/// Playwright renders assertion errors as `expect(received).toBe(expected)`
/// or `expect(locator).toBeVisible() failed`.
fn is_assertion_message(message: &str) -> bool {
    message.lines().take(3).any(|l| {
        l.trim_start()
            .trim_start_matches("Error: ")
            .starts_with("expect(")
    })
}

/// Removes ANSI escape sequences (`ESC [ ... <letter>`).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// ANSI stripped; source snippet lines (`  3 | code`, `> 3 | code`,
/// `    |   ^`) and stack frames (`    at ...`) dropped; blank runs
/// collapsed; capped at [`MAX_MESSAGE_BYTES`].
fn sanitize(message: &str) -> String {
    let plain = strip_ansi(message);
    let kept: Vec<&str> = plain
        .lines()
        .map(str::trim_end)
        .filter(|l| !is_snippet_line(l) && !l.trim_start().starts_with("at "))
        .collect();
    let mut joined = String::new();
    let mut blank = false;
    for l in kept {
        if l.trim().is_empty() {
            if !blank && !joined.is_empty() {
                joined.push(' ');
            }
            blank = true;
            continue;
        }
        if !joined.is_empty() && !blank {
            joined.push(' ');
        }
        blank = false;
        joined.push_str(l.trim());
    }
    truncate_utf8(joined.trim(), MAX_MESSAGE_BYTES)
}

fn is_snippet_line(line: &str) -> bool {
    let t = line.trim_start().trim_start_matches('>').trim_start();
    match t.split_once('|') {
        Some((num, _)) => num.trim().is_empty() || num.trim().chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// At most `max` bytes, cut on a char boundary.
pub fn truncate_utf8(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> SpecRun {
        let path = format!(
            "{}/tests/fixtures/playwright/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        SpecRun::from_playwright_json(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn an_assertion_failure_is_red() {
        let run = fixture("assertion_fail");
        assert_eq!((run.failed, run.assertion_failures, run.passed), (1, 1, 0));
        assert_eq!(run.red_first(), RedFirst::Red);
        assert!(!run.gate_passed());
        assert_eq!(run.failures[0].title, "cart total @ticket:SHOP-1");
    }

    #[test]
    fn a_passing_spec_is_red_first_passed() {
        let run = fixture("pass");
        assert_eq!(run.red_first(), RedFirst::Passed);
        assert!(run.gate_passed());
    }

    #[test]
    fn a_syntax_error_is_not_an_assertion_failure() {
        let run = fixture("syntax_error");
        // Playwright reports the SyntaxError and then "No tests found".
        assert_eq!(run.load_errors.len(), 2, "{:?}", run.load_errors);
        assert_eq!(run.total(), 0);
        assert!(
            matches!(run.red_first(), RedFirst::NotAnAssertionFailure(m) if m.contains("SyntaxError"))
        );
        assert!(!run.gate_passed());
    }

    #[test]
    fn a_thrown_error_is_not_an_assertion_failure() {
        let run = fixture("thrown_error");
        assert_eq!((run.failed, run.assertion_failures), (1, 0));
        assert!(
            matches!(run.red_first(), RedFirst::NotAnAssertionFailure(m) if m.contains("ECONNREFUSED"))
        );
    }

    #[test]
    fn a_timeout_is_not_an_assertion_failure() {
        let run = fixture("timeout");
        assert_eq!(run.timed_out, 1);
        assert!(matches!(
            run.red_first(),
            RedFirst::NotAnAssertionFailure(_)
        ));
        let mut whole = fixture("assertion_fail");
        whole.process_timed_out = true;
        assert!(matches!(
            whole.red_first(),
            RedFirst::NotAnAssertionFailure(_)
        ));
    }

    #[test]
    fn failure_context_has_no_ansi_snippets_or_stack_and_is_capped() {
        let run = fixture("assertion_fail");
        let ctx = run.failure_context(4096);
        assert!(ctx.contains("cart total @ticket:SHOP-1 [failed]"), "{ctx}");
        assert!(
            ctx.contains("Expected: 3") && ctx.contains("Received: 2"),
            "{ctx}"
        );
        assert!(!ctx.contains('\u{1b}'), "{ctx}");
        assert!(
            !ctx.contains("import { test"),
            "source snippet leaked: {ctx}"
        );
        assert!(!ctx.contains(" at "), "stack leaked: {ctx}");
        assert!(run.failure_context(20).len() <= 20);
        assert_eq!(truncate_utf8("ééé", 3), "é");
    }

    #[test]
    fn an_empty_report_ran_nothing() {
        let run = SpecRun::from_playwright_json(br#"{"suites":[],"errors":[]}"#).unwrap();
        assert!(
            matches!(run.red_first(), RedFirst::NotAnAssertionFailure(m) if m == "no test ran")
        );
        assert!(!run.gate_passed());
        assert!(SpecRun::from_playwright_json(b"{}").is_err());
    }
}
