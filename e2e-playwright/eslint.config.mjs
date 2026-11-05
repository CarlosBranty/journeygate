// Flat config. eslint-plugin-playwright enforces the determinism rules of
// section 10: no arbitrary waits, no skipped or focused tests, no raw selectors.
import playwright from 'eslint-plugin-playwright';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  { ignores: ['node_modules/', 'test-results/', 'playwright-report/', 'blob-report/', 'results/', '.auth/'] },
  ...tseslint.configs.recommended,
  {
    files: ['tests/**/*.ts', 'src/**/*.ts'],
    ignores: ['src/**/*.test.ts'],
    ...playwright.configs['flat/recommended'],
    rules: {
      ...playwright.configs['flat/recommended'].rules,
      'playwright/no-wait-for-timeout': 'error',
      'playwright/no-wait-for-selector': 'error',
      'playwright/no-skipped-test': ['error', { allowConditional: false }],
      'playwright/no-focused-test': 'error',
      'playwright/no-page-pause': 'error',
      'playwright/no-networkidle': 'error',
      'playwright/no-force-option': 'error',
      'playwright/no-raw-locators': 'error',
      'playwright/no-conditional-in-test': 'error',
      'playwright/prefer-web-first-assertions': 'error',
      // Assertions live in Page Objects and flows, so a test may only call helpers.
      'playwright/expect-expect': [
        'error',
        { assertFunctionNames: ['buyWithCard'], assertFunctionPatterns: ['^expect[A-Z]'] },
      ],
    },
  },
);
