import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { startApp, testIdText, countTestId } from './support/harness.mjs';

let app;
before(async () => {
  app = await startApp();
});
after(() => app.close());

let seq = 0;
async function seed(c, products) {
  const namespace = `shop-${++seq}`;
  const res = await c.testApi('POST', '/__test__/seed', { json: { namespace, products } });
  assert.equal(res.status, 201);
  return { namespace, products: res.json().products };
}

const approved = { cardNumber: '4242 4242 4242 4242', expiry: '12/34', cvc: '123' };
const declined = { cardNumber: '4000 0000 0000 0002', expiry: '12/34', cvc: '123' };

function assertHtml(res, title) {
  assert.match(res.headers.get('content-type'), /^text\/html; charset=utf-8$/);
  assert.match(res.text, new RegExp(`<title>${title} \\| Demo Shop</title>`));
  assert.match(res.text, /<nav aria-label="Main">/);
}

test('healthz answers ok', async () => {
  const res = await app.client().request('GET', '/healthz');
  assert.equal(res.status, 200);
  assert.deepEqual(res.json(), { status: 'ok' });
  assert.match(res.headers.get('content-type'), /^application\/json; charset=utf-8$/);
});

test('unknown routes render the not-found page', async () => {
  const c = app.client();
  for (const [method, path] of [['GET', '/nope'], ['GET', '/products/missing'], ['DELETE', '/checkout'], ['GET', '/__test__/unknown']]) {
    const res = await c.request(method, path);
    assert.equal(res.status, 404, `${method} ${path}`);
    assertHtml(res, 'Not found');
    assert.equal(testIdText(res.text, 'not-found'), 'Page not found');
  }
});

test('product page shows name, price, stock and the add form', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Blue <mug>', priceCents: 1999, stock: 3 }]);
  const res = await c.request('GET', `/products/${p.slug}`);
  assert.equal(res.status, 200);
  assertHtml(res, 'Blue &lt;mug&gt;');
  assert.match(res.text, /<h1[^>]*data-testid="product-name"[^>]*>Blue &lt;mug&gt;<\/h1>/);
  assert.equal(testIdText(res.text, 'product-price'), '$19.99');
  assert.equal(testIdText(res.text, 'product-stock'), '3 in stock');
  assert.match(res.text, /<form method="post" action="\/cart">/);
  assert.match(res.text, new RegExp(`<input type="hidden" name="productId" value="${p.id}">`));
  assert.match(res.text, /<label for="quantity">Quantity<\/label>/);
  assert.match(res.text, /<input type="number" id="quantity" name="quantity" min="1" value="1">/);
  assert.match(res.text, /<button type="submit">Add to cart<\/button>/);
});

test('product page disables the button when out of stock', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Gone', priceCents: 100, stock: 0 }]);
  const res = await c.request('GET', `/products/${p.slug}`);
  assert.equal(testIdText(res.text, 'product-stock'), 'Out of stock');
  assert.match(res.text, /<button type="submit" disabled>Add to cart<\/button>/);
});

test('empty checkout shows the empty message and no payment form', async () => {
  const res = await app.client().request('GET', '/checkout');
  assert.equal(res.status, 200);
  assertHtml(res, 'Checkout');
  assert.match(res.text, /<h1>Checkout<\/h1>/);
  assert.equal(testIdText(res.text, 'cart-empty'), 'Your cart is empty');
  assert.doesNotMatch(res.text, /aria-label="Payment"/);
  assert.equal(countTestId(res.text, 'payment-error'), 0);
});

test('adding to the cart redirects to checkout and accumulates quantity', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Blue mug', priceCents: 1999, stock: 3 }]);
  const add = await c.request('POST', '/cart', { form: { productId: p.id, quantity: '1' } });
  assert.equal(add.status, 303);
  assert.equal(add.headers.get('location'), '/checkout');
  assert.match(add.setCookies[0], /^cart=[0-9a-f]{32}; Path=\/; HttpOnly; SameSite=Lax$/);
  const again = await c.request('POST', '/cart', { form: { productId: p.id } });
  assert.equal(again.status, 303);
  assert.equal(again.setCookies.length, 0, 'an existing cart keeps its cookie');
  const page = await c.request('GET', '/checkout');
  assert.equal(countTestId(page.text, 'cart-line'), 1);
  assert.equal(testIdText(page.text, 'cart-line-name'), 'Blue mug');
  assert.equal(testIdText(page.text, 'cart-line-quantity'), '2');
  assert.equal(testIdText(page.text, 'cart-total'), '$39.98');
  assert.match(page.text, /<form aria-label="Payment" method="post" action="\/checkout">/);
  assert.match(page.text, /<label for="cardNumber">Card number<\/label>/);
  assert.match(page.text, /name="cardNumber" autocomplete="cc-number" inputmode="numeric"/);
  assert.match(page.text, /<label for="expiry">Expiry<\/label>/);
  assert.match(page.text, /name="expiry" placeholder="MM\/YY"/);
  assert.match(page.text, /<label for="cvc">CVC<\/label>/);
  assert.match(page.text, /<button type="submit">Pay now<\/button>/);
  assert.equal(countTestId(page.text, 'cart-empty'), 0);
});

test('carts are per cookie, not shared between clients', async () => {
  const a = app.client();
  const b = app.client();
  const { products: [p] } = await seed(a, [{ name: 'Mug', priceCents: 100, stock: 5 }]);
  await a.request('POST', '/cart', { form: { productId: p.id } });
  const page = await b.request('GET', '/checkout');
  assert.equal(countTestId(page.text, 'cart-empty'), 1);
});

test('adding more than stock answers 409 with cart-error', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Mug', priceCents: 100, stock: 2 }]);
  const res = await c.request('POST', '/cart', { form: { productId: p.id, quantity: '3' } });
  assert.equal(res.status, 409);
  assertHtml(res, 'Cart');
  assert.match(res.text, /data-testid="cart-error" role="alert"/);
  assert.equal(testIdText(res.text, 'cart-error'), 'Not enough stock');
});

test('adding an unknown product answers 404', async () => {
  const res = await app.client().request('POST', '/cart', { form: { productId: 'p_999999' } });
  assert.equal(res.status, 404);
  assert.equal(testIdText(res.text, 'not-found'), 'Page not found');
});

test('adding with an invalid quantity or no productId answers 400', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Mug', priceCents: 100, stock: 2 }]);
  for (const form of [{ productId: p.id, quantity: '0' }, { productId: p.id, quantity: '1.5' }, { productId: p.id, quantity: 'x' }, {}]) {
    const res = await c.request('POST', '/cart', { form });
    assert.equal(res.status, 400, JSON.stringify(form));
    assert.equal(countTestId(res.text, 'cart-error'), 1);
  }
});

test('paying with an empty cart answers 400', async () => {
  const res = await app.client().request('POST', '/checkout', { form: approved });
  assert.equal(res.status, 400);
  assertHtml(res, 'Checkout');
  assert.match(res.text, /data-testid="payment-error" role="alert"/);
  assert.equal(testIdText(res.text, 'payment-error'), 'Your cart is empty');
});

test('invalid card details answer 400 and keep no values', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Mug', priceCents: 100, stock: 2 }]);
  await c.request('POST', '/cart', { form: { productId: p.id } });
  const res = await c.request('POST', '/checkout', { form: { cardNumber: '4242 4242 4242 4242', expiry: '13/34', cvc: '123' } });
  assert.equal(res.status, 400);
  assert.equal(testIdText(res.text, 'payment-error'), 'Invalid card details');
  assert.doesNotMatch(res.text, /4242/);
  assert.equal(countTestId(res.text, 'cart-line'), 1);
});

test('a declined card answers 402, keeps the cart and creates no order', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Mug', priceCents: 100, stock: 2 }]);
  await c.request('POST', '/cart', { form: { productId: p.id } });
  const res = await c.request('POST', '/checkout', { form: declined });
  assert.equal(res.status, 402);
  assert.equal(testIdText(res.text, 'payment-error'), 'Your card was declined');
  assert.equal(countTestId((await c.request('GET', '/checkout')).text, 'cart-line'), 1);
  assert.deepEqual((await c.testApi('GET', `/__test__/orders?productId=${p.id}`)).json(), { orders: [] });
});

test('stock exhausted after adding answers 409 at checkout', async () => {
  const a = app.client();
  const b = app.client();
  const { products: [p] } = await seed(a, [{ name: 'Mug', priceCents: 100, stock: 1 }]);
  await a.request('POST', '/cart', { form: { productId: p.id } });
  await b.request('POST', '/cart', { form: { productId: p.id } });
  assert.equal((await a.request('POST', '/checkout', { form: approved })).status, 200);
  const res = await b.request('POST', '/checkout', { form: approved });
  assert.equal(res.status, 409);
  assert.equal(testIdText(res.text, 'payment-error'), 'Not enough stock');
  assert.equal((await a.testApi('GET', `/__test__/orders?productId=${p.id}`)).json().orders.length, 1);
});

test('account requires a session', async () => {
  const res = await app.client().request('GET', '/account');
  assert.equal(res.status, 401);
  assertHtml(res, 'Account');
  assert.match(res.text, /<h1>Sign in required<\/h1>/);
  assert.equal(testIdText(res.text, 'login-required'), 'Please sign in');
});

test('account shows the logged-in email and role', async () => {
  const c = app.client();
  await c.testApi('POST', '/__test__/login', { json: { role: 'customer' } });
  const res = await c.request('GET', '/account');
  assert.equal(res.status, 200);
  assertHtml(res, 'Account');
  assert.match(res.text, /<h1>Your account<\/h1>/);
  assert.equal(testIdText(res.text, 'account-email'), 'customer@demo.test');
  assert.equal(testIdText(res.text, 'account-role'), 'customer');
});

test('reference flow: login, seed, add, pay, verify, clean up', async () => {
  const c = app.client();
  assert.equal((await c.testApi('POST', '/__test__/login', { json: { role: 'customer' } })).status, 200);
  const namespace = 'r1793452800-4242-a1b2c3-x0-r9';
  const seeded = await c.testApi('POST', '/__test__/seed', {
    json: { namespace, products: [{ slug: `${namespace}-mug`, name: 'Blue mug', priceCents: 1999, stock: 3 }] },
  });
  const [p] = seeded.json().products;
  assert.equal((await c.request('GET', `/products/${p.slug}`)).status, 200);
  const add = await c.request('POST', '/cart', { form: { productId: p.id, quantity: '1' } });
  assert.equal(add.status, 303);
  assert.equal((await c.request('GET', '/checkout')).status, 200);
  const pay = await c.request('POST', '/checkout', { form: approved });
  assert.equal(pay.status, 200);
  assertHtml(pay, 'Order confirmed');
  assert.equal(countTestId(pay.text, 'order-confirmation'), 1);
  assert.match(pay.text, /<h1>Order confirmed<\/h1>/);
  const number = testIdText(pay.text, 'order-number');
  assert.match(number, /^ORD-\d{6}$/);
  assert.equal(testIdText(pay.text, 'order-total'), '$19.99');
  assert.ok(pay.setCookies.some((s) => /^cart=; .*Max-Age=0/.test(s)), 'cart cookie cleared');

  const orders = (await c.testApi('GET', `/__test__/orders?productId=${p.id}`)).json().orders;
  assert.equal(orders.length, 1);
  assert.equal(orders[0].number, number);
  assert.equal(orders[0].namespace, namespace);
  assert.equal(orders[0].status, 'confirmed');
  assert.equal(orders[0].email, 'customer@demo.test');
  assert.deepEqual(orders[0].lines, [{ productId: p.id, quantity: 1, unitPriceCents: 1999 }]);
  assert.equal(orders[0].totalCents, 1999);
  assert.deepEqual(Object.keys(orders[0]), ['id', 'number', 'namespace', 'status', 'lines', 'totalCents', 'email', 'createdAt']);

  const stock = await c.request('GET', `/products/${p.slug}`);
  assert.equal(testIdText(stock.text, 'product-stock'), '2 in stock');
  assert.equal(countTestId((await c.request('GET', '/checkout')).text, 'cart-empty'), 1);

  assert.equal((await c.testApi('DELETE', `/__test__/seed/${namespace}`)).status, 204);
  assert.deepEqual((await c.testApi('GET', `/__test__/orders?productId=${p.id}`)).json(), { orders: [] });
});

test('an anonymous order has a null email', async () => {
  const c = app.client();
  const { products: [p] } = await seed(c, [{ name: 'Mug', priceCents: 100, stock: 1 }]);
  await c.request('POST', '/cart', { form: { productId: p.id } });
  await c.request('POST', '/checkout', { form: approved });
  const [order] = (await c.testApi('GET', `/__test__/orders?productId=${p.id}`)).json().orders;
  assert.equal(order.email, null);
});
