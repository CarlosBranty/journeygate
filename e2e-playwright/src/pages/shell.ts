import type { Locator, Page } from '@playwright/test';

/** Header present on every page of the demo shop. */
export class Shell {
  readonly nav: Locator;
  readonly checkoutLink: Locator;
  readonly accountLink: Locator;

  constructor(page: Page) {
    this.nav = page.getByRole('navigation', { name: 'Main' });
    this.checkoutLink = this.nav.getByRole('link', { name: 'Checkout' });
    this.accountLink = this.nav.getByRole('link', { name: 'Account' });
  }
}
