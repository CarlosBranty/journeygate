import { expect, type Locator, type Page } from '@playwright/test';

export class OrderConfirmationPage {
  readonly container: Locator;
  readonly heading: Locator;
  readonly orderNumber: Locator;
  readonly total: Locator;

  constructor(page: Page) {
    this.container = page.getByTestId('order-confirmation');
    this.heading = page.getByRole('heading', { name: 'Order confirmed' });
    this.orderNumber = this.container.getByTestId('order-number');
    this.total = this.container.getByTestId('order-total');
  }

  async expectConfirmed(): Promise<void> {
    await expect(this.container).toBeVisible();
    await expect(this.heading).toBeVisible();
    await expect(this.orderNumber).toHaveText(/^ORD-\d{6}$/);
  }
}
