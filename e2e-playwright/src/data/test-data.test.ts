import { describe, expect, it } from 'vitest';
import { TestData, type TestApi } from './test-data';

interface Call {
  method: string;
  url: string;
  data?: unknown;
  headers?: Record<string, string>;
}

function fakeApi(responses: Record<string, { status: number; body?: unknown }>) {
  const calls: Call[] = [];
  const respond = (method: string, url: string, options: { data?: unknown; headers?: Record<string, string> } = {}) => {
    calls.push({ method, url, ...options });
    const r = responses[`${method} ${url.split('?')[0]}`] ?? { status: 500, body: { error: 'unexpected' } };
    return Promise.resolve({
      status: () => r.status,
      ok: () => r.status >= 200 && r.status < 300,
      json: () => Promise.resolve(r.body),
      text: () => Promise.resolve(JSON.stringify(r.body ?? '')),
    });
  };
  const api: TestApi = {
    post: (url, options) => respond('POST', url, options),
    get: (url, options) => respond('GET', url, options),
    delete: (url, options) => respond('DELETE', url, options),
  };
  return { api, calls };
}

const NS = 'local-abc-x0-r0';
const seeded = {
  namespace: NS,
  products: [{ id: 'p_17', slug: `${NS}-p1`, name: 'Blue mug', priceCents: 1999, stock: 3 }],
};

describe('TestData', () => {
  it('seeds one product inside its namespace with the test token', async () => {
    const { api, calls } = fakeApi({ 'POST /__test__/seed': { status: 201, body: seeded } });
    const data = new TestData(api, NS, 'tok');

    const product = await data.product({ stock: 3, priceCents: 1999 });

    expect(product).toEqual(seeded.products[0]);
    expect(calls[0]).toEqual({
      method: 'POST',
      url: '/__test__/seed',
      headers: { 'x-test-token': 'tok' },
      data: { namespace: NS, products: [{ name: 'Blue mug', priceCents: 1999, stock: 3, slug: `${NS}-p1` }] },
    });
  });

  it('gives each product a distinct namespaced slug', async () => {
    const { api, calls } = fakeApi({ 'POST /__test__/seed': { status: 201, body: seeded } });
    const data = new TestData(api, NS, 'tok');
    await data.product({ name: 'A' });
    await data.product({ name: 'B' });
    const slugs = calls.map((c) => (c.data as { products: { slug: string }[] }).products[0]?.slug);
    expect(slugs).toEqual([`${NS}-p1`, `${NS}-p2`]);
  });

  it('fails loudly when seeding is rejected', async () => {
    const { api } = fakeApi({ 'POST /__test__/seed': { status: 401, body: { error: 'invalid test token' } } });
    await expect(new TestData(api, NS, '').product({})).rejects.toThrow(/seed failed: 401/);
  });

  it('lists orders for a product', async () => {
    const orders = [{ id: 'o_1', number: 'ORD-000001', status: 'confirmed' }];
    const { api, calls } = fakeApi({ 'GET /__test__/orders': { status: 200, body: { orders } } });
    expect(await new TestData(api, NS, 'tok').ordersFor('p_17')).toEqual(orders);
    expect(calls[0]?.url).toBe('/__test__/orders?productId=p_17');
    expect(calls[0]?.headers).toEqual({ 'x-test-token': 'tok' });
  });

  it('deletes its namespace on cleanup', async () => {
    const { api, calls } = fakeApi({ [`DELETE /__test__/seed/${NS}`]: { status: 204 } });
    await new TestData(api, NS, 'tok').cleanup();
    expect(calls[0]).toMatchObject({ method: 'DELETE', url: `/__test__/seed/${NS}` });
  });

  it('exposes the contract test cards', () => {
    const { api } = fakeApi({});
    const data = new TestData(api, NS, 'tok');
    expect(data.cards.approved).toEqual({ number: '4242 4242 4242 4242', expiry: '12/34', cvc: '123' });
    expect(data.cards.declined).toEqual({ number: '4000 0000 0000 0002', expiry: '12/34', cvc: '123' });
  });
});
