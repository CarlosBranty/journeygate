//! Backend ids, the default backend and the hidden-path filter.

use crate::{Edge, GraphSource, MapError, Symbol};
use serde::{Deserialize, Serialize};

pub const GUARD_IMPACT: &str = "guard-impact";
pub const CODEGRAPH: &str = "codegraph";

/// Which [`GraphSource`] backs `guard_map`.
///
/// The default is the benchmark decision `guard_map_uses_guard_impact`
/// (docs/benchmarks/: import-edge macro F1 0.981 vs 0.520, depth-3 impact
/// recall 0.955 vs 0.866, guard-impact ahead on both pre-registered criteria).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    #[default]
    GuardImpact,
    Codegraph,
}

impl Backend {
    pub fn id(self) -> &'static str {
        match self {
            Backend::GuardImpact => GUARD_IMPACT,
            Backend::Codegraph => CODEGRAPH,
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            GUARD_IMPACT => Some(Backend::GuardImpact),
            CODEGRAPH => Some(Backend::Codegraph),
            _ => None,
        }
    }
}

/// Directories a Coder must never see (sandbox.md, guard-map.md "Never").
pub const CODER_HIDDEN_PREFIXES: [&str; 2] = ["tests/journeys/", "tests/smoke/"];

/// A [`GraphSource`] with every file under `hidden` prefixes removed: they are
/// not nodes, not edge endpoints and have no symbols. Wrap any backend with
/// [`CODER_HIDDEN_PREFIXES`] for a Coder.
pub struct HidePaths<S> {
    inner: S,
    hidden: Vec<String>,
}

impl<S: GraphSource> HidePaths<S> {
    pub fn new<I, P>(inner: S, hidden: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        Self {
            inner,
            hidden: hidden.into_iter().map(Into::into).collect(),
        }
    }

    pub fn for_coder(inner: S) -> Self {
        Self::new(inner, CODER_HIDDEN_PREFIXES)
    }

    fn hides(&self, path: &str) -> bool {
        self.hidden.iter().any(|h| path.starts_with(h.as_str()))
    }
}

impl<S: GraphSource> GraphSource for HidePaths<S> {
    fn backend(&self) -> &'static str {
        self.inner.backend()
    }

    fn commit(&self) -> &str {
        self.inner.commit()
    }

    fn files(&self) -> Result<Vec<String>, MapError> {
        let mut f = self.inner.files()?;
        f.retain(|p| !self.hides(p));
        Ok(f)
    }

    fn edges(&self) -> Result<Vec<Edge>, MapError> {
        let mut e = self.inner.edges()?;
        e.retain(|x| !self.hides(&x.from) && !self.hides(&x.to));
        Ok(e)
    }

    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError> {
        if self.hides(path) {
            return Ok(Vec::new());
        }
        self.inner.symbols(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeGraphSource;
    use crate::{MapRequest, build_map};

    #[test]
    fn default_backend_is_the_benchmark_decision() {
        assert_eq!(Backend::default(), Backend::GuardImpact);
        assert_eq!(Backend::default().id(), "guard-impact");
        assert_eq!(Backend::from_id("codegraph"), Some(Backend::Codegraph));
        assert_eq!(Backend::from_id("x"), None);
        assert_eq!(
            serde_json::to_string(&Backend::GuardImpact).unwrap(),
            "\"guard-impact\""
        );
    }

    #[test]
    fn hidden_tests_never_reach_the_map() {
        let inner = FakeGraphSource::new("fake", "c")
            .edge("tests/journeys/login.spec.ts", "src/login.ts")
            .edge("src/app.ts", "src/login.ts")
            .symbol(
                "tests/smoke/smoke.spec.ts",
                "secretAssertion",
                "function",
                1,
            );
        let s = HidePaths::for_coder(inner);
        let r = build_map(
            &s,
            &MapRequest {
                focus_files: vec!["tests/journeys/login.spec.ts".into()],
                mentioned: vec!["secretAssertion".into()],
                token_budget: 1024,
            },
        )
        .unwrap();
        let wire = serde_json::to_string(&r).unwrap();
        assert!(!wire.contains("tests/journeys") && !wire.contains("tests/smoke"));
        assert!(!wire.contains("secretAssertion"));
        assert_eq!(r.entries.len(), 2);
    }
}
