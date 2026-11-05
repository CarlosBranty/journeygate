//! An in-memory [`GraphSource`] for tests (crate tests and, behind the
//! `fakes` feature, downstream crates such as `guard-mcp`).

use crate::{Edge, GraphSource, MapError, Symbol};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct FakeGraphSource {
    pub backend: &'static str,
    pub commit: String,
    pub files: Vec<String>,
    pub edges: Vec<Edge>,
    pub symbols: BTreeMap<String, Vec<Symbol>>,
    /// When set, every call fails with `MapError::Unavailable(..)`.
    pub unavailable: Option<String>,
}

impl FakeGraphSource {
    pub fn new(backend: &'static str, commit: &str) -> Self {
        Self {
            backend,
            commit: commit.to_string(),
            ..Self::default()
        }
    }

    pub fn file(mut self, path: &str) -> Self {
        if !self.files.iter().any(|f| f == path) {
            self.files.push(path.to_string());
            self.files.sort();
        }
        self
    }

    /// `from` imports `to` (both become nodes).
    pub fn edge(self, from: &str, to: &str) -> Self {
        let mut me = self.file(from).file(to);
        me.edges.push(Edge {
            from: from.into(),
            to: to.into(),
            weight: 1.0,
        });
        me
    }

    pub fn symbol(mut self, path: &str, name: &str, kind: &str, line: u32) -> Self {
        self = self.file(path);
        self.symbols.entry(path.into()).or_default().push(Symbol {
            name: name.into(),
            kind: kind.into(),
            line,
        });
        self
    }

    fn check(&self) -> Result<(), MapError> {
        match &self.unavailable {
            Some(why) => Err(MapError::Unavailable(why.clone())),
            None => Ok(()),
        }
    }
}

impl GraphSource for FakeGraphSource {
    fn backend(&self) -> &'static str {
        self.backend
    }

    fn commit(&self) -> &str {
        &self.commit
    }

    fn files(&self) -> Result<Vec<String>, MapError> {
        self.check()?;
        Ok(self.files.clone())
    }

    fn edges(&self) -> Result<Vec<Edge>, MapError> {
        self.check()?;
        Ok(self.edges.clone())
    }

    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError> {
        self.check()?;
        Ok(self.symbols.get(path).cloned().unwrap_or_default())
    }
}
