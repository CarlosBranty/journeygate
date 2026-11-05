// Cart area: POST /cart adds a product to the cookie-identified cart.
import { readForm, redirect, sendHtml, serializeCookie } from '../http.mjs';
import { StoreError } from '../store.mjs';
import { sendNotFound } from '../views/not-found.mjs';
import { escapeHtml, renderPage } from '../views/layout.mjs';

export const CART_COOKIE = 'cart';

export function renderCartError(message) {
  return renderPage('Cart', `<h1>Cart</h1>
<p data-testid="cart-error" role="alert">${escapeHtml(message)}</p>
<p><a href="/checkout">Go to checkout</a></p>`);
}

function parseQuantity(raw) {
  if (raw === null || raw === '') return 1;
  if (!/^\d+$/.test(raw)) return null;
  const n = Number(raw);
  return Number.isSafeInteger(n) && n >= 1 ? n : null;
}

export function registerCart(router, { store }) {
  router.add('POST', '/cart', async (req, res, { cookies }) => {
    const form = await readForm(req);
    const productId = form.get('productId');
    const quantity = parseQuantity(form.get('quantity'));
    if (!productId || quantity === null) {
      return sendHtml(res, 400, renderCartError('Invalid quantity'));
    }
    try {
      const { cartId, created } = store.addToCart(cookies[CART_COOKIE], productId, quantity);
      redirect(res, '/checkout', created ? { 'set-cookie': serializeCookie(CART_COOKIE, cartId) } : {});
    } catch (err) {
      if (!(err instanceof StoreError)) throw err;
      if (err.status === 404) return sendNotFound(res);
      sendHtml(res, 409, renderCartError('Not enough stock'));
    }
  });
}
