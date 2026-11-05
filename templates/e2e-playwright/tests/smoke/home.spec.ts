import { expect, test } from '../../src/fixtures';

test.describe('Smoke: app is up', { tag: ['@smoke'] }, () => {
  test('the base URL answers and the page has a title', async ({ page, request }) => {
    const res = await request.get('/');
    expect(res.status()).toBeLessThan(400);
    await page.goto('/');
    await expect(page).toHaveTitle(/\S/);
  });
});
