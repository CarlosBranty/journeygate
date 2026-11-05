import { expect, type Locator, type Page } from '@playwright/test';

export class ProductPage {
  readonly heading: Locator;
  readonly name: Locator;
  readonly price: Locator;
  readonly stock: Locator;
  readonly quantity: Locator;
  readonly addButton: Locator;
  readonly cartError: Locator;
  private readonly page: Page;

  constructor(page: Page) {
    this.page = page;
    this.heading = page.getByRole('heading', { level: 1 });
    this.name = page.getByTestId('product-name');
    this.price = page.getByTestId('product-price');
    this.stock = page.getByTestId('product-stock');
    this.quantity = page.getByLabel('Quantity');
    this.addButton = page.getByRole('button', { name: 'Add to cart' });
    this.cartError = page.getByTestId('cart-error');
  }

  async goto(slug: string): Promise<void> {
    await this.page.goto(`/products/${encodeURIComponent(slug)}`);
    await expect(this.heading).toBeVisible();
  }

  /** Adds the product; on success the app redirects to /checkout. */
  async addToCart(quantity = 1): Promise<void> {
    await this.quantity.fill(String(quantity));
    await this.addButton.click();
  }
}
