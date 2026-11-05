//! Shared helpers for the per-language fixture tests.
#![allow(dead_code)]

use guard_core::config::parse_target;
use guard_impact::ImpactIndex;
use std::path::PathBuf;

pub fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Builds the index of `tests/fixtures/<name>/` with its own `target.toml`.
pub fn index(name: &str) -> ImpactIndex {
    let root = fixture_root(name);
    let text = std::fs::read_to_string(root.join("target.toml")).expect("fixture target.toml");
    let config = parse_target(&text).expect("valid fixture target.toml");
    ImpactIndex::build(&root, &config).expect("index builds")
}

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

pub fn sorted(mut v: Vec<&str>) -> Vec<&str> {
    v.sort_unstable();
    v
}
