import { authStatePath, test } from '../../../src/fixtures';

test.describe(
  'Account: customer session',
  { tag: ['@journey:account.customer-session', '@area:account'] },
  () => {
    test.describe('signed in', () => {
      test.use({ storageState: authStatePath('customer') });

      test('shows the customer email and role', async ({ account }) => {
        await account.goto();
        await account.expectSignedInAs('customer');
      });
    });

    test.describe('signed out', () => {
      test.use({ storageState: { cookies: [], origins: [] } });

      test('asks the visitor to sign in', async ({ account }) => {
        await account.goto();
        await account.expectSignInRequired();
      });
    });
  },
);
