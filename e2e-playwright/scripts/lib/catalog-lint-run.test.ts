import { describe, expect, it } from 'vitest';
import { runCatalogLint, type LintIo } from './catalog-lint-run.ts';

const catalog = {
  version: 1,
  journeys: [
    {
      id: 'checkout.customer-card',
      area: 'checkout',
      title: 't',
      impact_question: 'q',
      paths: [],
      smoke: true,
      quarantined: false,
    },
  ],
};

const listJson = {
  suites: [
    {
      specs: [
        {
          title: 'pays',
          file: 'journeys/checkout/customer-card.spec.ts',
          line: 1,
          tags: ['@journey:checkout.customer-card', '@area:checkout', '@smoke'],
          tests: [{ projectName: 'chromium' }],
        },
      ],
    },
  ],
};

function io(files: Record<string, string>, list: unknown = listJson) {
  const out: string[] = [];
  const err: string[] = [];
  const value: LintIo = {
    readFile: (p) => {
      const content = files[p];
      if (content === undefined) throw new Error(`ENOENT ${p}`);
      return content;
    },
    listTests: () => JSON.stringify(list),
    out: (s) => out.push(s),
    err: (s) => err.push(s),
  };
  return { value, out, err };
}

const files = {
  'catalog/journeys.json': JSON.stringify(catalog),
  '../target.toml': '[areas.checkout]\npaths = []\n',
};

describe('runCatalogLint', () => {
  it('exits 0 when catalog and tests agree', () => {
    const { value, out } = io(files);
    expect(runCatalogLint(['--target', '../target.toml'], value)).toBe(0);
    expect(out.join('\n')).toMatch(/1 journey/);
  });

  it('exits 1 and prints every violation', () => {
    const { value, err } = io({ ...files, '../target.toml': '[areas.account]\n' });
    expect(runCatalogLint(['--target', '../target.toml'], value)).toBe(1);
    expect(err.join('\n')).toMatch(/area "checkout"/);
  });

  it('exits 1 on a structurally invalid catalog', () => {
    const { value, err } = io({ ...files, 'catalog/journeys.json': '{"version":2,"journeys":[]}' });
    expect(runCatalogLint(['--target', '../target.toml'], value)).toBe(1);
    expect(err.join('\n')).toMatch(/version/);
  });

  it('exits 2 without --target', () => {
    const { value, err } = io(files);
    expect(runCatalogLint([], value)).toBe(2);
    expect(err.join('\n')).toMatch(/--target/);
  });

  it('exits 2 when a file cannot be read or the list is broken', () => {
    expect(runCatalogLint(['--target', 'missing.toml'], io(files).value)).toBe(2);
    expect(runCatalogLint(['--target', '../target.toml'], io(files, { errors: [{ message: 'boom' }] }).value)).toBe(2);
  });

  it('reads a pre-computed list from --list-json instead of running Playwright', () => {
    const { value } = io({ ...files, 'list.json': JSON.stringify(listJson) });
    value.listTests = () => {
      throw new Error('should not run playwright');
    };
    expect(runCatalogLint(['--target', '../target.toml', '--list-json', 'list.json'], value)).toBe(0);
  });

  it('honours --catalog', () => {
    const { value } = io({ '../target.toml': files['../target.toml'], 'other.json': JSON.stringify(catalog) });
    expect(runCatalogLint(['--target', '../target.toml', '--catalog', 'other.json'], value)).toBe(0);
  });
});
