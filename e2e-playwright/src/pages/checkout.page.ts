import { expect, type Locator, type Page } from '@playwright/test';
import type { Card } from '../data/test-data';

/** Messages of the payment rules table in docs/contracts/demo-app.md. */
export type PaymentErrorMessage =
  | 'Your cart is empty'
  | 'Invalid card details'
  | 'Your card was declined'
  | 'Not enough stock';

export class CheckoutPage {
  readonly heading: Locator;
  readonly emptyCart: Locator;
  readonly lines: Locator;
  readonly total: Locator;
  readonly paymentForm: Locator;
  readonly cardNumber: Locator;
  readonly expiry: Locator;
  readonly cvc: Locator;
  readonly payButton: Locator;
  readonly paymentError: Locator;
  private readonly page: Page;

  constructor(page: Page) {
    this.page = page;
    this.heading = page.getByRole('heading', { name: 'Checkout' });
    this.emptyCart = page.getByTestId('cart-empty');
    this.lines = page.getByTestId('cart-line');
    this.total = page.getByTestId('cart-total');
    this.paymentForm = page.getByRole('form', { name: 'Payment' });
    this.cardNumber = page.getByLabel('Card number');
    this.expiry = page.getByLabel('Expiry');
    this.cvc = page.getByLabel('CVC');
    this.payButton = page.getByRole('button', { name: 'Pay now' });
    this.paymentError = page.getByTestId('payment-error');
  }

  async goto(): Promise<void> {
    await this.page.goto('/checkout');
    await expect(this.heading).toBeVisible();
  }

  /** The cart line whose name is exactly `productName`. */
  line(productName: string): Locator {
    return this.lines.filter({
      has: this.page.getByTestId('cart-line-name').getByText(productName, { exact: true }),
    });
  }

  lineQuantity(productName: string): Locator {
    return this.line(productName).getByTestId('cart-line-quantity');
  }

  async payWithCard(card: Card): Promise<void> {
    await this.cardNumber.fill(card.number);
    await this.expiry.fill(card.expiry);
    await this.cvc.fill(card.cvc);
    await this.payButton.click();
  }

  async expectPaymentError(message: PaymentErrorMessage): Promise<void> {
    await expect(this.paymentError).toHaveText(message);
  }
}
