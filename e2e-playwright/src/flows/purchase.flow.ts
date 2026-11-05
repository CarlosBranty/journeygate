import { expect } from '@playwright/test';
import type { Card, Product } from '../data/test-data';
import type { CheckoutPage, OrderConfirmationPage, ProductPage } from '../pages';

export interface PurchasePages {
  productPage: ProductPage;
  checkout: CheckoutPage;
}

/** Product page -> add to cart -> checkout shows the line -> pay. */
export async function addToCartAndPay(
  { productPage, checkout }: PurchasePages,
  product: Product,
  card: Card,
  quantity = 1,
): Promise<void> {
  await productPage.goto(product.slug);
  await productPage.addToCart(quantity);
  await expect(checkout.heading).toBeVisible();
  await expect(checkout.lineQuantity(product.name)).toHaveText(String(quantity));
  await checkout.payWithCard(card);
}

/** The full happy path, ending on a confirmed order. Returns the order number. */
export async function buyWithCard(
  pages: PurchasePages & { confirmation: OrderConfirmationPage },
  product: Product,
  card: Card,
  quantity = 1,
): Promise<string> {
  await addToCartAndPay(pages, product, card, quantity);
  await pages.confirmation.expectConfirmed();
  return (await pages.confirmation.orderNumber.textContent()) ?? '';
}
