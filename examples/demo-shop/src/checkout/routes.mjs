// Checkout area: cart summary, payment and order confirmation.
import { readForm, sendHtml, serializeCookie } from '../http.mjs';
import { StoreError } from '../store.mjs';
import { CART_COOKIE } from '../cart/routes.mjs';
import { escapeHtml, formatPrice, renderPage } from '../views/layout.mjs';
import { evaluatePayment } from './payment.mjs';

const EMPTY_CART = 'Your cart is empty';

export function renderCheckoutPage(lines, error) {
  const alert = error ? `\n<p data-testid="payment-error" role="alert">${escapeHtml(error)}</p>` : '';
  if (lines.length === 0) {
    return renderPage('Checkout', `<h1>Checkout</h1>${alert}
<p data-testid="cart-empty">${EMPTY_CART}</p>`);
  }
  const items = lines
    .map(({ product, quantity }) => `<li data-testid="cart-line"><span data-testid="cart-line-name">${escapeHtml(product.name)}</span> x <span data-testid="cart-line-quantity">${quantity}</span></li>`)
    .join('\n');
  const total = lines.reduce((sum, { product, quantity }) => sum + product.priceCents * quantity, 0);
  return renderPage('Checkout', `<h1>Checkout</h1>${alert}
<ul>
${items}
</ul>
<p>Total: <span data-testid="cart-total">${formatPrice(total)}</span></p>
<form aria-label="Payment" method="post" action="/checkout">
<label for="cardNumber">Card number</label>
<input type="text" id="cardNumber" name="cardNumber" autocomplete="cc-number" inputmode="numeric">
<label for="expiry">Expiry</label>
<input type="text" id="expiry" name="expiry" placeholder="MM/YY" autocomplete="cc-exp">
<label for="cvc">CVC</label>
<input type="text" id="cvc" name="cvc" autocomplete="cc-csc" inputmode="numeric">
<button type="submit">Pay now</button>
</form>`);
}

export function renderConfirmation(order) {
  return renderPage('Order confirmed', `<section data-testid="order-confirmation">
<h1>Order confirmed</h1>
<p>Order number: <span data-testid="order-number">${escapeHtml(order.number)}</span></p>
<p>Total: <span data-testid="order-total">${formatPrice(order.totalCents)}</span></p>
</section>`);
}

export function registerCheckout(router, { store }) {
  router.add('GET', '/checkout', (req, res, { cookies }) => {
    sendHtml(res, 200, renderCheckoutPage(store.getCartLines(cookies[CART_COOKIE])));
  });

  router.add('POST', '/checkout', async (req, res, { cookies }) => {
    const form = await readForm(req);
    const cartId = cookies[CART_COOKIE];
    const lines = store.getCartLines(cartId);
    const fail = (status, message) => sendHtml(res, status, renderCheckoutPage(store.getCartLines(cartId), message));
    if (lines.length === 0) return fail(400, EMPTY_CART);

    const payment = evaluatePayment({
      cardNumber: form.get('cardNumber') ?? undefined,
      expiry: form.get('expiry') ?? undefined,
      cvc: form.get('cvc') ?? undefined,
    });
    if (!payment.ok) return fail(payment.status, payment.message);

    const session = store.getSession(cookies.sid);
    try {
      const order = store.placeOrder(cartId, session ? session.email : null);
      sendHtml(res, 200, renderConfirmation(order), {
        'set-cookie': serializeCookie(CART_COOKIE, '', { maxAge: 0 }),
      });
    } catch (err) {
      if (!(err instanceof StoreError)) throw err;
      fail(err.status, err.message);
    }
  });
}
