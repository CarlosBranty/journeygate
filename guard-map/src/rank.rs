//! Personalized PageRank over the file graph (guard-map.md, "Algorithm").

use crate::Edge;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::BTreeMap;

pub const DAMPING: f64 = 0.85;
pub const MAX_ITERATIONS: usize = 50;
pub const TOLERANCE: f64 = 1e-6;

/// Ranks `files` by personalized PageRank.
///
/// - Edges whose endpoints are not in `files`, self-loops and non-positive or
///   non-finite weights are ignored; parallel edges are summed.
/// - `personalization` maps files to positive seed weights (unknown files and
///   non-positive weights are ignored); when nothing remains it is uniform.
/// - Dangling nodes redistribute their mass uniformly.
///
/// The result is sorted by rank descending, ties by path ascending, and sums
/// to 1 (within floating-point error) when `files` is non-empty.
pub fn personalized_pagerank(
    files: &[String],
    edges: &[Edge],
    personalization: &BTreeMap<String, f64>,
) -> Vec<(String, f64)> {
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort();
    sorted.dedup();
    let n = sorted.len();
    if n == 0 {
        return Vec::new();
    }

    let mut graph: DiGraph<&str, f64> = DiGraph::with_capacity(n, edges.len());
    let index: BTreeMap<&str, NodeIndex> = sorted
        .iter()
        .map(|f| (f.as_str(), graph.add_node(f.as_str())))
        .collect();
    for e in edges {
        if !(e.weight.is_finite() && e.weight > 0.0) || e.from == e.to {
            continue;
        }
        let (Some(&a), Some(&b)) = (index.get(e.from.as_str()), index.get(e.to.as_str())) else {
            continue;
        };
        match graph.find_edge(a, b) {
            Some(ix) => graph[ix] += e.weight,
            None => {
                graph.add_edge(a, b, e.weight);
            }
        }
    }

    let mut seed = vec![0.0; n];
    for (path, w) in personalization {
        if let Some(&ix) = index.get(path.as_str())
            && w.is_finite()
            && *w > 0.0
        {
            seed[ix.index()] += w;
        }
    }
    let total: f64 = seed.iter().sum();
    if total > 0.0 {
        seed.iter_mut().for_each(|s| *s /= total);
    } else {
        seed.iter_mut().for_each(|s| *s = 1.0 / n as f64);
    }

    let out_weight: Vec<f64> = graph
        .node_indices()
        .map(|u| graph.edges(u).map(|e| *e.weight()).sum())
        .collect();

    let mut rank = seed.clone();
    for _ in 0..MAX_ITERATIONS {
        let dangling: f64 = graph
            .node_indices()
            .filter(|u| out_weight[u.index()] == 0.0)
            .map(|u| rank[u.index()])
            .sum();
        let mut next: Vec<f64> = seed
            .iter()
            .map(|s| (1.0 - DAMPING) * s + DAMPING * dangling / n as f64)
            .collect();
        for edge in graph.raw_edges() {
            let u = edge.source().index();
            let v = edge.target().index();
            next[v] += DAMPING * rank[u] * edge.weight / out_weight[u];
        }
        let delta: f64 = next.iter().zip(&rank).map(|(a, b)| (a - b).abs()).sum();
        rank = next;
        if delta < TOLERANCE {
            break;
        }
    }

    let mut ranked: Vec<(String, f64)> = sorted
        .iter()
        .enumerate()
        .map(|(i, f)| ((*f).clone(), rank[i]))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            weight: 1.0,
        }
    }

    fn files(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn sum(r: &[(String, f64)]) -> f64 {
        r.iter().map(|x| x.1).sum()
    }

    #[test]
    fn empty_graph_ranks_nothing() {
        assert!(personalized_pagerank(&[], &[], &BTreeMap::new()).is_empty());
    }

    #[test]
    fn uniform_without_edges_or_seeds_ties_by_path() {
        let r = personalized_pagerank(&files(&["b", "a", "c"]), &[], &BTreeMap::new());
        let paths: Vec<&str> = r.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(paths, ["a", "b", "c"]);
        for (_, s) in &r {
            assert!((s - 1.0 / 3.0).abs() < 1e-9);
        }
    }

    #[test]
    fn imported_file_outranks_importers() {
        let r = personalized_pagerank(
            &files(&["a", "b", "core"]),
            &[e("a", "core"), e("b", "core")],
            &BTreeMap::new(),
        );
        assert_eq!(r[0].0, "core");
        assert!((sum(&r) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn personalization_pulls_rank_toward_seed_and_its_dependencies() {
        // Two disjoint chains; seeding x makes x and its dependency y top.
        let edges = [e("x", "y"), e("p", "q")];
        let seeds = BTreeMap::from([("x".to_string(), 1.0)]);
        let r = personalized_pagerank(&files(&["p", "q", "x", "y"]), &edges, &seeds);
        let top: Vec<&str> = r.iter().take(2).map(|x| x.0.as_str()).collect();
        assert!(top.contains(&"x") && top.contains(&"y"), "{r:?}");
        assert!((sum(&r) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn unknown_seeds_and_edges_are_ignored() {
        let seeds = BTreeMap::from([("ghost".to_string(), 1.0)]);
        let r = personalized_pagerank(&files(&["a", "b"]), &[e("a", "ghost"), e("a", "a")], &seeds);
        assert_eq!(r.len(), 2);
        assert!((r[0].1 - r[1].1).abs() < 1e-9, "uniform fallback: {r:?}");
    }

    #[test]
    fn parallel_edges_are_summed() {
        let single = personalized_pagerank(
            &files(&["a", "b", "c"]),
            &[
                Edge {
                    weight: 2.0,
                    ..e("a", "b")
                },
                e("a", "c"),
            ],
            &BTreeMap::new(),
        );
        let doubled = personalized_pagerank(
            &files(&["a", "b", "c"]),
            &[e("a", "b"), e("a", "b"), e("a", "c")],
            &BTreeMap::new(),
        );
        for (x, y) in single.iter().zip(&doubled) {
            assert_eq!(x.0, y.0);
            assert!((x.1 - y.1).abs() < 1e-12);
        }
    }

    #[test]
    fn deterministic_across_runs() {
        let fs = files(&["a", "b", "c", "d"]);
        let es = [e("a", "b"), e("b", "c"), e("c", "a"), e("d", "a")];
        let seeds = BTreeMap::from([("d".to_string(), 1.0)]);
        assert_eq!(
            personalized_pagerank(&fs, &es, &seeds),
            personalized_pagerank(&fs, &es, &seeds)
        );
    }
}
