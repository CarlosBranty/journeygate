//! Structured records of where two backends disagree about impact.
//!
//! Two comparisons:
//! - [`compare_impact`]: depth-limited reverse reachability (who is impacted
//!   by a change to `seed`) over the edges of any two [`GraphSource`]s.
//! - [`compare_affected`]: CodeGraph's own `affected` test selection against
//!   guard-impact's transitive importers restricted to test-like paths.
//!
//! Every disagreement becomes one record, returned to the caller and logged
//! through `tracing` (target [`LOG_TARGET`], one JSON `record` field) so it can
//! be collected without parsing prose. Agreement produces no record.

use crate::codegraph::CodeGraphSource;
use crate::impact_source::GuardImpactSource;
use crate::{Edge, GraphSource, MapError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const LOG_TARGET: &str = "guard_map::discrepancy";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscrepancyKind {
    /// Reverse reachability over both graphs differs.
    Impact,
    /// CodeGraph's `affected` tests differ from guard-impact's.
    Affected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Discrepancy {
    pub kind: DiscrepancyKind,
    /// The changed file the comparison starts from.
    pub seed: String,
    /// Hops followed (1 = direct dependents).
    pub depth: u32,
    pub left: String,
    pub right: String,
    /// Sorted.
    pub only_left: Vec<String>,
    /// Sorted.
    pub only_right: Vec<String>,
    pub common: u32,
    /// Files whose dependents a backend could not list in full (CodeGraph's
    /// `+K more`), mapped to the hidden count; `only_left` entries may be
    /// explained by them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub right_truncated: BTreeMap<String, u32>,
}

impl Discrepancy {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("Discrepancy serializes")
    }
}

/// One JSON object per line.
pub fn to_jsonl(records: &[Discrepancy]) -> String {
    records.iter().map(|r| r.to_json() + "\n").collect()
}

fn log(records: &[Discrepancy]) {
    for r in records {
        tracing::warn!(
            target: LOG_TARGET,
            kind = ?r.kind,
            seed = %r.seed,
            only_left = r.only_left.len(),
            only_right = r.only_right.len(),
            record = %r.to_json(),
            "backend discrepancy"
        );
    }
}

/// Files reaching `seed` through at most `depth` reversed edges (`seed` excluded).
pub fn reverse_reach(edges: &[Edge], seed: &str, depth: u32) -> BTreeSet<String> {
    let mut importers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in edges {
        importers
            .entry(e.to.as_str())
            .or_default()
            .push(e.from.as_str());
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut queue: VecDeque<(&str, u32)> = VecDeque::from([(seed, 0)]);
    while let Some((f, d)) = queue.pop_front() {
        if d == depth {
            continue;
        }
        for &i in importers.get(f).into_iter().flatten() {
            if i != seed && seen.insert(i) {
                queue.push_back((i, d + 1));
            }
        }
    }
    seen.into_iter().map(str::to_string).collect()
}

fn diff(
    kind: DiscrepancyKind,
    seed: &str,
    depth: u32,
    (left, l): (&str, &BTreeSet<String>),
    (right, r): (&str, &BTreeSet<String>),
    truncated: &BTreeMap<String, u32>,
) -> Option<Discrepancy> {
    let only_left: Vec<String> = l.difference(r).cloned().collect();
    let only_right: Vec<String> = r.difference(l).cloned().collect();
    if only_left.is_empty() && only_right.is_empty() {
        return None;
    }
    Some(Discrepancy {
        kind,
        seed: seed.to_string(),
        depth,
        left: left.to_string(),
        right: right.to_string(),
        only_left,
        only_right,
        common: u32::try_from(l.intersection(r).count()).unwrap_or(u32::MAX),
        right_truncated: truncated.clone(),
    })
}

/// Compares reverse reachability of each seed in `left` and `right`.
/// `right_truncated` comes from the right backend when it lists dependents
/// partially ([`CodeGraphSource::truncated`]); pass an empty map otherwise.
pub fn compare_impact(
    left: &dyn GraphSource,
    right: &dyn GraphSource,
    seeds: &[String],
    depth: u32,
    right_truncated: &BTreeMap<String, u32>,
) -> Result<Vec<Discrepancy>, MapError> {
    let (le, re) = (left.edges()?, right.edges()?);
    let records: Vec<Discrepancy> = seeds
        .iter()
        .filter_map(|seed| {
            let l = reverse_reach(&le, seed, depth);
            let r = reverse_reach(&re, seed, depth);
            // Only truncation inside the right side's walk can explain a gap.
            let relevant: BTreeMap<String, u32> = right_truncated
                .iter()
                .filter(|(p, _)| *p == seed || r.contains(*p))
                .map(|(p, n)| (p.clone(), *n))
                .collect();
            diff(
                DiscrepancyKind::Impact,
                seed,
                depth,
                (left.backend(), &l),
                (right.backend(), &r),
                &relevant,
            )
        })
        .collect();
    log(&records);
    Ok(records)
}

/// Heuristic test-path classifier shared by both sides of
/// [`compare_affected`]: `*.test.*`, `*.spec.*`, `*_test.go`, `test_*.py`,
/// `*_test.py`, or a `test`/`tests`/`__tests__`/`spec` directory.
pub fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let dirs = path.split('/').rev().skip(1);
    name.contains(".test.")
        || name.contains(".spec.")
        || name.ends_with("_test.go")
        || (name.ends_with(".py") && (name.starts_with("test_") || name.ends_with("_test.py")))
        || dirs
            .into_iter()
            .any(|d| matches!(d, "test" | "tests" | "__tests__" | "spec"))
}

/// Compares, per changed file, CodeGraph's `affected --json -d <depth>` test
/// selection (left: `codegraph`) with guard-impact's importers within `depth`
/// hops, plus the file itself, kept when [`is_test_path`] (right:
/// `guard-impact`). CodeGraph's selection is also filtered with
/// [`is_test_path`] so the two sides use one test definition.
pub fn compare_affected(
    impact: &GuardImpactSource,
    codegraph: &CodeGraphSource,
    changed: &[String],
    depth: u32,
) -> Result<Vec<Discrepancy>, MapError> {
    let edges = impact.edges()?;
    let mut records = Vec::new();
    for seed in changed {
        let cg: BTreeSet<String> = codegraph
            .affected(std::slice::from_ref(seed), depth)?
            .affected_tests
            .into_iter()
            .filter(|p| is_test_path(p))
            .collect();
        let mut gi = reverse_reach(&edges, seed, depth);
        gi.insert(seed.clone());
        gi.retain(|p| is_test_path(p));
        if let Some(d) = diff(
            DiscrepancyKind::Affected,
            seed,
            depth,
            (codegraph.backend(), &cg),
            (impact.backend(), &gi),
            &BTreeMap::new(),
        ) {
            records.push(d);
        }
    }
    log(&records);
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeGraphSource;

    fn e(from: &str, to: &str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            weight: 1.0,
        }
    }

    #[test]
    fn reverse_reach_respects_depth_and_cycles() {
        let edges = [e("b", "a"), e("c", "b"), e("d", "c"), e("a", "d")];
        let set = |d| {
            reverse_reach(&edges, "a", d)
                .into_iter()
                .collect::<Vec<_>>()
        };
        assert_eq!(set(0), Vec::<String>::new());
        assert_eq!(set(1), ["b"]);
        assert_eq!(set(3), ["b", "c", "d"]);
        assert_eq!(set(10), ["b", "c", "d"], "seed excluded despite the cycle");
    }

    #[test]
    fn impact_discrepancies_are_structured_and_agreement_is_silent() {
        let left = FakeGraphSource::new("guard-impact", "c")
            .edge("src/app.ts", "src/core.ts")
            .edge("src/cli.ts", "src/core.ts");
        let right = FakeGraphSource::new("codegraph", "c")
            .edge("src/app.ts", "src/core.ts")
            .edge("src/core.test.ts", "src/core.ts")
            .edge("src/other.ts", "src/other2.ts");
        let truncated = BTreeMap::from([
            ("src/core.ts".to_string(), 3),
            ("src/unrelated.ts".to_string(), 9),
        ]);
        let recs = compare_impact(
            &left,
            &right,
            &[
                "src/core.ts".into(),
                "src/other2.ts".into(),
                "src/none.ts".into(),
            ],
            3,
            &truncated,
        )
        .unwrap();
        assert_eq!(recs.len(), 2, "src/none.ts agrees (both empty)");
        let core = &recs[0];
        assert_eq!(core.kind, DiscrepancyKind::Impact);
        assert_eq!(core.only_left, ["src/cli.ts"]);
        assert_eq!(core.only_right, ["src/core.test.ts"]);
        assert_eq!(core.common, 1);
        assert_eq!(
            core.right_truncated,
            BTreeMap::from([("src/core.ts".to_string(), 3)])
        );
        assert_eq!(recs[1].only_right, ["src/other.ts"]);
        assert!(recs[1].right_truncated.is_empty());

        let line = to_jsonl(&recs[..1]);
        let back: Discrepancy = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(&back, core);
        assert!(line.contains("\"kind\":\"impact\""));
        assert!(!to_jsonl(&recs[1..]).contains("right_truncated"));
    }

    #[test]
    fn test_path_classifier() {
        for t in [
            "src/hono.test.ts",
            "src/a.spec.tsx",
            "pkg/x_test.go",
            "tests/test_console.py",
            "rich/foo_test.py",
            "adapters/aws/test/mock.ts",
            "src/__tests__/x.js",
        ] {
            assert!(is_test_path(t), "{t}");
        }
        for t in ["src/hono.ts", "src/testing.ts", "contest/x.go", "latest.py"] {
            assert!(!is_test_path(t), "{t}");
        }
    }
}
