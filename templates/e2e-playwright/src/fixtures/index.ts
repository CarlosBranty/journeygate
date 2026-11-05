import { test as base } from '@playwright/test';
import { testTokenHeaders } from './auth';
import { compileBlockedHosts, isBlockedHost } from './blocked-domains';
import { TestData, seedPath } from './data';
import { dataNamespace } from './namespace';
import { installSeededRandom, seedFromTestId } from './seeded-random';

type Fixtures = {
  data: TestData;
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
    const data = new TestData(request, namespace, seedPath(process.env.GUARD_SEED_PATH), testTokenHeaders());
    await use(data);
    await data.cleanup();
  },
});

export { expect } from '@playwright/test';
export { authStatePath } from './auth';
export type { TestData } from './data';
