import { authStatePath, expect, test } from '../../../src/fixtures';
import { addToCartAndPay, buyWithCard } from '../../../src/flows/purchase.flow';

test.use({ storageState: authStatePath('customer') });

test.describe(
  'Checkout: customer pays by card',
  { tag: ['@journey:checkout.customer-card', '@area:checkout', '@smoke'] },
  () => {
    test('creates exactly one confirmed order', async ({ data, productPage, checkout, confirmation }) => {
      const product = await data.product({ name: 'Blue mug', stock: 3, priceCents: 1999 });

      const orderNumber = await buyWithCard({ productPage, checkout, confirmation }, product, data.cards.approved);

      await expect(confirmation.total).toHaveText('$19.99');
      const orders = await data.ordersFor(product.id);
      expect(orders).toHaveLength(1);
      expect(orders[0]).toMatchObject({
        number: orderNumber,
        status: 'confirmed',
        totalCents: 1999,
        email: 'customer@demo.test',
        lines: [{ productId: product.id, quantity: 1, unitPriceCents: 1999 }],
      });
    });

    test('a declined card creates no order and keeps the cart', async ({ data, productPage, checkout }) => {
      const product = await data.product({ name: 'Red mug', stock: 3, priceCents: 1500 });

      await addToCartAndPay({ productPage, checkout }, product, data.cards.declined);

      await checkout.expectPaymentError('Your card was declined');
      await expect(checkout.lineQuantity(product.name)).toHaveText('1');
      expect(await data.ordersFor(product.id)).toHaveLength(0);
    });
  },
);
