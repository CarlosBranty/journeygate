//! `Secret`: a credential value held only in memory.
//!
//! It has no `Serialize` impl and its `Debug` prints `***`, so a virtual key
//! or a provider token cannot leak into logs, `task.json`, the decision log or
//! a fixture by accident. Files and configuration carry env var *names* only.

/// A secret string. Read it with [`Secret::expose`] at the single place that
/// hands it to a process (an env var of a sandbox, an `Authorization` header).
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(\"***\")")
    }
}
