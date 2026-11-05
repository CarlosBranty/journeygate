import { parseArgs } from 'node:util';
import { areaKeysFromToml, collectListedTests, lintCatalog, parseCatalog } from './catalog-rules.ts';

/** Side effects the CLI needs, injected so the orchestration is unit-testable. */
export interface LintIo {
  readFile(path: string): string;
  /** Returns the stdout of `npx playwright test --list --reporter=json`. */
  listTests(): string;
  out(line: string): void;
  err(line: string): void;
}

const USAGE = 'usage: catalog-lint --target <path/to/target.toml> [--catalog catalog/journeys.json] [--list-json <file>]';

/** Exit codes: 0 = clean, 1 = violations, 2 = usage or infrastructure error. */
export function runCatalogLint(argv: string[], io: LintIo): number {
  let values: { target?: string | undefined; catalog?: string | undefined; 'list-json'?: string | undefined };
  try {
    ({ values } = parseArgs({
      args: argv,
      options: {
        target: { type: 'string' },
        catalog: { type: 'string' },
        'list-json': { type: 'string' },
      },
      strict: true,
    }));
  } catch (error) {
    io.err(`${String(error)}\n${USAGE}`);
    return 2;
  }
  if (!values.target) {
    io.err(`missing --target\n${USAGE}`);
    return 2;
  }

  const catalogPath = values.catalog ?? 'catalog/journeys.json';
  let catalogDoc: unknown;
  let areas: Set<string>;
  let tests;
  try {
    catalogDoc = JSON.parse(io.readFile(catalogPath));
    areas = areaKeysFromToml(io.readFile(values.target));
    const listJson = values['list-json'] ? io.readFile(values['list-json']) : io.listTests();
    tests = collectListedTests(JSON.parse(listJson));
  } catch (error) {
    io.err(`catalog-lint: ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }

  const { catalog, errors: structural } = parseCatalog(catalogDoc);
  const errors = catalog ? lintCatalog({ catalog, tests, areas }) : structural;
  if (errors.length > 0) {
    for (const e of errors) io.err(`catalog-lint: ${e}`);
    io.err(`catalog-lint: ${errors.length} violation(s)`);
    return 1;
  }
  io.out(`catalog-lint: ok (${catalog?.journeys.length ?? 0} journey(s), ${tests.length} test(s))`);
  return 0;
}
