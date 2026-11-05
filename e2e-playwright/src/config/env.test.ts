import { describe, expect, it } from 'vitest';
import {
  baseUrl,
  escapeRe,
  isCI,
  parseList,
  quarantineGrepInvert,
  reporters,
  shardPlanFiles,
  tagGrep,
  videoMode,
} from './env';

describe('parseList', () => {
  it('splits on commas and drops empty entries', () => {
    expect(parseList('a,,b,')).toEqual(['a', 'b']);
  });

  it('returns an empty list for unset or empty values', () => {
    expect(parseList(undefined)).toEqual([]);
    expect(parseList('')).toEqual([]);
  });
});

describe('escapeRe', () => {
  it('escapes every regex metacharacter', () => {
    const raw = 'a.b*c+?^$' + '{}()|[]\\';
    expect(new RegExp(`^${escapeRe(raw)}$`).test(raw)).toBe(true);
    expect(new RegExp(`^${escapeRe('a.b')}$`).test('axb')).toBe(false);
  });
});

describe('tagGrep', () => {
  // Playwright matches grep against the title path followed by the tags.
  const target = (tags: string[]) => `Checkout pays ${tags.join(' ')}`;
  const matches = (grep: RegExp[] | undefined, tags: string[]) =>
    grep === undefined || grep.some((re) => re.test(target(tags)));

  it('runs everything when E2E_TAGS is unset or empty', () => {
    expect(tagGrep(undefined)).toBeUndefined();
    expect(tagGrep('')).toBeUndefined();
  });

  it('builds one regex per tag', () => {
    expect(tagGrep('@smoke,@ticket:SHOP-123')).toHaveLength(2);
  });

  it('matches a whole tag only', () => {
    expect(matches(tagGrep('@ticket:SHOP-12'), ['@ticket:SHOP-123'])).toBe(false);
    expect(matches(tagGrep('@journey:checkout.card'), ['@journey:checkout.card-3ds'])).toBe(false);
    expect(matches(tagGrep('@journey:checkout.card'), ['@journey:checkout.card', '@smoke'])).toBe(true);
  });

  it('treats the dot in an id literally', () => {
    expect(matches(tagGrep('@journey:checkout.card'), ['@journey:checkoutXcard'])).toBe(false);
  });

  it('ORs the selected tags', () => {
    const grep = tagGrep('@journey:a.b,@smoke');
    expect(matches(grep, ['@smoke'])).toBe(true);
    expect(matches(grep, ['@journey:a.b'])).toBe(true);
    expect(matches(grep, ['@journey:a.c'])).toBe(false);
  });
});

describe('shardPlanFiles', () => {
  it('is undefined without GUARD_SHARD_PLAN', () => {
    expect(shardPlanFiles(undefined, () => '')).toBeUndefined();
    expect(shardPlanFiles('', () => '')).toBeUndefined();
  });

  it('reads the files list from the plan', () => {
    const read = (p: string) => {
      expect(p).toBe('/abs/plan.json');
      return JSON.stringify({ files: ['**/tests/journeys/checkout/customer-card.spec.ts'] });
    };
    expect(shardPlanFiles('/abs/plan.json', read)).toEqual(['**/tests/journeys/checkout/customer-card.spec.ts']);
  });

  it('rejects a plan without a non-empty string list', () => {
    expect(() => shardPlanFiles('/p', () => '{}')).toThrow(/GUARD_SHARD_PLAN/);
    expect(() => shardPlanFiles('/p', () => '{"files":[]}')).toThrow(/GUARD_SHARD_PLAN/);
    expect(() => shardPlanFiles('/p', () => '{"files":[1]}')).toThrow(/GUARD_SHARD_PLAN/);
    expect(() => shardPlanFiles('/p', () => 'not json')).toThrow(/GUARD_SHARD_PLAN/);
  });
});

describe('reporters', () => {
  it('uses json, html and list outside a gate shard', () => {
    expect(reporters(undefined)).toEqual([
      ['json', { outputFile: 'results/report.json' }],
      ['html', { open: 'never' }],
      ['list'],
    ]);
  });

  it('uses a numbered blob plus list inside a gate shard', () => {
    expect(reporters('3')).toEqual([['blob', { fileName: 'report-3.zip' }], ['list']]);
  });

  it('rejects a shard index that is not a positive integer', () => {
    expect(() => reporters('0')).toThrow(/GUARD_SHARD_INDEX/);
    expect(() => reporters('x')).toThrow(/GUARD_SHARD_INDEX/);
  });
});

describe('videoMode', () => {
  it('is off only for GUARD_VIDEO=off', () => {
    expect(videoMode('off')).toBe('off');
    expect(videoMode('on-first-retry')).toBe('on-first-retry');
    expect(videoMode(undefined)).toBe('on-first-retry');
  });
});

describe('isCI and baseUrl', () => {
  it('treats any non-empty CI as true', () => {
    expect(isCI('1')).toBe(true);
    expect(isCI('')).toBe(false);
    expect(isCI(undefined)).toBe(false);
  });

  it('defaults the base URL', () => {
    expect(baseUrl(undefined)).toBe('http://localhost:3000');
    expect(baseUrl('')).toBe('http://localhost:3000');
    expect(baseUrl('http://host.docker.internal:4000')).toBe('http://host.docker.internal:4000');
  });
});

describe('quarantineGrepInvert', () => {
  const catalog = JSON.stringify({
    version: 1,
    journeys: [
      { id: 'checkout.card', quarantined: true, smoke: true },
      { id: 'account.session', quarantined: false, smoke: false },
      { id: 'billing.pdf', quarantined: true, smoke: false },
    ],
  });
  const target = (tags: string[]) => `Some test ${tags.join(' ')}`;
  const excluded = (invert: RegExp[] | undefined, tags: string[]) =>
    invert !== undefined && invert.some((re) => re.test(target(tags)));

  it('excludes nothing when E2E_TAGS is unset (a manual run) or the catalog is absent', () => {
    expect(quarantineGrepInvert(undefined, catalog)).toBeUndefined();
    expect(quarantineGrepInvert('', catalog)).toBeUndefined();
    expect(quarantineGrepInvert('@smoke', undefined)).toBeUndefined();
  });

  it('excludes quarantined journeys reached through @smoke in --affected, --all and the gate', () => {
    const invert = quarantineGrepInvert('@journey:account.session,@smoke', catalog);
    expect(excluded(invert, ['@journey:checkout.card', '@smoke'])).toBe(true);
    expect(excluded(invert, ['@journey:billing.pdf'])).toBe(true);
    expect(excluded(invert, ['@journey:account.session'])).toBe(false);
    expect(excluded(invert, ['@smoke'])).toBe(false);
  });

  it('keeps a quarantined journey that was requested explicitly with --journey', () => {
    const invert = quarantineGrepInvert('@journey:checkout.card', catalog);
    expect(excluded(invert, ['@journey:checkout.card', '@smoke'])).toBe(false);
    expect(excluded(invert, ['@journey:billing.pdf'])).toBe(true);
  });

  it('matches whole tags only', () => {
    const invert = quarantineGrepInvert('@smoke', catalog);
    expect(excluded(invert, ['@journey:checkout.card-3ds'])).toBe(false);
  });

  it('is undefined when no journey is quarantined', () => {
    const none = JSON.stringify({ version: 1, journeys: [{ id: 'a.b', quarantined: false }] });
    expect(quarantineGrepInvert('@smoke', none)).toBeUndefined();
  });

  it('rejects an unreadable catalog', () => {
    expect(() => quarantineGrepInvert('@smoke', 'not json')).toThrow(/journeys\.json/);
    expect(() => quarantineGrepInvert('@smoke', '{}')).toThrow(/journeys\.json/);
  });
});
