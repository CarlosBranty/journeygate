// Tests of spec-quality.mjs (docs/contracts/qa-coder-protocol.md, "Deterministic
// test-quality checks"). Run: node --test guard-sandbox/assets/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { checkSpec } from './spec-quality.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const opts = { ticket: 'SHOP-1', apiBases: ['/api/'] };
const ids = (src, o = opts) => checkSpec(src, o).map((v) => v.id);

const GOOD = `import { test, expect } from '@playwright/test';
test('adds an item @ticket:SHOP-1', async ({ page }) => {
  await page.goto('/cart');
  await expect(page.getByTestId('total')).toHaveText('2');
});
`;

test('a well-formed spec has no violations', () => {
  assert.deepEqual(checkSpec(GOOD, opts), []);
});

test('no_focus rejects test.only, describe.only, it.only, fit, fdescribe', () => {
  for (const callee of ['test.only', 'test.describe.only', 'it.only', 'fit', 'fdescribe', 'describe.only']) {
    const src = `${callee}('t @ticket:SHOP-1', async () => { expect(a).toBe(1); });`;
    assert.ok(ids(src).includes('no_focus'), callee);
  }
});

test('no_skip rejects skip and fixme variants, conditional skip and x-prefixed', () => {
  const cases = [
    `test.skip('t @ticket:SHOP-1', async () => { expect(a).toBe(1); });`,
    `test.fixme('t @ticket:SHOP-1', async () => { expect(a).toBe(1); });`,
    `test.describe.skip('d', () => {});`,
    `describe.skip('d', () => {});`,
    `xit('t', () => {});`,
    `xdescribe('d', () => {});`,
    `test('t @ticket:SHOP-1', async ({ browserName }) => { test.skip(browserName === 'webkit'); expect(a).toBe(1); });`,
  ];
  for (const src of cases) assert.ok(ids(src).includes('no_skip'), src);
});

test('no_empty_test rejects a test whose callback has no statements', () => {
  assert.ok(ids(`test('t @ticket:SHOP-1', async () => {});`).includes('no_empty_test'));
});

test('has_expect counts expect calls, including inside test.step', () => {
  assert.ok(ids(`test('t @ticket:SHOP-1', async ({ page }) => { await page.goto('/'); });`).includes('has_expect'));
  const stepped = `test('t @ticket:SHOP-1', async ({ page }) => {
    await test.step('open', async () => { await expect(page).toHaveURL('/'); });
  });`;
  assert.ok(!ids(stepped).includes('has_expect'));
  const soft = `test('t @ticket:SHOP-1', async ({ page }) => { expect.soft(await page.title()).toBe('x'); });`;
  assert.ok(!ids(soft).includes('has_expect'));
});

test('no_trivial_expect rejects expect on a literal', () => {
  for (const lit of ['true', '1', "'a'", 'null', '`x`', '-1']) {
    const src = `test('t @ticket:SHOP-1', async () => { expect(${lit}).toBe(${lit}); });`;
    assert.ok(ids(src).includes('no_trivial_expect'), lit);
  }
  assert.ok(!ids(GOOD).includes('no_trivial_expect'));
});

test('no_wait_for_timeout rejects any waitForTimeout call', () => {
  const src = `test('t @ticket:SHOP-1', async ({ page }) => { await page.waitForTimeout(500); await expect(page).toHaveURL('/'); });`;
  assert.ok(ids(src).includes('no_wait_for_timeout'));
  const frame = `test('t @ticket:SHOP-1', async ({ page }) => { await page.mainFrame().waitForTimeout(1); await expect(page).toHaveURL('/'); });`;
  assert.ok(ids(frame).includes('no_wait_for_timeout'));
});

test('no_self_mock rejects page/context.route on the app API, allows third parties', () => {
  const mk = (recv, pat) => `test('t @ticket:SHOP-1', async ({ page, context }) => {
    await ${recv}.route(${pat}, (r) => r.fulfill({ body: '{}' }));
    await expect(page).toHaveURL('/');
  });`;
  assert.ok(ids(mk('page', "'**/api/cart'")).includes('no_self_mock'));
  assert.ok(ids(mk('context', '/\\/api\\/cart/')).includes('no_self_mock'));
  assert.ok(ids(mk('page', "'http://localhost:3000/cart'"), { ...opts, apiBases: ['/api/', 'http://localhost:3000'] }).includes('no_self_mock'));
  assert.ok(!ids(mk('page', "'https://maps.example.com/**'")).includes('no_self_mock'));
});

test('no_swallowed_assertions rejects try/expect/catch without rethrow', () => {
  const swallowed = `test('t @ticket:SHOP-1', async ({ page }) => {
    try { await expect(page).toHaveURL('/'); } catch (e) { console.log(e); }
  });`;
  assert.ok(ids(swallowed).includes('no_swallowed_assertions'));
  const rethrown = `test('t @ticket:SHOP-1', async ({ page }) => {
    try { await expect(page).toHaveURL('/'); } catch (e) { await page.screenshot(); throw e; }
  });`;
  assert.ok(!ids(rethrown).includes('no_swallowed_assertions'));
});

test('ticket_tag requires @ticket:<KEY> on each test, by title, tag option or describe', () => {
  assert.ok(ids(`test('no tag', async () => { expect(a).toBe(1); });`).includes('ticket_tag'));
  assert.ok(!ids(`test('t', { tag: '@ticket:SHOP-1' }, async () => { expect(a).toBe(1); });`).includes('ticket_tag'));
  assert.ok(!ids(`test('t', { tag: ['@journey:checkout', '@ticket:SHOP-1'] }, async () => { expect(a).toBe(1); });`).includes('ticket_tag'));
  assert.ok(!ids(`test.describe('cart @ticket:SHOP-1', () => { test('t', async () => { expect(a).toBe(1); }); });`).includes('ticket_tag'));
  assert.ok(ids(`test('t @ticket:OTHER-9', async () => { expect(a).toBe(1); });`).includes('ticket_tag'));
});

test('journey_tag_format rejects tags that are not @ticket:<KEY> or @journey:<id>', () => {
  assert.ok(ids(`test('t @ticket:SHOP-1 @smoke', async () => { expect(a).toBe(1); });`).includes('journey_tag_format'));
  assert.ok(ids(`test('t', { tag: ['@ticket:SHOP-1', 'nope'] }, async () => { expect(a).toBe(1); });`).includes('journey_tag_format'));
  assert.ok(!ids(`test('t @ticket:SHOP-1 @journey:checkout.pay', async () => { expect(a).toBe(1); });`).includes('journey_tag_format'));
});

test('a syntax error is reported as parse_error', () => {
  assert.ok(ids(`test('t @ticket:SHOP-1', async () => { expect(1 + ; });`).includes('parse_error'));
});

test('violations carry 1-based line numbers', () => {
  const v = checkSpec(`\n\ntest.only('t @ticket:SHOP-1', async () => { expect(a).toBe(1); });`, opts);
  assert.equal(v.find((x) => x.id === 'no_focus').line, 3);
});

test('CLI reads stdin and prints JSON', () => {
  const r = spawnSync(process.execPath, [path.join(here, 'spec-quality.mjs'), '--ticket', 'SHOP-1', '--api-base', '/api/'], {
    input: `test.only('t @ticket:SHOP-1', async () => { expect(a).toBe(1); });`,
    encoding: 'utf8',
  });
  assert.equal(r.status, 0, r.stderr);
  const out = JSON.parse(r.stdout);
  assert.deepEqual(out.violations.map((v) => v.id), ['no_focus']);
});

test('CLI without --ticket exits 2', () => {
  const r = spawnSync(process.execPath, [path.join(here, 'spec-quality.mjs')], { input: '', encoding: 'utf8' });
  assert.equal(r.status, 2);
});

test('no_host_access rejects Node built-ins, process, eval and dynamic code', () => {
  const bad = [
    `import { execSync } from 'node:child_process';`,
    `import fs from 'fs';`,
    `import * as net from 'node:net';`,
    `const cp = require('child_process');`,
    `const m = await import('node:os');`,
    `const k = process.env.LITELLM_MASTER_KEY;`,
    `const k = globalThis.process;`,
    `eval('1');`,
    `const f = new Function('return 1');`,
    `const m = await import(name);`,
  ];
  for (const line of bad) {
    const src = `${line}\n${GOOD}`;
    assert.ok(ids(src).includes('no_host_access'), line);
  }
  const fine = [
    `import { test as base } from '@playwright/test';`,
    `import { CartPage } from '../pages/cart';`,
    `import { faker } from '@faker-js/faker';`,
  ];
  for (const line of fine) {
    assert.ok(!ids(`${line}\n${GOOD}`).includes('no_host_access'), line);
  }
});
