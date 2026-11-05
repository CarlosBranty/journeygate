#!/usr/bin/env bash
# Reproduces the benchmark end to end (clones must already exist at the pinned
# commits listed in docs/benchmarks/impact-vs-codegraph.md, each indexed once
# with `codegraph init -y <clone>`).
# usage: scripts/run_all.sh <bench-clones-dir> <typescript-module-dir> <codegraph-package-dir>
set -euo pipefail
B="$1"; TS="$2"; CG="$3"
HERE="$(cd "$(dirname "$0")/.." && pwd)"; R="$HERE/results"; S="$HERE/scripts"
cd "$HERE"
cargo build --release
mkdir -p "$R"
# 1. Ground truth.
node "$S/gt_typescript.cjs" "$TS" "$B/hono" "$R/gt_hono.json"
python -I "$S/gt_python.py" "$B/rich" "$R/gt_rich.json"
(cd "$S/gogt" && go build -o "$B/_tools/gogt.exe" .)
(cd "$B/caddy" && GOOS=linux GOARCH=amd64 go list -e -json ./...) | "$B/_tools/gogt.exe" "$B/caddy" > "$R/gt_caddy.json"
(cd "$B/ripgrep" && cargo metadata --format-version 1 --no-deps) > "$B/_rg_meta.json"
python -I "$S/gt_rust.py" "$B/ripgrep" "$B/_rg_meta.json" "$R/gt_ripgrep.json"
# 2. Commit samples, tool outputs, timings, scores.
for nl in hono:typescript rich:python caddy:go ripgrep:rust; do
  n="${nl%%:*}"; l="${nl##*:}"
  (cd "$S" && python sample_commits.py "$B/$n" "$R/gt_$n.json" "$R/changesets_$n.json")
  target/release/impact-vs-codegraph.exe "$B/$n" "$l" "$R/changesets_$n.json" "$R/gi_$n.json"
  node --no-warnings "$S/cg_export.cjs" "$CG" "$B/$n" "$R/gt_$n.json" "$R/cg_edges_$n.json"
  node --no-warnings "$S/cg_query.cjs" "$CG" "$B/$n" "$R/changesets_$n.json" "$R/cg_$n.json"
done
bash "$S/timing.sh" "$B" "$R"
(cd "$S" && python -m unittest test_metrics && python score.py "$R")
