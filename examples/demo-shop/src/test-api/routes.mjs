// Test-only routes (/__test__/*). Mounted only outside production and guarded
// by the `x-test-token` header.
import { timingSafeEqual, createHash } from 'node:crypto';
import { HttpError, readJson, sendEmpty, sendJson, serializeCookie } from '../http.mjs';
import { StoreError } from '../store.mjs';
import { NAME_PATTERN, ROLE_PATTERN, isNonNegativeInteger, isPlainObject } from '../validation.mjs';

const digest = (value) => createHash('sha256').update(String(value)).digest();

export function tokenMatches(expected, given) {
  if (typeof given !== 'string') return false;
  return timingSafeEqual(digest(expected), digest(given));
}

export function registerTestApi(router, { store, token }) {
  const guarded = (handler) => async (req, res, ctx) => {
    if (!tokenMatches(token, req.headers['x-test-token'])) {
      req.resume();
      sendJson(res, 401, { error: 'invalid test token' });
      return;
    }
    await handler(req, res, ctx);
  };

  router.add('POST', '/__test__/login', guarded(async (req, res) => {
    const body = await readJson(req);
    if (!isPlainObject(body) || typeof body.role !== 'string' || !ROLE_PATTERN.test(body.role)) {
      throw new HttpError(400, 'role must match ^[a-z][a-z0-9_-]*$');
    }
    const session = store.createSession(body.role);
    sendJson(res, 200, { role: session.role, email: session.email }, {
      'set-cookie': serializeCookie('sid', session.sid),
    });
  }));

  router.add('POST', '/__test__/seed', guarded(async (req, res) => {
    const { namespace, products } = parseSeed(await readJson(req));
    let created;
    try {
      created = store.seed(namespace, products);
    } catch (err) {
      if (err instanceof StoreError) throw new HttpError(err.status, err.message);
      throw err;
    }
    sendJson(res, 201, {
      namespace,
      products: created.map(({ id, slug, name, priceCents, stock }) => ({ id, slug, name, priceCents, stock })),
    });
  }));

  router.add('DELETE', '/__test__/seed/:namespace', guarded(async (req, res, { params }) => {
    if (!NAME_PATTERN.test(params.namespace)) throw new HttpError(400, 'invalid namespace');
    store.deleteNamespace(params.namespace);
    sendEmpty(res, 204);
  }));

  router.add('GET', '/__test__/orders', guarded(async (req, res, { url }) => {
    const productId = url.searchParams.get('productId');
    if (!productId) throw new HttpError(400, 'productId is required');
    sendJson(res, 200, { orders: store.ordersByProduct(productId) });
  }));
}

function parseSeed(body) {
  if (!isPlainObject(body)) throw new HttpError(400, 'body must be a JSON object');
  const { namespace, products } = body;
  if (typeof namespace !== 'string' || !NAME_PATTERN.test(namespace)) {
    throw new HttpError(400, 'namespace must match ^[A-Za-z0-9._-]{1,200}$');
  }
  if (!Array.isArray(products)) throw new HttpError(400, 'products must be an array');
  const parsed = products.map((p, i) => {
    if (!isPlainObject(p)) throw new HttpError(400, `products[${i}] must be an object`);
    if (p.slug !== undefined && (typeof p.slug !== 'string' || !NAME_PATTERN.test(p.slug))) {
      throw new HttpError(400, `products[${i}].slug must match ^[A-Za-z0-9._-]{1,200}$`);
    }
    if (typeof p.name !== 'string' || p.name === '') {
      throw new HttpError(400, `products[${i}].name must be a non-empty string`);
    }
    if (!isNonNegativeInteger(p.priceCents)) {
      throw new HttpError(400, `products[${i}].priceCents must be a non-negative integer`);
    }
    if (!isNonNegativeInteger(p.stock)) {
      throw new HttpError(400, `products[${i}].stock must be a non-negative integer`);
    }
    return { slug: p.slug, name: p.name, priceCents: p.priceCents, stock: p.stock };
  });
  return { namespace, products: parsed };
}
