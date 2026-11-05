/**
 * Keeps catalog/journeys.json and the Playwright tags 1:1.
 *
 *   node scripts/catalog-lint.ts --target ../target.toml
 *
 * Run from the e2e-playwright directory (Node >= 24 strips the types).
 */
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { runCatalogLint } from './lib/catalog-lint-run.ts';

function listTests(): string {
  // Filters from guard must not hide tests from the lint.
  const env = { ...process.env };
  delete env.E2E_TAGS;
  delete env.GUARD_SHARD_PLAN;
  delete env.GUARD_SHARD_INDEX;
  // A constant command line through the shell resolves npx(.cmd) on every OS.
  const result = spawnSync('npx playwright test --list --reporter=json', {
    env,
    encoding: 'utf8',
    maxBuffer: 256 * 1024 * 1024,
    shell: true,
  });
  if (result.error) throw result.error;
  if (!result.stdout.trim()) {
    throw new Error(`playwright --list produced no output (exit ${result.status}): ${result.stderr}`);
  }
  return result.stdout;
}

process.exitCode = runCatalogLint(process.argv.slice(2), {
  readFile: (path) => readFileSync(path, 'utf8'),
  listTests,
  out: (line) => console.log(line),
  err: (line) => console.error(line),
});
