import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRouter } from '../src/router.mjs';

test('matches a static route by method and path', () => {
  const router = createRouter();
  const handler = () => {};
  router.add('GET', '/healthz', handler);
  const match = router.match('GET', '/healthz');
  assert.equal(match.handler, handler);
  assert.deepEqual(match.params, {});
});

test('does not match a different method', () => {
  const router = createRouter();
  router.add('GET', '/healthz', () => {});
  assert.equal(router.match('POST', '/healthz'), null);
});

test('extracts and decodes named params', () => {
  const router = createRouter();
  router.add('GET', '/products/:slug', () => {});
  const match = router.match('GET', '/products/blue%20mug');
  assert.deepEqual(match.params, { slug: 'blue mug' });
});

test('does not match when segment counts differ', () => {
  const router = createRouter();
  router.add('GET', '/products/:slug', () => {});
  assert.equal(router.match('GET', '/products'), null);
  assert.equal(router.match('GET', '/products/a/b'), null);
  assert.equal(router.match('GET', '/products/'), null);
});

test('malformed percent-encoding does not match', () => {
  const router = createRouter();
  router.add('GET', '/products/:slug', () => {});
  assert.equal(router.match('GET', '/products/%E0%A4%A'), null);
});

test('first registered route wins', () => {
  const router = createRouter();
  const first = () => {};
  router.add('GET', '/a/:x', first);
  router.add('GET', '/a/b', () => {});
  assert.equal(router.match('GET', '/a/b').handler, first);
});
