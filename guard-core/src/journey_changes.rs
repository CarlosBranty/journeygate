//! Red flags for journeys modified in a PR (section 14 of the architecture doc).
//!
//! CODEOWNERS approval is always required for journeys; this only surfaces red
//! flags to the QA reviewer. The facts are extracted by
//! `guard_impact::journey_change` (tree-sitter, diffing each modified journey
//! file against the base commit).

use serde::{Deserialize, Serialize};

/// Facts about one modified journey spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JourneyChange {
    /// Catalog id (from the spec's `@journey:<id>` tag).
    pub id: String,
    /// Repo-relative spec path.
    pub file: String,
    /// `expect(...)` / `expect.poll(...)` / `expect.soft(...)` calls at base; `0` for a new file.
    pub assertions_before: u32,
    pub assertions_after: u32,
    /// `test.skip` / `test.fixme` / `.skip(` / `.fixme(` calls added (after minus before, floored at 0).
    pub skips_added: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "review", content = "flags", rename_all = "snake_case")]
pub enum ChangeReview {
    Clean,
    Flagged(Vec<String>),
}

/// Flag texts are part of the contract (they appear in PR and Jira comments):
///
/// - `journeys modified, but the ticket is not labeled flow-change` (once, when
///   `changes` is non-empty and `ticket_is_flow_change` is false)
/// - `<id>: assertions <before> -> <after>` when assertions decreased
/// - `<id>: <n> skip/fixme added` when `skips_added > 0`
///
/// Flags are in that order, then per change in input order.
pub fn review(changes: &[JourneyChange], ticket_is_flow_change: bool) -> ChangeReview {
    let mut flags = Vec::new();
    if !changes.is_empty() && !ticket_is_flow_change {
        flags.push(NOT_FLOW_CHANGE.to_string());
    }
    for change in changes {
        if change.assertions_after < change.assertions_before {
            flags.push(format!(
                "{}: assertions {} -> {}",
                change.id, change.assertions_before, change.assertions_after
            ));
        }
        if change.skips_added > 0 {
            flags.push(format!(
                "{}: {} skip/fixme added",
                change.id, change.skips_added
            ));
        }
    }
    if flags.is_empty() {
        ChangeReview::Clean
    } else {
        ChangeReview::Flagged(flags)
    }
}

const NOT_FLOW_CHANGE: &str = "journeys modified, but the ticket is not labeled flow-change";

#[cfg(test)]
mod tests;
