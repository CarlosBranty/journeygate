import { expect, test } from '../../src/fixtures';

test.describe('Smoke: app is up', { tag: ['@smoke'] }, () => {
  test('GET /healthz answers ok', async ({ request }) => {
    const res = await request.get('/healthz');
    expect(res.status()).toBe(200);
    expect(await res.json()).toEqual({ status: 'ok' });
  });
});
