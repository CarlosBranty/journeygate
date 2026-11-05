import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { startApp, TOKEN } from './support/harness.mjs';

let app;
before(async () => {
  app = await startApp();
});
after(() => app.close());

const ns = 'r1793452800-4242-a1b2c3-x0-r0';

test('test routes reject a missing or wrong token with 401', async () => {
  const c = app.client();
  for (const headers of [{}, { 'x-test-token': 'wrong' }]) {
    const res = await c.request('POST', '/__test__/login', { headers, json: { role: 'customer' } });
    assert.equal(res.status, 401);
    assert.deepEqual(res.json(), { error: 'invalid test token' });
    assert.match(res.headers.get('content-type'), /^application\/json; charset=utf-8$/);
  }
});

test('login returns the role and sets a session cookie', async () => {
  const c = app.client();
  const res = await c.testApi('POST', '/__test__/login', { json: { role: 'customer' } });
  assert.equal(res.status, 200);
  assert.deepEqual(res.json(), { role: 'customer', email: 'customer@demo.test' });
  assert.equal(res.setCookies.length, 1);
  assert.match(res.setCookies[0], /^sid=[0-9a-f]{32}; Path=\/; HttpOnly; SameSite=Lax$/);
});

test('login validates the body', async () => {
  const c = app.client();
  for (const body of ['not json', JSON.stringify({}), JSON.stringify({ role: 'Bad Role' }), JSON.stringify([])]) {
    const res = await c.testApi('POST', '/__test__/login', { body, headers: { 'content-type': 'application/json' } });
    assert.equal(res.status, 400, body);
    assert.equal(typeof res.json().error, 'string');
  }
});

test('seed creates products in request order and returns 201', async () => {
  const c = app.client();
  const res = await c.testApi('POST', '/__test__/seed', {
    json: {
      namespace: ns,
      products: [
        { slug: `${ns}-mug`, name: 'Blue mug', priceCents: 1999, stock: 3 },
        { name: 'Plain cup', priceCents: 0, stock: 0 },
      ],
    },
  });
  assert.equal(res.status, 201);
  const body = res.json();
  assert.equal(body.namespace, ns);
  assert.equal(body.products.length, 2);
  assert.deepEqual(Object.keys(body.products[0]).sort(), ['id', 'name', 'priceCents', 'slug', 'stock']);
  assert.equal(body.products[0].slug, `${ns}-mug`);
  assert.match(body.products[0].id, /^p_\d+$/);
  assert.match(body.products[1].slug, new RegExp(`^${ns}-p\\d+$`));
});

test('seed accepts an empty product list', async () => {
  const c = app.client();
  const res = await c.testApi('POST', '/__test__/seed', { json: { namespace: 'empty-ns', products: [] } });
  assert.equal(res.status, 201);
  assert.deepEqual(res.json(), { namespace: 'empty-ns', products: [] });
});

test('seed returns 409 on a slug in use and stores nothing', async () => {
  const c = app.client();
  await c.testApi('POST', '/__test__/seed', { json: { namespace: 'dup-a', products: [{ slug: 'dup-slug', name: 'A', priceCents: 1, stock: 1 }] } });
  const res = await c.testApi('POST', '/__test__/seed', {
    json: { namespace: 'dup-b', products: [{ slug: 'dup-b-new', name: 'B', priceCents: 1, stock: 1 }, { slug: 'dup-slug', name: 'C', priceCents: 1, stock: 1 }] },
  });
  assert.equal(res.status, 409);
  assert.deepEqual(res.json(), { error: 'slug already exists: dup-slug' });
  const page = await c.request('GET', '/products/dup-b-new');
  assert.equal(page.status, 404);
});

test('seed validates its input with 400', async () => {
  const c = app.client();
  const bad = [
    'nope',
    {},
    { namespace: 'bad ns', products: [] },
    { namespace: 'x'.repeat(201), products: [] },
    { namespace: 'ok' },
    { namespace: 'ok', products: [{ priceCents: 1, stock: 1 }] },
    { namespace: 'ok', products: [{ name: '', priceCents: 1, stock: 1 }] },
    { namespace: 'ok', products: [{ name: 'A', priceCents: -1, stock: 1 }] },
    { namespace: 'ok', products: [{ name: 'A', priceCents: 1.5, stock: 1 }] },
    { namespace: 'ok', products: [{ name: 'A', priceCents: 1, stock: '1' }] },
    { namespace: 'ok', products: [{ name: 'A', priceCents: 1 }] },
    { namespace: 'ok', products: [{ slug: 'bad slug', name: 'A', priceCents: 1, stock: 1 }] },
    { namespace: 'ok', products: [null] },
  ];
  for (const json of bad) {
    const res = await c.testApi('POST', '/__test__/seed', typeof json === 'string' ? { body: json } : { json });
    assert.equal(res.status, 400, JSON.stringify(json));
    assert.equal(typeof res.json().error, 'string');
  }
});

test('delete seed removes the namespace, is idempotent and validates the name', async () => {
  const c = app.client();
  const seeded = await c.testApi('POST', '/__test__/seed', {
    json: { namespace: 'del-ns', products: [{ slug: 'del-ns-a', name: 'A', priceCents: 1, stock: 1 }] },
  });
  assert.equal(seeded.status, 201);
  const del = await c.testApi('DELETE', '/__test__/seed/del-ns');
  assert.equal(del.status, 204);
  assert.equal(del.text, '');
  assert.equal((await c.request('GET', '/products/del-ns-a')).status, 404);
  assert.equal((await c.testApi('DELETE', '/__test__/seed/never-existed')).status, 204);
  assert.equal((await c.testApi('DELETE', '/__test__/seed/bad%20ns')).status, 400);
});

test('orders lookup requires productId and returns empty for unknown ids', async () => {
  const c = app.client();
  assert.equal((await c.testApi('GET', '/__test__/orders')).status, 400);
  assert.equal((await c.testApi('GET', '/__test__/orders?productId=')).status, 400);
  const res = await c.testApi('GET', '/__test__/orders?productId=p_999999');
  assert.equal(res.status, 200);
  assert.deepEqual(res.json(), { orders: [] });
});

test('every test route is 404 in production, even with a valid token', async () => {
  const prod = await startApp({ nodeEnv: 'production' });
  try {
    const c = prod.client();
    const calls = [
      ['POST', '/__test__/login', { json: { role: 'customer' } }],
      ['POST', '/__test__/seed', { json: { namespace: 'x', products: [] } }],
      ['DELETE', '/__test__/seed/x', {}],
      ['GET', '/__test__/orders?productId=p_1', {}],
    ];
    for (const [method, path, options] of calls) {
      const res = await c.testApi(method, path, options);
      assert.equal(res.status, 404, `${method} ${path}`);
    }
    assert.equal((await c.request('GET', '/healthz')).status, 200);
  } finally {
    await prod.close();
  }
});

test('the token comes from options', async () => {
  const custom = await startApp({ token: 'other' });
  try {
    const c = custom.client();
    const res = await c.request('POST', '/__test__/login', { headers: { 'x-test-token': TOKEN }, json: { role: 'customer' } });
    assert.equal(res.status, 401);
    const ok = await c.request('POST', '/__test__/login', { headers: { 'x-test-token': 'other' }, json: { role: 'customer' } });
    assert.equal(ok.status, 200);
  } finally {
    await custom.close();
  }
});
