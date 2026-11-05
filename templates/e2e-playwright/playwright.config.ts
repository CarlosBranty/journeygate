/**
 * Single reader of the guard runner environment (docs/contracts/runner-env.md).
 * guard never builds regexes or config: it only sets the variables read below.
 */
import { existsSync, readFileSync } from 'node:fs';
import { defineConfig, devices } from '@playwright/test';
import {
  baseUrl,
  isCI,
  quarantineGrepInvert,
  reporters,
  shardPlanFiles,
  tagGrep,
  videoMode,
} from './src/config/env';

const env = process.env;
const ci = isCI(env.CI);
const grep = tagGrep(env.E2E_TAGS);
// Quarantined journeys stay out of selections that reach them only through @smoke.
const catalogPath = new URL('./catalog/journeys.json', import.meta.url);
const grepInvert = quarantineGrepInvert(
  env.E2E_TAGS,
  existsSync(catalogPath) ? readFileSync(catalogPath, 'utf8') : undefined,
);
const planFiles = shardPlanFiles(env.GUARD_SHARD_PLAN, (path) => readFileSync(path, 'utf8'));

// One fixed environment everywhere, so a failure means the app changed.
const deterministic = {
  timezoneId: 'UTC',
  locale: 'en-US',
  viewport: { width: 1280, height: 720 },
  deviceScaleFactor: 1,
  colorScheme: 'light',
  reducedMotion: 'reduce',
} as const;

export default defineConfig({
  // testDir is the project root so report `file` paths read tests/..., the same
  // paths guard uses in shard plans and .guard/durations.json (cli.md).
  testDir: '.',
  outputDir: './test-results',
  fullyParallel: true,
  forbidOnly: ci,
  retries: ci ? 2 : 0, // 3 attempts; a pass on retry is reported as flaky, never as clean green
  ...(ci ? { workers: 4 } : {}), // local runs keep Playwright's default (half the cores)
  reporter: reporters(env.GUARD_SHARD_INDEX),
  use: {
    baseURL: baseUrl(env.TARGET_BASE_URL),
    testIdAttribute: 'data-testid',
    ...deterministic,
    screenshot: 'only-on-failure',
    trace: 'on-first-retry', // heavy evidence only for tests that already failed once
    video: videoMode(env.GUARD_VIDEO),
  },
  projects: [
    // Never filtered by E2E_TAGS or the shard plan: every run needs its sessions.
    { name: 'setup', testMatch: /setup[\\/].*\.setup\.ts$/ },
    {
      name: 'chromium',
      dependencies: ['setup'],
      testMatch: planFiles ?? '**/*.spec.ts',
      ...(grep ? { grep } : {}),
      ...(grepInvert ? { grepInvert } : {}),
      use: { ...devices['Desktop Chrome'], ...deterministic },
    },
  ],
});
