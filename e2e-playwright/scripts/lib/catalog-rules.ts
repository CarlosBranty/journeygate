/**
 * Pure rules behind scripts/catalog-lint.ts (docs/contracts/journeys-catalog.md).
 * No I/O here: the CLI reads files and runs Playwright, these functions judge.
 */

export const JOURNEY_ID_PATTERN = /^[a-z0-9]+(\.[a-z0-9-]+)+$/;
const TICKET_TAG_PATTERN = /^@ticket:\S+$/;

export interface Journey {
  id: string;
  area: string;
  title: string;
  impact_question: string;
  paths: string[];
  smoke: boolean;
  quarantined: boolean;
}

export interface Catalog {
  version: 1;
  journeys: Journey[];
}

export interface ListedTest {
  title: string;
  /** Relative to Playwright's testDir (the project root), e.g. `tests/journeys/checkout/customer-card.spec.ts`. */
  file: string;
  line: number;
  /** Normalised to the `@` form. */
  tags: string[];
}

const TOP_LEVEL_KEYS = new Set(['$schema', 'version', 'journeys']);
const JOURNEY_KEYS = ['id', 'area', 'title', 'impact_question', 'paths', 'smoke', 'quarantined'] as const;

const isObject = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isNonEmptyString = (v: unknown): v is string => typeof v === 'string' && v.length > 0;

/** Structural validation of catalog/journeys.json (mirrors schemas/journeys.schema.json). */
export function parseCatalog(doc: unknown): { catalog: Catalog | undefined; errors: string[] } {
  const errors: string[] = [];
  if (!isObject(doc)) return { catalog: undefined, errors: ['catalog must be a JSON object'] };

  for (const key of Object.keys(doc)) {
    if (!TOP_LEVEL_KEYS.has(key)) errors.push(`catalog has unknown top-level key "${key}"`);
  }
  if (doc.version !== 1) errors.push(`catalog version must be 1, got ${JSON.stringify(doc.version)}`);
  if (!Array.isArray(doc.journeys)) {
    errors.push('catalog "journeys" must be an array');
    return { catalog: undefined, errors };
  }

  const seen = new Set<string>();
  doc.journeys.forEach((j: unknown, index) => {
    const where = `journeys[${index}]`;
    if (!isObject(j)) {
      errors.push(`${where} must be an object`);
      return;
    }
    for (const key of Object.keys(j)) {
      if (!(JOURNEY_KEYS as readonly string[]).includes(key)) errors.push(`${where} has unknown key "${key}"`);
    }
    if (typeof j.id !== 'string' || !JOURNEY_ID_PATTERN.test(j.id)) {
      errors.push(`${where} id ${JSON.stringify(j.id)} must match ${JOURNEY_ID_PATTERN.source}`);
    } else if (seen.has(j.id)) {
      errors.push(`${where} duplicate id "${j.id}"`);
    } else {
      seen.add(j.id);
    }
    for (const key of ['area', 'title', 'impact_question'] as const) {
      if (!isNonEmptyString(j[key])) errors.push(`${where} ${key} must be a non-empty string`);
    }
    if (!Array.isArray(j.paths) || !j.paths.every(isNonEmptyString)) {
      errors.push(`${where} paths must be an array of non-empty strings`);
    }
    for (const key of ['smoke', 'quarantined'] as const) {
      if (typeof j[key] !== 'boolean') errors.push(`${where} ${key} must be a boolean`);
    }
  });

  return { catalog: errors.length === 0 ? (doc as unknown as Catalog) : undefined, errors };
}

/**
 * Keys of `[areas.<key>]` tables. A deliberately small reader: only standard
 * table headers at the start of a line count; comments and strings do not.
 */
export function areaKeysFromToml(toml: string): Set<string> {
  const keys = new Set<string>();
  const header = /^\s*\[\s*areas\s*\.\s*(?:"([^"]+)"|([A-Za-z0-9_-]+))\s*\]\s*(?:#.*)?$/;
  for (const line of toml.split(/\r?\n/)) {
    const m = header.exec(line);
    const key = m?.[1] ?? m?.[2];
    if (key) keys.add(key);
  }
  return keys;
}

export function normalizeTag(tag: string): string {
  return tag.startsWith('@') ? tag : `@${tag}`;
}

interface JsonSpec {
  title?: string;
  file?: string;
  line?: number;
  tags?: string[];
  tests?: { projectName?: string }[];
}

interface JsonSuite {
  specs?: JsonSpec[];
  suites?: JsonSuite[];
}

/**
 * Flattens `npx playwright test --list --reporter=json` output. Specs that
 * only run in the `setup` project are not journey tests and are skipped.
 */
export function collectListedTests(report: unknown): ListedTest[] {
  const root = report as { suites?: JsonSuite[]; errors?: { message?: string }[] };
  if (root.errors && root.errors.length > 0) {
    throw new Error(`playwright --list reported errors:\n${root.errors.map((e) => e.message ?? String(e)).join('\n')}`);
  }
  const out: ListedTest[] = [];
  const walk = (suite: JsonSuite) => {
    for (const spec of suite.specs ?? []) {
      const projects = (spec.tests ?? []).map((t) => t.projectName);
      if (projects.length > 0 && projects.every((p) => p === 'setup')) continue;
      out.push({
        title: spec.title ?? '',
        file: (spec.file ?? '').replace(/\\/g, '/'),
        line: spec.line ?? 0,
        tags: (spec.tags ?? []).map(normalizeTag),
      });
    }
    for (const child of suite.suites ?? []) walk(child);
  };
  for (const suite of root.suites ?? []) walk(suite);
  return out;
}

const isFeatureSpec = (file: string) => file.startsWith('features/') || file.startsWith('tests/features/');

/** Applies rules 1-5 of the catalog contract plus the target.toml area check. */
export function lintCatalog({
  catalog,
  tests,
  areas,
}: {
  catalog: Catalog;
  tests: ListedTest[];
  areas: ReadonlySet<string>;
}): string[] {
  const errors: string[] = [];
  const byId = new Map(catalog.journeys.map((j) => [j.id, j]));
  const covered = new Set<string>();

  for (const j of catalog.journeys) {
    if (!areas.has(j.area)) errors.push(`journey "${j.id}": area "${j.area}" is not an [areas.*] key in target.toml`);
  }

  for (const t of tests) {
    const where = `${t.file}:${t.line} "${t.title}"`;
    const journeyTags = t.tags.filter((tag) => tag.startsWith('@journey:'));
    const areaTags = t.tags.filter((tag) => tag.startsWith('@area:'));
    const hasSmoke = t.tags.includes('@smoke');

    if (journeyTags.length > 1) errors.push(`${where}: has more than one @journey tag (${journeyTags.join(', ')})`);

    for (const tag of journeyTags) {
      const id = tag.slice('@journey:'.length);
      const j = byId.get(id);
      if (!j) {
        errors.push(`${where}: ${tag} references "${id}", which is not in the catalog`);
        continue;
      }
      covered.add(id);
      const expectedArea = `@area:${j.area}`;
      if (areaTags.length !== 1 || areaTags[0] !== expectedArea) {
        errors.push(`${where}: expected exactly one ${expectedArea} tag, got [${areaTags.join(', ')}]`);
      }
      if (hasSmoke !== j.smoke) {
        errors.push(`${where}: @smoke must be ${j.smoke ? 'present' : 'absent'} because catalog smoke is ${j.smoke}`);
      }
    }

    if (isFeatureSpec(t.file) && !t.tags.some((tag) => TICKET_TAG_PATTERN.test(tag))) {
      errors.push(`${where}: specs under tests/features/** need at least one @ticket:<KEY> tag`);
    }
  }

  for (const j of catalog.journeys) {
    if (!j.quarantined && !covered.has(j.id)) {
      errors.push(`journey "${j.id}" is active but has no test tagged @journey:${j.id}`);
    }
  }
  return errors;
}
