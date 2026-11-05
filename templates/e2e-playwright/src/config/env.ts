/**
 * Pure readers for the runner-env contract (docs/contracts/runner-env.md).
 * playwright.config.ts is the single consumer; every function takes the raw
 * variable value so it can be unit-tested without touching process.env.
 */
import type { ReporterDescription } from '@playwright/test';

export const DEFAULT_BASE_URL = 'http://localhost:3000';

/** Splits a comma-separated contract value; empty items are ignored. */
export function parseList(value: string | undefined): string[] {
  if (!value) return [];
  return value
    .split(',')
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

export function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * One RegExp per selected tag; each must match a whole tag, so
 * `@ticket:SHOP-12` never matches `@ticket:SHOP-123`. Undefined = no filter.
 */
export function tagGrep(e2eTags: string | undefined): RegExp[] | undefined {
  const tags = parseList(e2eTags);
  if (tags.length === 0) return undefined;
  return tags.map((tag) => new RegExp(`${escapeRe(tag)}(?=\\s|$)`));
}

/**
 * `grepInvert` for quarantined journeys (journeys-catalog.md: "excluded from
 * --affected, --all and the gate; still runnable with --journey <id>").
 *
 * guard drops a quarantined journey's `@journey:` tag from those selections,
 * but `@smoke` still reaches a quarantined smoke journey. So every quarantined
 * journey is inverted unless E2E_TAGS names its `@journey:<id>` tag, which only
 * `--journey <id>` does. Unset E2E_TAGS (a manual run) or no catalog = no filter.
 */
export function quarantineGrepInvert(
  e2eTags: string | undefined,
  catalogText: string | undefined,
): RegExp[] | undefined {
  const selected = new Set(parseList(e2eTags));
  if (selected.size === 0 || catalogText === undefined) return undefined;
  let catalog: unknown;
  try {
    catalog = JSON.parse(catalogText);
  } catch (error) {
    throw new Error(`catalog/journeys.json is not readable JSON: ${String(error)}`);
  }
  const journeys = (catalog as { journeys?: unknown } | null)?.journeys;
  if (!Array.isArray(journeys)) {
    throw new Error('catalog/journeys.json must contain a "journeys" array');
  }
  const excluded = journeys
    .filter((j): j is { id: string; quarantined: true } => {
      const entry = j as { id?: unknown; quarantined?: unknown } | null;
      return typeof entry?.id === 'string' && entry.quarantined === true;
    })
    .map((j) => `@journey:${j.id}`)
    .filter((tag) => !selected.has(tag));
  if (excluded.length === 0) return undefined;
  return excluded.map((tag) => new RegExp(`${escapeRe(tag)}(?=\\s|$)`));
}

/**
 * Reads `{"files": [...]}` from GUARD_SHARD_PLAN. Undefined = default testMatch.
 * The globs are already prefixed with `**` by guard.
 */
export function shardPlanFiles(
  planPath: string | undefined,
  read: (path: string) => string,
): string[] | undefined {
  if (!planPath) return undefined;
  let plan: unknown;
  try {
    plan = JSON.parse(read(planPath));
  } catch (error) {
    throw new Error(`GUARD_SHARD_PLAN ${planPath} is not readable JSON: ${String(error)}`);
  }
  const files = (plan as { files?: unknown } | null)?.files;
  if (!Array.isArray(files) || files.length === 0 || !files.every((f) => typeof f === 'string' && f.length > 0)) {
    throw new Error(`GUARD_SHARD_PLAN ${planPath} must contain a non-empty "files" array of strings`);
  }
  return files as string[];
}

/** Reporter matrix from the contract, keyed on GUARD_SHARD_INDEX. */
export function reporters(shardIndex: string | undefined): ReporterDescription[] {
  if (!shardIndex) {
    return [['json', { outputFile: 'results/report.json' }], ['html', { open: 'never' }], ['list']];
  }
  if (!/^[1-9]\d*$/.test(shardIndex)) {
    throw new Error(`GUARD_SHARD_INDEX must be a 1-based integer, got "${shardIndex}"`);
  }
  return [['blob', { fileName: `report-${shardIndex}.zip` }], ['list']];
}

export function videoMode(guardVideo: string | undefined): 'off' | 'on-first-retry' {
  return guardVideo === 'off' ? 'off' : 'on-first-retry';
}

export function isCI(ci: string | undefined): boolean {
  return !!ci;
}

export function baseUrl(targetBaseUrl: string | undefined): string {
  return targetBaseUrl || DEFAULT_BASE_URL;
}
