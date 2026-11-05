//! How far a running `guard test` is, read from Playwright's `list` reporter
//! lines as they arrive in the log: the announced total ("Running 8 tests
//! using 2 workers") and one status line per finished attempt
//! ("  ✓  3 [chromium] › a.spec.ts › title (1.2s)"). A retry that passes
//! turns its earlier failure into a flaky pass.

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunProgress {
    /// The tests Playwright announced; `None` until it started.
    pub total: Option<usize>,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    /// Passed on a retry (also counted in `passed`).
    pub flaky: usize,
}

#[derive(Clone, Copy)]
enum Mark {
    Pass,
    Fail,
    Skip,
}

/// The status mark and test number that open a reporter line.
fn mark(line: &str) -> Option<Mark> {
    let rest = line.strip_prefix("  ")?.trim_start();
    let (symbol, after) = rest.split_once(char::is_whitespace)?;
    let mark = match symbol {
        "\u{2713}" | "\u{2714}" | "ok" => Mark::Pass,
        "\u{2718}" | "\u{00d7}" | "x" => Mark::Fail,
        "-" => Mark::Skip,
        _ => return None,
    };
    // The attempt number, then the project: "3 [chromium] › ...".
    let after = after.trim_start();
    let (number, tail) = after.split_once(char::is_whitespace)?;
    (number.parse::<u32>().is_ok() && tail.trim_start().starts_with('[')).then_some(mark)
}

impl RunProgress {
    pub fn feed(&mut self, line: &str) {
        if let Some(rest) = line.trim().strip_prefix("Running ") {
            if let Some(n) = rest.split_whitespace().next().and_then(|n| n.parse().ok()) {
                *self = Self {
                    total: Some(n),
                    ..Self::default()
                };
            }
            return;
        }
        let Some(mark) = mark(line) else {
            return;
        };
        let retry = line.contains("(retry #");
        match (mark, retry) {
            (Mark::Pass, true) if self.failed > 0 => {
                self.failed -= 1;
                self.passed += 1;
                self.flaky += 1;
            }
            // A retry repeats a test already counted.
            (_, true) => {}
            (Mark::Pass, false) => self.passed += 1,
            (Mark::Fail, false) => self.failed += 1,
            (Mark::Skip, false) => self.skipped += 1,
        }
    }

    /// Tests with a final outcome so far.
    pub fn done(&self) -> usize {
        self.passed + self.failed + self.skipped
    }

    /// 0 to 1, once the total is known.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.filter(|t| *t > 0)?;
        Some((self.done() as f32 / total as f32).min(1.0))
    }

    /// "4/8 journeys · 2 ok · 1 fallaron", once Playwright started.
    pub fn summary(&self) -> Option<String> {
        let total = self.total?;
        let mut text = format!("{}/{total} journeys", self.done().min(total));
        if self.passed > 0 {
            text.push_str(&format!(" · {} ok", self.passed));
        }
        if self.failed > 0 {
            text.push_str(&format!(" · {} fallaron", self.failed));
        }
        if self.flaky > 0 {
            text.push_str(&format!(" · {} inestables", self.flaky));
        }
        Some(text)
    }
}
