//! `guard_map`: a ranked, token-budgeted map of the repository for agents
//! (`docs/contracts/guard-map.md`).
//!
//! The ranking (personalized PageRank seeded by the ticket's focus files) runs
//! over any code graph that implements [`GraphSource`]. Candidate backends —
//! the `guard-impact` import graph, a CodeGraph index, a tree-sitter tag graph —
//! are chosen by benchmark (tokens.md, A/B protocol), so nothing here depends
//! on one of them. The response never exceeds [`MapRequest::token_budget`]
//! as measured by [`estimate_tokens`].
//!
//! Backends: [`GuardImpactSource`] (the default, [`Backend::default`], per the
//! impact-vs-CodeGraph benchmark) and [`CodeGraphSource`] (the read-only
//! `codegraph` CLI over a pre-built index). [`HidePaths`] removes the Coder's
//! hidden test dirs from any backend; [`discrepancy`] logs where the two
//! backends disagree; [`tool`] is the `guard_map` MCP tool for `guard-mcp`.

pub mod backend;
pub mod codegraph;
pub mod discrepancy;
#[cfg(any(test, feature = "fakes"))]
pub mod fake;
pub mod impact_source;
mod map;
pub mod rank;
mod symbols;
pub mod tool;

pub use backend::{Backend, CODER_HIDDEN_PREFIXES, HidePaths};
pub use codegraph::{CodeGraphCli, CodeGraphSource, ProcessCli};
pub use impact_source::GuardImpactSource;
pub use map::{
    FOCUS_WEIGHT, MAX_SYMBOLS_PER_FILE, MENTION_WEIGHT, build_map, focus_files, measure,
    mentioned_paths,
};
pub use symbols::scan_symbols;

use serde::{Deserialize, Serialize};

/// Default and ceiling of `token_budget` (guard-map.md).
pub const DEFAULT_TOKEN_BUDGET: u32 = 1024;
pub const MAX_TOKEN_BUDGET: u32 = 4096;
/// Smallest budget that still fits the response envelope.
pub const MIN_TOKEN_BUDGET: u32 = 128;

/// A directed dependency `from` → `to` (e.g. `from` imports `to`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// Positive; 1.0 for a plain import.
    pub weight: f64,
}

/// A named definition inside a file, as the backend reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Symbol {
    pub name: String,
    /// `function`, `class`, `type`, `const`, ... (backend vocabulary).
    pub kind: String,
    /// 1-based.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MapError {
    /// Index missing, stale or unreadable.
    #[error("graph source unavailable: {0}")]
    Unavailable(String),
    #[error("invalid map request: {0}")]
    InvalidRequest(String),
}

/// A code graph of one checkout. Synchronous: implementations read a local
/// index (wrap calls in `spawn_blocking`). Paths are repo-relative with `/`.
pub trait GraphSource: Send + Sync {
    /// Stable backend id reported in [`MapResponse::backend`] (e.g. `guard-impact`).
    fn backend(&self) -> &'static str;

    /// Commit the graph was built at.
    fn commit(&self) -> &str;

    /// Every node (file) of the graph, sorted.
    fn files(&self) -> Result<Vec<String>, MapError>;

    /// Every edge of the graph.
    fn edges(&self) -> Result<Vec<Edge>, MapError>;

    /// Definitions of one file, in source order. Unknown file = empty list.
    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError>;
}

/// Borrowed sources (including `&dyn GraphSource`) are sources too, so
/// wrappers such as [`HidePaths`] compose over a reference.
impl<T: GraphSource + ?Sized> GraphSource for &T {
    fn backend(&self) -> &'static str {
        (**self).backend()
    }
    fn commit(&self) -> &str {
        (**self).commit()
    }
    fn files(&self) -> Result<Vec<String>, MapError> {
        (**self).files()
    }
    fn edges(&self) -> Result<Vec<Edge>, MapError> {
        (**self).edges()
    }
    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError> {
        (**self).symbols(path)
    }
}

/// Input of the `guard_map` MCP tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRequest {
    /// Personalization seed: files the ticket or diff names. May be empty
    /// (uniform PageRank).
    #[serde(default)]
    pub focus_files: Vec<String>,
    /// Identifiers mentioned in the ticket; files defining them get extra seed weight.
    #[serde(default)]
    pub mentioned: Vec<String>,
    /// `MIN_TOKEN_BUDGET..=MAX_TOKEN_BUDGET`.
    #[serde(default = "default_budget")]
    pub token_budget: u32,
}

fn default_budget() -> u32 {
    DEFAULT_TOKEN_BUDGET
}

impl MapRequest {
    pub fn validate(&self) -> Result<(), MapError> {
        if !(MIN_TOKEN_BUDGET..=MAX_TOKEN_BUDGET).contains(&self.token_budget) {
            return Err(MapError::InvalidRequest(format!(
                "token_budget must be in {MIN_TOKEN_BUDGET}..={MAX_TOKEN_BUDGET}"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapEntry {
    pub path: String,
    /// PageRank score, descending across entries.
    pub rank: f64,
    /// `name:line` of the top definitions shown for this file.
    pub symbols: Vec<String>,
}

/// Output of the `guard_map` MCP tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapResponse {
    pub backend: String,
    pub commit: String,
    pub token_budget: u32,
    /// [`estimate_tokens`] of the serialized response; `<= token_budget`.
    pub estimated_tokens: u32,
    pub entries: Vec<MapEntry>,
    /// Ranked files left out to respect the budget.
    pub omitted: u32,
}

/// The budget meter shared by every backend: `ceil(bytes / 4)` of the UTF-8
/// text. Deliberately conservative and tokenizer-free, so results are
/// reproducible across models.
pub fn estimate_tokens(text: &str) -> u32 {
    text.len().div_ceil(4) as u32
}

/// `MapError::Unavailable` unless `source` was built at `head` (guard-map.md,
/// "Errors": an index built at another commit is unavailable, never stale data).
pub fn ensure_commit(source: &dyn GraphSource, head: &str) -> Result<(), MapError> {
    if source.commit() == head {
        Ok(())
    } else {
        Err(MapError::Unavailable(format!(
            "{} index built at {}, HEAD is {head}",
            source.backend(),
            source.commit()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_is_ceil_bytes_over_four() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn index_at_another_commit_is_unavailable() {
        let s = fake::FakeGraphSource::new("guard-impact", "aaa");
        assert!(ensure_commit(&s, "aaa").is_ok());
        assert!(matches!(
            ensure_commit(&s, "bbb"),
            Err(MapError::Unavailable(_))
        ));
    }

    #[test]
    fn request_defaults_and_budget_bounds() {
        let r: MapRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.token_budget, DEFAULT_TOKEN_BUDGET);
        assert!(r.validate().is_ok());
        for bad in [0, MIN_TOKEN_BUDGET - 1, MAX_TOKEN_BUDGET + 1] {
            let r = MapRequest {
                token_budget: bad,
                ..r.clone()
            };
            assert!(r.validate().is_err());
        }
    }
}
