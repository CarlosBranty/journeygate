// CodeGraph side of the impact-selection metric, for each change set:
//  * depth3: depth-3 reverse BFS over CodeGraph's own file-level dependents
//    API (SDK getFileDependents, read-only), changed files excluded;
//    in-process time per query;
//  * cli: `codegraph affected --path <repo> --depth 3 --json <files...>`
//    (CodeGraph's test-selection command; returns test files only), with the
//    wall time of the CLI process.
//
// usage: node cg_query.cjs <codegraph-package-dir> <repo> <changesets.json> <out.json>
const fs = require('fs');
const { execFileSync } = require('child_process');
const [, , pkgDir, repo, setsPath, out] = process.argv;
const { CodeGraph } = require(pkgDir);
const slash = (p) => p.split('\\').join('/');

(async () => {
  const sets = JSON.parse(fs.readFileSync(setsPath, 'utf8'));
  const cg = await CodeGraph.open(repo, { readOnly: true, sync: false });
  const results = [];
  for (const set of sets) {
    const t0 = process.hrtime.bigint();
    const start = new Set(set.files);
    const seen = new Set(start);
    let frontier = [...start];
    for (let d = 0; d < 3 && frontier.length; d++) {
      const next = [];
      for (const f of frontier) {
        for (const dep of cg.getFileDependents(f)) {
          const p = slash(dep);
          if (!seen.has(p)) { seen.add(p); next.push(p); }
        }
      }
      frontier = next;
    }
    const queryUs = Number(process.hrtime.bigint() - t0) / 1e3;
    const depth3 = [...seen].filter((f) => !start.has(f)).sort();

    const t1 = process.hrtime.bigint();
    const raw = execFileSync('codegraph', ['affected', '--path', repo, '--depth', '3', '--json', ...set.files], {
      encoding: 'utf8', shell: true, maxBuffer: 64 * 1024 * 1024,
    });
    const cliMs = Number(process.hrtime.bigint() - t1) / 1e6;
    const parsed = JSON.parse(raw.slice(raw.indexOf('{')));
    results.push({ id: set.id, depth3, query_us: queryUs, cli: (parsed.affectedTests || []).map(slash), cli_ms: cliMs });
  }
  cg.close();
  fs.writeFileSync(out, JSON.stringify(results));
  console.log(`sets=${results.length}`);
})().catch((e) => { console.error(e); process.exit(1); });
