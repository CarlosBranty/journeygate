import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { parseToml } from './support/mini-toml.mjs';
import { validate } from '../../../schemas/validate.mjs';

const read = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');
const schema = JSON.parse(read('../../../schemas/target.schema.json'));

test('mini TOML parser handles the subset target.toml uses', () => {
  const doc = parseToml(`version = 1 # c\n[a.b]\nx = "s#1"\ny = ['l', "m"]\nz = true\n`);
  assert.deepEqual(doc, { version: 1, a: { b: { x: 's#1', y: ['l', 'm'], z: true } } });
  assert.throws(() => parseToml('x = {}'));
});

test('target.toml is valid against schemas/target.schema.json', () => {
  const doc = parseToml(read('../../../target.toml'));
  assert.deepEqual(validate(schema, doc), []);
});

test('target.toml matches the demo-app contract', () => {
  const doc = parseToml(read('../../../target.toml'));
  assert.equal(doc.app.name, 'demo-shop');
  assert.equal(doc.app.start, 'node examples/demo-shop/server.mjs');
  assert.equal(doc.app.base_url, 'http://localhost:3000');
  assert.equal(doc.app.ready_path, '/healthz');
  assert.equal(doc.testability.token_env, 'GUARD_TEST_TOKEN');
  assert.deepEqual(doc.testability.roles, ['customer']);
  assert.deepEqual(Object.keys(doc.areas).sort(), ['account', 'catalog', 'checkout']);
  assert.equal(doc.areas.checkout.sensitive, true);
  assert.notEqual(doc.areas.catalog.sensitive, true);
  assert.notEqual(doc.areas.account.sensitive, true);
});

test('every area glob points at an existing source directory', async () => {
  const { existsSync } = await import('node:fs');
  const doc = parseToml(read('../../../target.toml'));
  for (const area of Object.values(doc.areas)) {
    for (const glob of area.paths) {
      const dir = glob.replace(/^examples\/demo-shop\//, '').replace(/\/\*\*$/, '');
      assert.ok(existsSync(new URL(`../${dir}`, import.meta.url)), glob);
    }
  }
});
