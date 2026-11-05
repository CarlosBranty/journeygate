import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createStore, StoreError } from '../src/store.mjs';

const ns = 'r1-test-a';
const mug = { slug: 'r1-test-a-mug', name: 'Blue mug', priceCents: 1999, stock: 3 };

test('seed assigns ids and default slugs in request order', () => {
  const store = createStore();
  const products = store.seed(ns, [mug, { name: 'Cup', priceCents: 500, stock: 1 }]);
  assert.equal(products.length, 2);
  assert.match(products[0].id, /^p_\d+$/);
  assert.notEqual(products[0].id, products[1].id);
  assert.equal(products[0].slug, 'r1-test-a-mug');
  assert.equal(products[1].slug, `${ns}-p${products[1].id.slice(2)}`);
  assert.equal(store.getProductBySlug('r1-test-a-mug').name, 'Blue mug');
  assert.equal(store.getProduct(products[0].id).namespace, ns);
});

test('seed rejects a duplicate slug atomically', () => {
  const store = createStore();
  store.seed(ns, [mug]);
  assert.throws(
    () => store.seed('other', [{ slug: 'fresh', name: 'A', priceCents: 1, stock: 1 }, mug]),
    (err) => err instanceof StoreError && err.status === 409 && err.message === `slug already exists: ${mug.slug}`,
  );
  assert.equal(store.getProductBySlug('fresh'), undefined);
});

test('seed rejects a slug duplicated inside one request', () => {
  const store = createStore();
  assert.throws(() => store.seed(ns, [mug, mug]), (err) => err.status === 409);
  assert.equal(store.getProductBySlug(mug.slug), undefined);
});

test('deleteNamespace removes products and orders of that namespace only', () => {
  const store = createStore();
  const [a] = store.seed('ns-a', [{ name: 'A', priceCents: 100, stock: 5 }]);
  const [b] = store.seed('ns-b', [{ name: 'B', priceCents: 100, stock: 5 }]);
  const cartA = store.addToCart(null, a.id, 1).cartId;
  store.placeOrder(cartA, null);
  store.deleteNamespace('ns-a');
  assert.equal(store.getProduct(a.id), undefined);
  assert.equal(store.getProductBySlug(a.slug), undefined);
  assert.deepEqual(store.ordersByProduct(a.id), []);
  assert.equal(store.getProduct(b.id).name, 'B');
});

test('sessions are created per role with 32-hex sids', () => {
  const store = createStore();
  const session = store.createSession('customer');
  assert.match(session.sid, /^[0-9a-f]{32}$/);
  assert.deepEqual(store.getSession(session.sid), { sid: session.sid, role: 'customer', email: 'customer@demo.test' });
  assert.equal(store.getSession('nope'), undefined);
});

test('addToCart creates a cart and accumulates quantity', () => {
  const store = createStore();
  const [p] = store.seed(ns, [mug]);
  const { cartId } = store.addToCart(null, p.id, 1);
  assert.match(cartId, /^[0-9a-f]{32}$/);
  assert.equal(store.addToCart(cartId, p.id, 2).cartId, cartId);
  assert.deepEqual(store.getCartLines(cartId), [{ product: store.getProduct(p.id), quantity: 3 }]);
});

test('addToCart refuses more than stock and unknown products', () => {
  const store = createStore();
  const [p] = store.seed(ns, [mug]);
  const { cartId } = store.addToCart(null, p.id, 3);
  assert.throws(() => store.addToCart(cartId, p.id, 1), (err) => err.status === 409);
  assert.equal(store.getCartLines(cartId)[0].quantity, 3);
  assert.throws(() => store.addToCart(null, 'p_999', 1), (err) => err.status === 404);
});

test('addToCart with an unknown cart id creates a new cart', () => {
  const store = createStore();
  const [p] = store.seed(ns, [mug]);
  const { cartId } = store.addToCart('deadbeef', p.id, 1);
  assert.notEqual(cartId, 'deadbeef');
  assert.equal(store.hasCart(cartId), true);
  assert.equal(store.hasCart('deadbeef'), false);
});

test('placeOrder creates a numbered order, decrements stock and clears the cart', () => {
  const store = createStore();
  const [p] = store.seed(ns, [mug]);
  const { cartId } = store.addToCart(null, p.id, 2);
  const order = store.placeOrder(cartId, 'customer@demo.test');
  assert.match(order.id, /^o_\d+$/);
  assert.equal(order.number, 'ORD-000001');
  assert.equal(order.namespace, ns);
  assert.equal(order.status, 'confirmed');
  assert.deepEqual(order.lines, [{ productId: p.id, quantity: 2, unitPriceCents: 1999 }]);
  assert.equal(order.totalCents, 3998);
  assert.equal(order.email, 'customer@demo.test');
  assert.ok(!Number.isNaN(Date.parse(order.createdAt)));
  assert.equal(store.getProduct(p.id).stock, 1);
  assert.deepEqual(store.getCartLines(cartId), []);
  assert.deepEqual(store.ordersByProduct(p.id), [order]);
  const second = store.placeOrder(store.addToCart(null, p.id, 1).cartId, null);
  assert.equal(second.number, 'ORD-000002');
  assert.deepEqual(store.ordersByProduct(p.id), [order, second]);
});

test('placeOrder refuses when stock dropped below a line quantity', () => {
  const store = createStore();
  const [p] = store.seed(ns, [mug]);
  const first = store.addToCart(null, p.id, 2).cartId;
  const second = store.addToCart(null, p.id, 2).cartId;
  store.placeOrder(first, null);
  assert.throws(() => store.placeOrder(second, null), (err) => err.status === 409 && err.message === 'Not enough stock');
  assert.equal(store.getProduct(p.id).stock, 1);
  assert.equal(store.getCartLines(second)[0].quantity, 2);
  assert.equal(store.ordersByProduct(p.id).length, 1);
});

test('cart lines whose product was deleted are dropped', () => {
  const store = createStore();
  const [p] = store.seed(ns, [mug]);
  const { cartId } = store.addToCart(null, p.id, 1);
  store.deleteNamespace(ns);
  assert.deepEqual(store.getCartLines(cartId), []);
});
