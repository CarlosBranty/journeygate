// Ground truth for TypeScript/JavaScript repos: every import specifier found by
// the TypeScript compiler's own pre-processor (ts.preProcessFile: import/export
// from, import(), require(), triple-slash refs) resolved with
// ts.resolveModuleName under the compiler options of the tsconfig that owns the
// file (first parsed tsconfig whose fileNames include it, else the nearest
// ancestor tsconfig.json, else the repo-root tsconfig.base.json/tsconfig.json).
//
// usage: node gt_typescript.cjs <typescript-module-dir> <repo> <out.json>
const path = require('path');
const fs = require('fs');
const { execFileSync } = require('child_process');
const [, , tsDir, repo, out] = process.argv;
const ts = require(tsDir);

const norm = (p) => p.split(path.win32.sep).join('/');
const root = norm(path.resolve(repo));
const rel = (abs) => norm(path.relative(root, abs));
const EXT = /\.(ts|tsx|mts|cts|js|jsx|mjs|cjs)$/;
const tracked = execFileSync('git', ['-C', root, 'ls-files'], { encoding: 'utf8' })
  .split('\n').filter(Boolean).map(norm);
const universe = tracked.filter((f) => EXT.test(f) && !f.includes('node_modules/'));
const inUniverse = new Set(universe);

const configs = tracked.filter((f) => /(^|\/)tsconfig[^/]*\.json$/.test(f)).map((f) => {
  const abs = path.join(root, f);
  const parsed = ts.getParsedCommandLineOfConfigFile(abs, {}, {
    ...ts.sys, onUnRecoverableConfigFileDiagnostic: () => {},
  });
  return parsed && { file: f, options: parsed.options, names: new Set(parsed.fileNames.map((n) => rel(n))) };
}).filter(Boolean)
  // Shared base configs (no include list of their own) are the last resort.
  .sort((a, b) => Number(/base/.test(a.file)) - Number(/base/.test(b.file)));
const byFile = (f) => configs.find((c) => c.file === f);
function optionsFor(file) {
  const owner = configs.find((c) => c.names.has(file));
  if (owner) return owner;
  let dir = path.posix.dirname(file);
  while (true) {
    const c = byFile(dir === '.' ? 'tsconfig.json' : `${dir}/tsconfig.json`);
    if (c && c.names.size >= 0 && Object.keys(c.options).length > 1) return c;
    if (dir === '.' || dir === '') break;
    dir = path.posix.dirname(dir);
  }
  return byFile('tsconfig.base.json') || byFile('tsconfig.json') || { file: '(defaults)', options: { moduleResolution: ts.ModuleResolutionKind.Bundler, allowJs: true } };
}

const edges = [];
const stats = { specifiers: 0, resolvedInRepo: 0, configUse: {} };
for (const file of universe) {
  const abs = path.join(root, file);
  const text = fs.readFileSync(abs, 'utf8');
  const info = ts.preProcessFile(text, true, true);
  const cfg = optionsFor(file);
  stats.configUse[cfg.file] = (stats.configUse[cfg.file] || 0) + 1;
  const options = { ...cfg.options, allowJs: true };
  const targets = new Set();
  for (const imp of [...info.importedFiles, ...info.referencedFiles]) {
    stats.specifiers++;
    const r = ts.resolveModuleName(imp.fileName, abs, options, ts.sys).resolvedModule;
    if (!r) continue;
    const to = rel(r.resolvedFileName);
    if (to !== file && inUniverse.has(to)) targets.add(to);
  }
  for (const to of targets) { edges.push([file, to]); stats.resolvedInRepo++; }
}
fs.writeFileSync(out, JSON.stringify({ tool: `typescript ${ts.version} preProcessFile+resolveModuleName`, universe, edges, stats }));
console.log(`universe=${universe.length} edges=${edges.length} specifiers=${stats.specifiers}`);
console.log(JSON.stringify(stats.configUse));
