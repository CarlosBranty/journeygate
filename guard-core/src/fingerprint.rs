//! Failure fingerprints: the same failure always lands on the same Jira Bug.
//!
//! Contract: `docs/contracts/reporting.md`, section "Fingerprint".
//!
//! ```text
//! fingerprint = first 8 hex chars of sha1(test_id + "\n" + normalize_error(message))
//! label       = "guard-fp-" + fingerprint
//! test_id     = "<project>::<file>::<title>"
//! ```

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::sync::LazyLock;

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").unwrap());
static UUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
        .unwrap()
});
static HEX_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-fA-F]{8,}\b").unwrap());
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+(\.\d+)?").unwrap());
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// Prefix of the Jira label that carries a fingerprint.
pub const LABEL_PREFIX: &str = "guard-fp-";

/// Eight lowercase hex characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(pub String);

impl Fingerprint {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `guard-fp-<8hex>`.
    pub fn label(&self) -> String {
        format!("{LABEL_PREFIX}{}", self.0)
    }
}

/// `<project>::<file>::<title>`, where `title` is the full Playwright title
/// path joined with ` > ` (as in `.guard/verdict.json`).
pub fn test_id(project: &str, file: &str, title: &str) -> String {
    format!("{project}::{file}::{title}")
}

/// Normalizes an error message so that run-specific noise does not change the
/// fingerprint. Rules, in order (reporting.md):
///
/// 1. Strip ANSI escape sequences (`\x1b[...m`).
/// 2. Keep the first non-empty line.
/// 3. Replace UUIDs with `<id>`.
/// 4. Replace runs of 8+ hex digits containing at least one digit with `<id>`.
/// 5. Replace every remaining run of ASCII digits (with optional `.` decimals) with `<n>`.
/// 6. Collapse whitespace runs to one space and trim.
///
/// Line and column numbers (`file.ts:12:5`) are covered by rule 5.
pub fn normalize_error(message: &str) -> String {
    let without_ansi = ANSI.replace_all(message, "");
    let line = without_ansi
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let line = UUID.replace_all(line, "<id>");
    let line = HEX_RUN.replace_all(&line, |caps: &Captures<'_>| {
        let run = &caps[0];
        if run.bytes().any(|b| b.is_ascii_digit()) {
            "<id>".to_string()
        } else {
            run.to_string()
        }
    });
    let line = NUMBER.replace_all(&line, "<n>");
    WHITESPACE.replace_all(&line, " ").trim().to_string()
}

/// The fingerprint of a failure.
pub fn fingerprint(test_id: &str, error_message: &str) -> Fingerprint {
    sha1_prefix(&format!("{test_id}\n{}", normalize_error(error_message)))
}

/// First 8 lowercase hex characters of `sha1(input)`.
fn sha1_prefix(input: &str) -> Fingerprint {
    let digest = Sha1::digest(input.as_bytes());
    Fingerprint(digest[..4].iter().map(|b| format!("{b:02x}")).collect())
}

/// Parses `guard-fp-<8hex>` back into a fingerprint.
pub fn from_label(label: &str) -> Option<Fingerprint> {
    let hex = label.strip_prefix(LABEL_PREFIX)?;
    let valid = hex.len() == 8 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    valid.then(|| Fingerprint(hex.to_string()))
}

#[cfg(test)]
mod tests;
