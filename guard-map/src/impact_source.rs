//! [`GraphSource`] over the `guard-impact` import graph (the default backend,
//! chosen by the impact-vs-CodeGraph benchmark: docs/benchmarks/).

use crate::symbols::scan_symbols;
use crate::{Edge, GraphSource, MapError, Symbol};
use guard_core::config::TargetConfig;
use guard_impact::ImpactIndex;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Reads a repository file's text. `None` = unreadable or absent.
type Reader = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct GuardImpactSource {
    index: ImpactIndex,
    commit: String,
    files: Vec<String>,
    read: Reader,
    symbols: Mutex<BTreeMap<String, Vec<Symbol>>>,
}

impl std::fmt::Debug for GuardImpactSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardImpactSource")
            .field("commit", &self.commit)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

impl GuardImpactSource {
    /// Walks and indexes the checkout at `root` (see `ImpactIndex::build`).
    pub fn build(root: &Path, config: &TargetConfig, commit: &str) -> Result<Self, MapError> {
        let index =
            ImpactIndex::build(root, config).map_err(|e| MapError::Unavailable(e.to_string()))?;
        Ok(Self::from_index(index, root.to_path_buf(), commit))
    }

    /// Wraps an already-built index of the checkout at `root`.
    pub fn from_index(index: ImpactIndex, root: PathBuf, commit: &str) -> Self {
        Self::with_reader(
            index,
            commit,
            Box::new(move |rel: &str| {
                let meta = std::fs::metadata(root.join(rel)).ok()?;
                if meta.len() > guard_impact::MAX_PARSED_FILE_BYTES {
                    return None;
                }
                std::fs::read_to_string(root.join(rel)).ok()
            }),
        )
    }

    /// For in-memory trees (tests): `read` returns a file's contents.
    pub fn with_reader(index: ImpactIndex, commit: &str, read: Reader) -> Self {
        // Nodes are the files a language plugin analyzes: configs and docs carry
        // no edges and would only dilute the ranking.
        let files = index
            .files()
            .filter(|f| index.language_of(f).is_some())
            .map(str::to_string)
            .collect();
        Self {
            index,
            commit: commit.to_string(),
            files,
            read,
            symbols: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn index(&self) -> &ImpactIndex {
        &self.index
    }
}

impl GraphSource for GuardImpactSource {
    fn backend(&self) -> &'static str {
        crate::backend::GUARD_IMPACT
    }

    fn commit(&self) -> &str {
        &self.commit
    }

    fn files(&self) -> Result<Vec<String>, MapError> {
        Ok(self.files.clone())
    }

    fn edges(&self) -> Result<Vec<Edge>, MapError> {
        Ok(self
            .files
            .iter()
            .flat_map(|from| {
                self.index.imports_of(from).into_iter().map(move |to| Edge {
                    from: from.clone(),
                    to: to.to_string(),
                    weight: 1.0,
                })
            })
            .collect())
    }

    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError> {
        // Only indexed files are read: never anything outside the checkout.
        if self
            .files
            .binary_search_by(|f| f.as_str().cmp(path))
            .is_err()
        {
            return Ok(Vec::new());
        }
        let mut cache = self.symbols.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(hit) = cache.get(path) {
            return Ok(hit.clone());
        }
        let found = (self.read)(path)
            .map(|src| scan_symbols(path, &src))
            .unwrap_or_default();
        cache.insert(path.to_string(), found.clone());
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::config::parse_target;

    fn config() -> TargetConfig {
        parse_target(
            "version = 1\n[app]\nname = \"t\"\nlanguages = []\nbase_url = \"http://localhost\"\n\
             [areas.web]\ndescription = \"Web\"\npaths = [\"src/**\"]\n",
        )
        .unwrap()
    }

    fn tree() -> BTreeMap<String, String> {
        [
            (
                "src/app.ts",
                "import { r } from './router'\nexport function app() {}\n",
            ),
            (
                "src/router.ts",
                "import { u } from './utils'\nexport class Router {}\n",
            ),
            ("src/utils.ts", "export const u = 1\n"),
            ("README.md", "# readme\n"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
    }

    fn source() -> GuardImpactSource {
        let t = tree();
        let index = ImpactIndex::from_sources(t.clone(), &config()).unwrap();
        GuardImpactSource::with_reader(index, "deadbeef", Box::new(move |p| t.get(p).cloned()))
    }

    #[test]
    fn exposes_analyzed_files_and_import_edges() {
        let s = source();
        assert_eq!(s.backend(), "guard-impact");
        assert_eq!(s.commit(), "deadbeef");
        assert_eq!(
            s.files().unwrap(),
            ["src/app.ts", "src/router.ts", "src/utils.ts"]
        );
        let edges: Vec<(String, String)> = s
            .edges()
            .unwrap()
            .into_iter()
            .map(|e| (e.from, e.to))
            .collect();
        assert_eq!(
            edges,
            [
                ("src/app.ts".to_string(), "src/router.ts".to_string()),
                ("src/router.ts".to_string(), "src/utils.ts".to_string())
            ]
        );
    }

    #[test]
    fn symbols_come_from_the_scanner_and_only_for_indexed_files() {
        let s = source();
        let names: Vec<String> = s
            .symbols("src/router.ts")
            .unwrap()
            .into_iter()
            .map(|x| x.name)
            .collect();
        assert_eq!(names, ["Router"]);
        assert!(s.symbols("README.md").unwrap().is_empty());
        assert!(s.symbols("../../etc/passwd").unwrap().is_empty());
    }

    #[test]
    fn builds_from_a_checkout_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        for (p, c) in tree() {
            let path = dir.path().join(&p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        let s = GuardImpactSource::build(dir.path(), &config(), "c0ffee").unwrap();
        assert_eq!(s.files().unwrap().len(), 3);
        assert_eq!(s.symbols("src/app.ts").unwrap()[0].name, "app");
        assert!(matches!(
            GuardImpactSource::build(&dir.path().join("missing"), &config(), "x"),
            Err(MapError::Unavailable(_))
        ));
    }
}
