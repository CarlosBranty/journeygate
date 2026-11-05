//! Benchmark helpers over guard-impact's public API.

use guard_impact::ImpactIndex;
use std::collections::BTreeSet;

/// A minimal valid `target.toml` that enables one guard-impact language plugin.
pub fn config_for(language: &str) -> String {
    format!(
        "version = 1

[app]
name = \"bench\"
languages = [\"{language}\"]
base_url = \"http://localhost:1\"

[areas.all]
description = \"Whole repository\"
paths = [\"**\"]
"
    )
}

/// Files reaching any of `changed` through at most `depth` import hops,
/// following `ImpactIndex::importers_of`. The changed files themselves are
/// excluded. Sorted.
pub fn bounded_dependents(index: &ImpactIndex, changed: &[String], depth: usize) -> Vec<String> {
    let start: BTreeSet<String> = changed.iter().cloned().collect();
    let mut seen: BTreeSet<String> = start.clone();
    let mut frontier: Vec<String> = start.iter().cloned().collect();
    for _ in 0..depth {
        let mut next = Vec::new();
        for f in &frontier {
            for importer in index.importers_of(f) {
                if seen.insert(importer.to_string()) {
                    next.push(importer.to_string());
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    seen.difference(&start).cloned().collect()
}
