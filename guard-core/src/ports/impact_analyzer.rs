//! `ImpactAnalyzer`: deterministic impact facts from the repository.
//!
//! Synchronous on purpose: it is CPU work over a local checkout (tree-sitter
//! parsing), not network I/O. Async callers wrap it in `spawn_blocking`.

use super::types::{Area, ImpactFacts};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImpactError {
    /// The repository root cannot be read.
    #[error("cannot read repository: {0}")]
    Io(String),
    /// A glob of `target.toml` failed to compile.
    #[error("invalid glob: {0}")]
    InvalidGlob(String),
}

pub trait ImpactAnalyzer: Send + Sync {
    /// Fan-out and hard-rule facts for `changed_files` (repo-relative, forward
    /// slashes). Unknown or deleted files count with fan-out `0` and still go
    /// through the `human_only_paths` and schema-migration checks.
    fn fan_out(&self, changed_files: &[String]) -> Result<ImpactFacts, ImpactError>;

    /// `[areas]` keys whose `paths` globs match any of `changed_paths`,
    /// sorted and deduplicated.
    fn areas_for(&self, changed_paths: &[String]) -> Vec<Area>;

    /// Repository files that a ticket's mentioned module names refer to: an
    /// exact repo path, a path suffix (`orders/service.ts`), or a file stem
    /// (`OrderService` matches `**/OrderService.*` and `**/order-service.*`).
    /// Sorted, deduplicated, at most 200 paths.
    fn resolve_mentions(&self, mentions: &[String]) -> Vec<String>;
}
