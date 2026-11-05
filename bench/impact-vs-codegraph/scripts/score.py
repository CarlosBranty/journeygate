"""Scores both tools against the ground truth and applies the pre-registered rule.

Inputs per repo (results/): gt_<r>.json, gi_<r>.json (guard-impact bench
binary), cg_edges_<r>.json (cg_export.cjs), cg_<r>.json (cg_query.cjs),
changesets_<r>.json, timing.tsv.

PRE-REGISTERED DECISION RULE (fixed before any data was produced):
  macro-average (over the 4 repos) F1 of metric 1 and macro-average recall of
  metric 2 (primary affected set: depth-3 dependents); if guard-impact is lower
  than CodeGraph on EITHER, decision = guard_map_uses_codegraph, otherwise
  decision = guard_map_uses_guard_impact.

usage: python score.py <results-dir>
"""

import csv
import json
import os
import re
import statistics
import sys

from metrics import bounded_dependents, edge_scores, set_scores

REPOS = [("hono", "TypeScript"), ("rich", "Python"), ("caddy", "Go"), ("ripgrep", "Rust")]
DEPTH = 3
TEST_RE = re.compile(r"(^|/)(tests?|__tests__|testdata)/|_test\.go$|\.(test|spec)\.[cm]?[jt]sx?$|(^|/)test_[^/]*\.py$|_test\.py$|(^|/)conftest\.py$")


def mean(xs):
    xs = [x for x in xs if x is not None]
    return sum(xs) / len(xs) if xs else None


def micro(truths, preds):
    tp = sum(len(t & p) for t, p in zip(truths, preds))
    tot_t = sum(len(t) for t in truths)
    tot_p = sum(len(p) for p in preds)
    return (tp / tot_p if tot_p else None, tp / tot_t if tot_t else None)


def load(res, name):
    return json.load(open(os.path.join(res, name)))


def load_timing(path):
    timing = {}
    with open(path) as fh:
        for row in csv.DictReader(fh, delimiter="\t"):
            timing.setdefault((row["repo"], row["tool"]), []).append(int(row["wall_ms"]))
    return timing


def main():
    res = sys.argv[1]
    timing = load_timing(os.path.join(res, "timing.tsv"))
    out = {"repos": {}}
    for repo, lang in REPOS:
        out["repos"][repo] = score_repo(res, repo, lang, timing)
    finish(out, res)


def score_repo(res, repo, lang, timing, test_re=TEST_RE):
    """Metric 1, metric 2 (depth 3), test selection and timing medians of one repo."""
    gt, gi = load(res, f"gt_{repo}.json"), load(res, f"gi_{repo}.json")
    cge, cgq = load(res, f"cg_edges_{repo}.json"), load(res, f"cg_{repo}.json")
    sets = load(res, f"changesets_{repo}.json")
    universe = set(gt["universe"])
    m1 = {
        "guard_impact": edge_scores(gt["edges"], gi["edges"], universe),
        "codegraph": edge_scores(gt["edges"], cge["edges"], universe),
    }
    giq = {q["id"]: q for q in gi["queries"]}
    cgqm = {q["id"]: q for q in cgq}
    per = {"guard_impact": [], "codegraph": [], "guard_impact_unbounded": []}
    truths, preds = [], {k: [] for k in per}
    tests = {"guard_impact": [], "codegraph_cli": []}
    test_truths, test_preds = [], {k: [] for k in tests}
    for s in sets:
        truth = bounded_dependents(gt["edges"], s["files"], DEPTH)
        g = set(giq[s["id"]]["depth3"]) & universe
        gu = set(giq[s["id"]]["unbounded"]) & universe
        c = set(cgqm[s["id"]]["depth3"]) & universe
        truths.append(truth)
        for k, p in (("guard_impact", g), ("codegraph", c), ("guard_impact_unbounded", gu)):
            per[k].append(set_scores(truth, p))
            preds[k].append(p)
        t_truth = {f for f in truth if test_re.search(f)}
        if t_truth:
            cli = set(cgqm[s["id"]]["cli"]) & universe - set(s["files"])
            test_truths.append(t_truth)
            for k, p in (("guard_impact", {f for f in g if test_re.search(f)}), ("codegraph_cli", cli)):
                tests[k].append(set_scores(t_truth, p))
                test_preds[k].append(p)
    m2 = {}
    for k in per:
        mp, mr = micro(truths, preds[k])
        m2[k] = {
            "recall": mean(r["recall"] for r in per[k]),
            "precision": mean(r["precision"] for r in per[k]),
            "micro_recall": mr,
            "micro_precision": mp,
            "commits": len(per[k]),
            "empty_predictions": sum(1 for p in preds[k] if not p),
        }
    m2_tests = {}
    for k in tests:
        mp, mr = micro(test_truths, test_preds[k])
        m2_tests[k] = {
            "recall": mean(r["recall"] for r in tests[k]),
            "precision": mean(r["precision"] for r in tests[k]),
            "micro_recall": mr,
            "micro_precision": mp,
            "commits": len(tests[k]),
        }
    med = lambda key: statistics.median(timing.get((repo, key), [0]))
    return {
        "language": lang,
        "ground_truth_tool": gt["tool"],
        "universe_files": len(universe),
        "gt_edges": len({(a, b) for a, b in gt["edges"]}),
        "metric1": m1,
        "metric2": m2,
        "metric2_tests": m2_tests,
        "changed_files_mean": mean(len(s["files"]) for s in sets),
        "gt_dependents_mean": mean(len(t) for t in truths),
        "time": {
            "guard_impact_index_ms_median": med("guard-impact"),
            "guard_impact_index_inproc_ms_median": med("guard-impact-inproc"),
            "codegraph_index_ms_median": med("codegraph"),
            "guard_impact_query_us_median": statistics.median(q["query_us"] for q in gi["queries"]),
            "codegraph_sdk_query_us_median": statistics.median(q["query_us"] for q in cgq),
            "codegraph_cli_affected_ms_median": statistics.median(q["cli_ms"] for q in cgq),
        },
    }


def finish(out, res):
    rs = out["repos"].values()
    macro = {
        "f1": {t: mean(r["metric1"][t]["f1"] for r in rs) for t in ("guard_impact", "codegraph")},
        "impact_recall": {t: mean(r["metric2"][t]["recall"] for r in rs) for t in ("guard_impact", "codegraph")},
        "impact_precision": {t: mean(r["metric2"][t]["precision"] for r in rs) for t in ("guard_impact", "codegraph")},
    }
    lower_f1 = macro["f1"]["guard_impact"] < macro["f1"]["codegraph"]
    lower_recall = macro["impact_recall"]["guard_impact"] < macro["impact_recall"]["codegraph"]
    out["macro"] = macro
    out["decision"] = "guard_map_uses_codegraph" if (lower_f1 or lower_recall) else "guard_map_uses_guard_impact"
    out["decision_inputs"] = {"guard_impact_lower_f1": lower_f1, "guard_impact_lower_recall": lower_recall}
    json.dump(out, open(os.path.join(res, "summary.json"), "w"), indent=2)
    print(json.dumps({"macro": macro, "decision": out["decision"]}, indent=2))


if __name__ == "__main__":
    main()
