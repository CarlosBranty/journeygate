// Exports CodeGraph's file-level dependency edges through its public SDK
// (CodeGraph.open read-only, getFileDependencies), i.e. the "file-level
// projection of the symbol dependency graph" that CodeGraph documents as the
// basis for its blast-radius and `affected` selection.
//
// usage: node cg_export.cjs <codegraph-package-dir> <repo> <universe.json> <out.json>
// <universe.json> is a ground-truth file; its `universe` lists the files queried.
const fs = require('fs');
const [, , pkgDir, repo, gtPath, out] = process.argv;
const { CodeGraph } = require(pkgDir);
const slash = (p) => p.split('\\').join('/');

(async () => {
  const universe = JSON.parse(fs.readFileSync(gtPath, 'utf8')).universe;
  const t0 = process.hrtime.bigint();
  const cg = await CodeGraph.open(repo, { readOnly: true, sync: false });
  const openMs = Number(process.hrtime.bigint() - t0) / 1e6;
  const edges = [];
  const t1 = process.hrtime.bigint();
  for (const f of universe) {
    for (const to of cg.getFileDependencies(f)) {
      if (slash(to) !== f) edges.push([f, slash(to)]);
    }
  }
  const exportMs = Number(process.hrtime.bigint() - t1) / 1e6;
  cg.close();
  fs.writeFileSync(out, JSON.stringify({ edges, openMs, exportMs }));
  console.log(`queried=${universe.length} edges=${edges.length} openMs=${openMs.toFixed(1)} exportMs=${exportMs.toFixed(1)}`);
})().catch((e) => { console.error(e); process.exit(1); });
