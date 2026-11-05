//! `ImpactAnalyzer` adapter.
//!
//! Implements `guard_core::ports::ImpactAnalyzer`: builds a reverse import graph
//! of the repository with tree-sitter (one plugin per language), computes
//! fan-out for changed files, maps paths to `[areas]` of `target.toml`, flags
//! `[hard_rules].human_only_paths` and schema migrations, and extracts
//! assertion/skip counts of modified journey specs. Languages without a plugin
//! fall back to the `target.toml` globs. Contract: `docs/contracts/ports.md`.
//!
//! Plugins: TypeScript (`.ts`, `.tsx`, `.mts`, `.cts`), JavaScript (`.js`,
//! `.jsx`, `.mjs`, `.cjs`), Python, Go, Rust, C# (`.cs`) and PHP (`.php`). Each one is a grammar crate,
//! the query `languages/<lang>/imports.scm` (embedded at build time), a
//! resolver and a fixture mini-repo under `tests/fixtures/<lang>/`.

mod cargo;
mod csharp;
mod diff;
mod extract;
mod index;
mod journey;
mod lang;
mod php;
mod resolve;
mod tsconfig;

pub use diff::{
    MAX_FILES, MAX_HUNK_LINES, MAX_HUNKS, MAX_LINE_CHARS, MAX_SUMMARY_JSON_BYTES, MAX_SYMBOLS,
    summarize_diff,
};
pub use index::{
    AnalysisMethod, FileImpact, ImpactIndex, MAX_MENTION_PATHS, MAX_PARSED_FILE_BYTES,
    MIGRATION_GLOBS,
};
pub use journey::journey_change;
pub use lang::Language;
