import { defineConfig } from 'vitest/config';

// Unit tests for pure helpers only. Browser specs (*.spec.ts) belong to Playwright.
export default defineConfig({
  test: {
    include: ['src/**/*.test.ts', 'scripts/**/*.test.ts'],
    environment: 'node',
  },
});
