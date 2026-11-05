"""Scores the C# and PHP extension of the benchmark (2026-11-01).

Same inputs, metrics and sampling as score.py (results/gt_<r>.json,
gi_<r>.json, cg_edges_<r>.json, cg_<r>.json, changesets_<r>.json), timings
from results/timing-csharp-php.tsv. Writes results/summary-csharp-php.json.

PRE-REGISTERED RULE, per language (fixed before any C#/PHP tool output was
produced): if guard-impact's metric-1 F1 or its metric-2 recall (mean
per-commit recall of the depth-3 dependents) is lower than CodeGraph's for that
language, the decision is codegraph_better_for_<lang>, otherwise
guard_impact_better_for_<lang>. See metrics.language_decision.

usage: python score_csharp_php.py <results-dir> [<timing.tsv> <summary.json>]
(both names relative to <results-dir>; defaults timing-csharp-php.tsv and
summary-csharp-php.json, the pre-registered run. The after-fixes re-run uses
timing-csharp-php-after-fixes.tsv and summary-csharp-php-after-fixes.json.)
"""

import json
import os
import re
import sys

from metrics import language_decision
from score import load_timing, score_repo

REPOS = [("eshop", "C#", "csharp"), ("symfony-console", "PHP", "php")]
# Context-only test-file heuristic for these two layouts (tests/ projects in
# eShop, Tests/ and *Test.php in symfony/console).
TEST_RE = re.compile(r"(^|/)[Tt]ests?/|(^|/)[^/]*\.(Unit|Functional)?Tests?/|Tests?\.(cs|php)$")


def main():
    res = sys.argv[1]
    timing_name, summary_name = (sys.argv[2:4] + [None, None])[:2]
    timing = load_timing(os.path.join(res, timing_name or "timing-csharp-php.tsv"))
    out = {"rule": language_decision.__doc__.strip(), "repos": {}, "decisions": {}}
    for repo, lang, key in REPOS:
        r = score_repo(res, repo, lang, timing, test_re=TEST_RE)
        out["repos"][repo] = r
        gi_f1, cg_f1 = r["metric1"]["guard_impact"]["f1"], r["metric1"]["codegraph"]["f1"]
        gi_rec, cg_rec = r["metric2"]["guard_impact"]["recall"], r["metric2"]["codegraph"]["recall"]
        out["decisions"][key] = {
            "decision": language_decision(key, gi_f1, cg_f1, gi_rec, cg_rec),
            "guard_impact_lower_f1": gi_f1 < cg_f1,
            "guard_impact_lower_recall": gi_rec < cg_rec,
        }
    with open(os.path.join(res, summary_name or "summary-csharp-php.json"), "w") as fh:
        json.dump(out, fh, indent=2)
    print(json.dumps(out["decisions"], indent=2))


if __name__ == "__main__":
    main()
