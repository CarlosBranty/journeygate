//! A capped log of run output lines: the oldest lines go first, so memory stays
//! bounded however long a run is.

use std::collections::VecDeque;

#[cfg(test)]
mod tests;

/// The default cap of the Run screen log.
pub const DEFAULT_CAP: usize = 5000;

#[derive(Debug, Clone)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    cap: usize,
    dropped: usize,
}

impl LogBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap.min(256)),
            cap: cap.max(1),
            dropped: 0,
        }
    }

    /// Appends one line without terminal escapes or a trailing `\r`.
    pub fn push(&mut self, line: String) {
        let line = if line.contains('\u{1b}') {
            strip_ansi(&line)
        } else {
            line
        };
        let line = match line.strip_suffix('\r') {
            Some(trimmed) => trimmed.to_string(),
            None => line,
        };
        if self.lines.len() == self.cap {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.dropped = 0;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.lines.get(index).map(String::as_str)
    }

    /// Lines dropped because of the cap since the last [`Self::clear`].
    pub fn dropped(&self) -> usize {
        self.dropped
    }
}

/// Removes ANSI CSI sequences (`ESC [ ... final-byte`) and lone escapes.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // Parameter and intermediate bytes, then one final byte @..~.
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        }
    }
    out
}

/// How the log colours a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// `$ guard ...`, the header the GUI writes.
    Command,
    /// `[exit 0]`, the footer the GUI writes.
    Exit,
    Error,
    Pass,
    Plain,
}

/// A cheap guess from Playwright's and guard's line reporter output.
pub fn line_kind(line: &str) -> LineKind {
    let trimmed = line.trim_start();
    if line.starts_with("$ ") {
        return LineKind::Command;
    }
    if line.starts_with('[') && line.ends_with(']') {
        return LineKind::Exit;
    }
    let lower = trimmed.to_lowercase();
    if trimmed.starts_with('\u{2718}')
        || trimmed.starts_with('\u{00d7}')
        || lower.starts_with("error")
        || lower.contains(" failed")
        || lower.starts_with("failed")
    {
        return LineKind::Error;
    }
    if trimmed.starts_with('\u{2713}') || lower.contains(" passed") {
        return LineKind::Pass;
    }
    LineKind::Plain
}
