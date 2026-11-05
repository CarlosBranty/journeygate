import { test as base } from '@playwright/test';
import { TestData } from '../data/test-data';
import { AccountPage, CheckoutPage, OrderConfirmationPage, ProductPage, Shell } from '../pages';
import { compileBlockedHosts, isBlockedHost } from './blocked-domains';
import { dataNamespace } from './namespace';
import { installSeededRandom, seedFromTestId } from './seeded-random';

type Fixtures = {
  data: TestData;
  shell: Shell;
  productPage: ProductPage;
  checkout: CheckoutPage;
  confirmation: OrderConfirmationPage;
  account: AccountPage;
};

const blockedHosts = compileBlockedHosts(process.env.GUARD_BLOCKED_DOMAINS);

export const test = base.extend<Fixtures>({
  // Every page of the context gets the seeded Math.random and the third-party block list.
  context: async ({ context }, use, testInfo) => {
    const seed = seedFromTestId(testInfo.testId);
    await context.addInitScript(installSeededRandom, seed);
    testInfo.annotations.push({ type: 'seed', description: String(seed) });
    if (blockedHosts.length > 0) {
      await context.route(
        (url) => isBlockedHost(url.hostname, blockedHosts),
        (route) => route.abort('blockedbyclient'),
      );
    }
    await use(context);
  },
  data: async ({ request }, use, testInfo) => {
    const namespace = dataNamespace({
      runId: process.env.RUN_ID,
      testId: testInfo.testId,
      repeatEachIndex: testInfo.repeatEachIndex,
      retry: testInfo.retry,
    });
    const data = new TestData(request, namespace, process.env.GUARD_TEST_TOKEN ?? '');
    await use(data);
    await data.cleanup();
  },
  shell: async ({ page }, use) => use(new Shell(page)),
  productPage: async ({ page }, use) => use(new ProductPage(page)),
  checkout: async ({ page }, use) => use(new CheckoutPage(page)),
  confirmation: async ({ page }, use) => use(new OrderConfirmationPage(page)),
  account: async ({ page }, use) => use(new AccountPage(page)),
});

export { expect } from '@playwright/test';
export { authStatePath } from './auth';
