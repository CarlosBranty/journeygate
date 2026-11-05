#!/usr/bin/env bash
# Reproduces the C# and PHP extension (2026-11-01) end to end. The clones must
# already exist at the pinned commits listed in the report section
# "C# and PHP (2026-11-01)", each indexed once with `codegraph init -y`.
# usage: scripts/run_csharp_php.sh <bench-clones-dir> <codegraph-package-dir>
set -euo pipefail
B="$1"; CG="$2"
HERE="$(cd "$(dirname "$0")/.." && pwd)"; R="$HERE/results"; S="$HERE/scripts"
cd "$HERE"
cargo build --release
mkdir -p "$R"
# 0. Ground-truth tools pass their fixtures, scoring helpers pass their tests.
(cd "$S" && python -m unittest test_gt test_metrics)
# 1. Ground truth.
dotnet build gt_csharp/gt_csharp.csproj -c Release -v q -nologo
dotnet gt_csharp/bin/Release/net10.0/gt_csharp.dll "$B/eshop" "$R/gt_eshop.json"
php "$S/gt_php.php" "$B/symfony-console" "$R/gt_symfony-console.json"
# 2. Commit samples, tool outputs, timings, scores.
for nl in eshop:csharp symfony-console:php; do
  n="${nl%%:*}"; l="${nl##*:}"
  (cd "$S" && python sample_commits.py "$B/$n" "$R/gt_$n.json" "$R/changesets_$n.json")
  target/release/impact-vs-codegraph.exe "$B/$n" "$l" "$R/changesets_$n.json" "$R/gi_$n.json"
  node --no-warnings "$S/cg_export.cjs" "$CG" "$B/$n" "$R/gt_$n.json" "$R/cg_edges_$n.json"
  node --no-warnings "$S/cg_query.cjs" "$CG" "$B/$n" "$R/changesets_$n.json" "$R/cg_$n.json"
done
REPOS="eshop:csharp symfony-console:php" OUT=timing-csharp-php.tsv bash "$S/timing.sh" "$B" "$R"
(cd "$S" && python score_csharp_php.py "$R")
