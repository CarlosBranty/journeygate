import { expect, type Locator, type Page } from '@playwright/test';

export class AccountPage {
  readonly heading: Locator;
  readonly email: Locator;
  readonly role: Locator;
  readonly signInRequiredHeading: Locator;
  readonly loginRequired: Locator;
  private readonly page: Page;

  constructor(page: Page) {
    this.page = page;
    this.heading = page.getByRole('heading', { name: 'Your account' });
    this.email = page.getByTestId('account-email');
    this.role = page.getByTestId('account-role');
    this.signInRequiredHeading = page.getByRole('heading', { name: 'Sign in required' });
    this.loginRequired = page.getByTestId('login-required');
  }

  async goto(): Promise<void> {
    await this.page.goto('/account');
  }

  async expectSignedInAs(role: string): Promise<void> {
    await expect(this.heading).toBeVisible();
    await expect(this.email).toHaveText(`${role}@demo.test`);
    await expect(this.role).toHaveText(role);
  }

  async expectSignInRequired(): Promise<void> {
    await expect(this.signInRequiredHeading).toBeVisible();
    await expect(this.loginRequired).toHaveText('Please sign in');
  }
}
