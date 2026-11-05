#!/usr/bin/env bash
# Wall-time of a full index build, RUNS times per tool per repo (warm file cache).
#   guard-impact: the bench binary (in-process ImpactIndex::build time is also
#                 recorded by the binary itself as index_ms);
#   CodeGraph:    `codegraph index -q <repo>` (full rebuild, same as a fresh init).
# usage: scripts/timing.sh <bench-clones-dir> <results-dir>
# env: RUNS (3), REPOS ("name:language ..."), OUT (timing.tsv, inside <results-dir>)
set -euo pipefail
BENCH="$1"; RES="$2"; RUNS="${RUNS:-3}"
BIN="$(dirname "$0")/../target/release/impact-vs-codegraph.exe"
REPOS="${REPOS:-hono:typescript rich:python caddy:go ripgrep:rust}"
out="$RES/${OUT:-timing.tsv}"
printf 'repo\ttool\trun\twall_ms\n' > "$out"
for nl in $REPOS; do
  n="${nl%%:*}"; l="${nl##*:}"
  for r in $(seq 1 "$RUNS"); do
    s=$(date +%s%N); "$BIN" "$BENCH/$n" "$l" "$RES/changesets_$n.json" "$RES/gi_${n}_run.json"; e=$(date +%s%N)
    gi_in=$(python -I -c "import json,sys;print(json.load(open(sys.argv[1]))['index_ms'])" "$RES/gi_${n}_run.json")
    printf '%s\tguard-impact\t%s\t%s\n' "$n" "$r" "$(( (e - s) / 1000000 ))" >> "$out"
    printf '%s\tguard-impact-inproc\t%s\t%s\n' "$n" "$r" "$gi_in" >> "$out"
    s=$(date +%s%N); codegraph index -q "$BENCH/$n" > /dev/null 2>&1; e=$(date +%s%N)
    printf '%s\tcodegraph\t%s\t%s\n' "$n" "$r" "$(( (e - s) / 1000000 ))" >> "$out"
  done
  rm -f "$RES/gi_${n}_run.json"
done
cat "$out"
