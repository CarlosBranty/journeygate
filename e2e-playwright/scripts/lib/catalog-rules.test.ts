import { describe, expect, it } from 'vitest';
import {
  areaKeysFromToml,
  collectListedTests,
  lintCatalog,
  normalizeTag,
  parseCatalog,
  type Catalog,
  type ListedTest,
} from './catalog-rules.ts';

const journey = (over: Partial<Catalog['journeys'][number]> = {}): Catalog['journeys'][number] => ({
  id: 'checkout.customer-card',
  area: 'checkout',
  title: 'Customer pays by card',
  impact_question: 'This diff can change checkout.',
  paths: [],
  smoke: true,
  quarantined: false,
  ...over,
});

const listed = (tags: string[], file = 'journeys/checkout/customer-card.spec.ts'): ListedTest => ({
  title: 'creates exactly one confirmed order',
  file,
  line: 10,
  tags,
});

const AREAS = new Set(['checkout', 'account']);

describe('parseCatalog', () => {
  it('accepts a valid catalog', () => {
    const { catalog, errors } = parseCatalog({ version: 1, journeys: [journey()] });
    expect(errors).toEqual([]);
    expect(catalog?.journeys).toHaveLength(1);
  });

  it('allows an optional $schema key', () => {
    expect(parseCatalog({ $schema: './x.json', version: 1, journeys: [] }).errors).toEqual([]);
  });

  it('rejects a wrong version and unknown top-level keys', () => {
    const { errors } = parseCatalog({ version: 2, journeys: [], extra: true });
    expect(errors).toEqual(expect.arrayContaining([expect.stringMatching(/version/), expect.stringMatching(/extra/)]));
  });

  it.each(['checkout', 'Checkout.card', 'checkout.card_x', 'checkout..card', '.checkout.card'])(
    'rejects the id %s',
    (id) => {
      expect(parseCatalog({ version: 1, journeys: [journey({ id })] }).errors.join('\n')).toMatch(/id/);
    },
  );

  it('accepts multi-segment ids with dashes and digits', () => {
    expect(parseCatalog({ version: 1, journeys: [journey({ id: 'billing2.invoice-pdf.v2' })] }).errors).toEqual([]);
  });

  it('rejects duplicate ids', () => {
    expect(parseCatalog({ version: 1, journeys: [journey(), journey()] }).errors.join('\n')).toMatch(/duplicate/);
  });

  it('rejects missing or mistyped fields', () => {
    const bad = { ...journey(), smoke: 'yes', title: '' } as unknown;
    const { errors } = parseCatalog({ version: 1, journeys: [bad] });
    expect(errors.join('\n')).toMatch(/smoke/);
    expect(errors.join('\n')).toMatch(/title/);
  });

  it('rejects a non-object document', () => {
    expect(parseCatalog(null).errors).not.toEqual([]);
  });
});

describe('areaKeysFromToml', () => {
  it('reads [areas.<key>] table headers', () => {
    const toml = [
      'version = 1',
      '[app]',
      'name = "demo"',
      '[areas.checkout]',
      'paths = ["a/**"]',
      '  [ areas.ui_general ]  # comment',
      '[areas."account"]',
      '[areas.checkout.extra]',
      '# [areas.commented]',
      'description = "[areas.inside_string]"',
    ].join('\n');
    expect([...areaKeysFromToml(toml)].sort()).toEqual(['account', 'checkout', 'ui_general']);
  });
});

describe('normalizeTag', () => {
  it('adds the leading @ when the reporter strips it', () => {
    expect(normalizeTag('smoke')).toBe('@smoke');
    expect(normalizeTag('@smoke')).toBe('@smoke');
  });
});

describe('collectListedTests', () => {
  it('flattens nested suites and ignores setup-only specs', () => {
    const report = {
      suites: [
        {
          suites: [
            {
              specs: [
                {
                  title: 'pays',
                  file: 'journeys/checkout/customer-card.spec.ts',
                  line: 7,
                  tags: ['journey:checkout.customer-card', '@smoke'],
                  tests: [{ projectName: 'chromium' }],
                },
              ],
            },
          ],
        },
        {
          specs: [
            {
              title: 'authenticate as customer',
              file: 'setup/auth.setup.ts',
              line: 3,
              tags: [],
              tests: [{ projectName: 'setup' }],
            },
          ],
        },
      ],
    };
    expect(collectListedTests(report)).toEqual([
      {
        title: 'pays',
        file: 'journeys/checkout/customer-card.spec.ts',
        line: 7,
        tags: ['@journey:checkout.customer-card', '@smoke'],
      },
    ]);
  });

  it('throws on top-level list errors', () => {
    expect(() => collectListedTests({ suites: [], errors: [{ message: 'SyntaxError in spec' }] })).toThrow(
      /SyntaxError in spec/,
    );
  });
});

describe('lintCatalog', () => {
  const run = (journeys: Catalog['journeys'], tests: ListedTest[], areas = AREAS) =>
    lintCatalog({ catalog: { version: 1, journeys }, tests, areas });

  it('passes a 1:1 catalog', () => {
    expect(run([journey()], [listed(['@journey:checkout.customer-card', '@area:checkout', '@smoke'])])).toEqual([]);
  });

  it('rule 1: every active journey has a test', () => {
    expect(run([journey()], []).join('\n')).toMatch(/checkout\.customer-card.*no test/);
  });

  it('rule 1: quarantined journeys may have no test', () => {
    expect(run([journey({ quarantined: true })], [])).toEqual([]);
  });

  it('rule 2: every @journey tag is in the catalog', () => {
    const errors = run([], [listed(['@journey:checkout.ghost', '@area:checkout'])]);
    expect(errors.join('\n')).toMatch(/checkout\.ghost.*not in the catalog/);
  });

  it('rule 3: at most one @journey tag per test', () => {
    const errors = run(
      [journey(), journey({ id: 'checkout.other', smoke: false })],
      [listed(['@journey:checkout.customer-card', '@journey:checkout.other', '@area:checkout', '@smoke'])],
    );
    expect(errors.join('\n')).toMatch(/more than one @journey/);
  });

  it('rule 3: the @area tag equals the catalog area', () => {
    expect(run([journey()], [listed(['@journey:checkout.customer-card', '@area:account', '@smoke'])]).join('\n')).toMatch(
      /@area:checkout/,
    );
    expect(run([journey()], [listed(['@journey:checkout.customer-card', '@smoke'])]).join('\n')).toMatch(
      /@area:checkout/,
    );
  });

  it('rule 4: @smoke iff the catalog says smoke', () => {
    expect(run([journey()], [listed(['@journey:checkout.customer-card', '@area:checkout'])]).join('\n')).toMatch(
      /@smoke/,
    );
    expect(
      run([journey({ smoke: false })], [listed(['@journey:checkout.customer-card', '@area:checkout', '@smoke'])]).join(
        '\n',
      ),
    ).toMatch(/@smoke/);
  });

  it('rule 4: non-journey smoke tests are allowed', () => {
    expect(run([], [listed(['@smoke'], 'smoke/healthz.spec.ts')])).toEqual([]);
  });

  it('rule 5: feature specs need a @ticket tag', () => {
    expect(run([], [listed([], 'features/shop-123.spec.ts')]).join('\n')).toMatch(/@ticket/);
    expect(run([], [listed(['@ticket:SHOP-123'], 'features/shop-123.spec.ts')])).toEqual([]);
    expect(run([], [listed([], 'tests/features/nested/a.spec.ts')]).join('\n')).toMatch(/@ticket/);
  });

  it('rejects a malformed @ticket tag', () => {
    expect(run([], [listed(['@ticket:'], 'features/x.spec.ts')]).join('\n')).toMatch(/@ticket/);
  });

  it('every catalog area exists in target.toml', () => {
    const errors = run([journey({ area: 'billing', id: 'billing.invoice', smoke: false })], [
      listed(['@journey:billing.invoice', '@area:billing']),
    ]);
    expect(errors.join('\n')).toMatch(/area "billing".*target\.toml/);
  });
});
