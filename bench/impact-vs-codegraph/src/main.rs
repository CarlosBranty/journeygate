//! `impact-vs-codegraph <repo> <language> <changesets.json> <out.json>`
//!
//! Builds guard-impact's `ImpactIndex` over `<repo>` through its public API,
//! dumps every file->file import edge, and for each change set computes the
//! depth-3 dependents (`importers_of` BFS) and the unbounded
//! `transitive_importers` union, timing index build and each query.

use guard_core::config::parse_target;
use guard_core::ports::ImpactAnalyzer;
use guard_impact::ImpactIndex;
use impact_vs_codegraph::{bounded_dependents, config_for};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

#[derive(Deserialize)]
struct ChangeSet {
    id: String,
    files: Vec<String>,
}

#[derive(Serialize)]
struct Query {
    id: String,
    depth3: Vec<String>,
    unbounded: Vec<String>,
    query_us: u128,
    fan_out_us: u128,
}

#[derive(Serialize)]
struct Output {
    index_ms: u128,
    files: usize,
    edges: Vec<(String, String)>,
    queries: Vec<Query>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: impact-vs-codegraph <repo> <language> <changesets.json> <out.json>");
        std::process::exit(2);
    }
    let root = Path::new(&args[1]);
    let config = parse_target(&config_for(&args[2])).expect("valid bench config");
    let sets: Vec<ChangeSet> =
        serde_json::from_str(&std::fs::read_to_string(&args[3]).expect("changesets"))
            .expect("changesets json");

    let t = Instant::now();
    let index = ImpactIndex::build(root, &config).expect("index builds");
    let index_ms = t.elapsed().as_millis();

    let files: Vec<String> = index.files().map(str::to_string).collect();
    let mut edges = Vec::new();
    for f in &files {
        for to in index.imports_of(f) {
            edges.push((f.clone(), to.to_string()));
        }
    }

    let mut queries = Vec::new();
    for set in sets {
        let t = Instant::now();
        let depth3 = bounded_dependents(&index, &set.files, 3);
        let query_us = t.elapsed().as_micros();
        let changed: BTreeSet<&str> = set.files.iter().map(String::as_str).collect();
        let unbounded: BTreeSet<String> = set
            .files
            .iter()
            .flat_map(|f| index.transitive_importers(f))
            .filter(|f| !changed.contains(f))
            .map(str::to_string)
            .collect();
        let t = Instant::now();
        let _ = index.fan_out(&set.files).expect("fan_out");
        let fan_out_us = t.elapsed().as_micros();
        queries.push(Query {
            id: set.id,
            depth3,
            unbounded: unbounded.into_iter().collect(),
            query_us,
            fan_out_us,
        });
    }

    let out = Output {
        index_ms,
        files: files.len(),
        edges,
        queries,
    };
    std::fs::write(&args[4], serde_json::to_string(&out).expect("json")).expect("write output");
}
