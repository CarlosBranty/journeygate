// In-memory store. Products and orders are keyed by namespace so parallel
// tests never collide; carts are keyed by the `cart` cookie, not the session.
import { randomBytes } from 'node:crypto';

export class StoreError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

const newId = () => randomBytes(16).toString('hex');

export function createStore() {
  const products = new Map(); // id -> product
  const slugs = new Map(); // slug -> id
  const sessions = new Map(); // sid -> session
  const carts = new Map(); // cartId -> Map(productId -> quantity)
  const orders = []; // oldest first
  let productSeq = 0;
  let orderSeq = 0;

  const seed = (namespace, items) => {
    const pending = new Set();
    for (const item of items) {
      if (item.slug === undefined) continue;
      if (slugs.has(item.slug) || pending.has(item.slug)) {
        throw new StoreError(409, `slug already exists: ${item.slug}`);
      }
      pending.add(item.slug);
    }
    const created = items.map((item) => {
      const n = ++productSeq;
      return {
        id: `p_${n}`,
        namespace,
        slug: item.slug ?? `${namespace}-p${n}`,
        name: item.name,
        priceCents: item.priceCents,
        stock: item.stock,
      };
    });
    for (const product of created) {
      if (slugs.has(product.slug)) throw new StoreError(409, `slug already exists: ${product.slug}`);
    }
    for (const product of created) {
      products.set(product.id, product);
      slugs.set(product.slug, product.id);
    }
    return created.map((p) => ({ ...p }));
  };

  const deleteNamespace = (namespace) => {
    for (const [id, product] of products) {
      if (product.namespace !== namespace) continue;
      products.delete(id);
      slugs.delete(product.slug);
    }
    for (let i = orders.length - 1; i >= 0; i--) {
      if (orders[i].namespace === namespace) orders.splice(i, 1);
    }
  };

  const getProduct = (id) => products.get(id);
  const getProductBySlug = (slug) => {
    const id = slugs.get(slug);
    return id === undefined ? undefined : products.get(id);
  };

  const createSession = (role) => {
    const session = { sid: newId(), role, email: `${role}@demo.test` };
    sessions.set(session.sid, session);
    return { ...session };
  };
  const getSession = (sid) => {
    const session = sid === undefined ? undefined : sessions.get(sid);
    return session && { ...session };
  };

  const hasCart = (cartId) => cartId !== undefined && cartId !== null && carts.has(cartId);

  const addToCart = (cartId, productId, quantity) => {
    const product = products.get(productId);
    if (!product) throw new StoreError(404, 'product not found');
    const existing = hasCart(cartId) ? carts.get(cartId) : undefined;
    const total = (existing?.get(productId) ?? 0) + quantity;
    if (total > product.stock) throw new StoreError(409, 'Not enough stock');
    const id = existing ? cartId : newId();
    const cart = existing ?? new Map();
    cart.set(productId, total);
    carts.set(id, cart);
    return { cartId: id, created: !existing };
  };

  const getCartLines = (cartId) => {
    if (!hasCart(cartId)) return [];
    const lines = [];
    for (const [productId, quantity] of carts.get(cartId)) {
      const product = products.get(productId);
      if (product) lines.push({ product, quantity });
    }
    return lines;
  };

  const placeOrder = (cartId, email) => {
    const lines = getCartLines(cartId);
    if (lines.length === 0) throw new StoreError(400, 'Your cart is empty');
    if (lines.some(({ product, quantity }) => quantity > product.stock)) {
      throw new StoreError(409, 'Not enough stock');
    }
    for (const { product, quantity } of lines) product.stock -= quantity;
    const n = ++orderSeq;
    const orderLines = lines.map(({ product, quantity }) => ({
      productId: product.id,
      quantity,
      unitPriceCents: product.priceCents,
    }));
    const order = {
      id: `o_${n}`,
      number: `ORD-${String(n).padStart(6, '0')}`,
      namespace: lines[0].product.namespace,
      status: 'confirmed',
      lines: orderLines,
      totalCents: orderLines.reduce((sum, l) => sum + l.quantity * l.unitPriceCents, 0),
      email,
      createdAt: new Date().toISOString(),
    };
    orders.push(order);
    carts.delete(cartId);
    return structuredClone(order);
  };

  const ordersByProduct = (productId) =>
    orders.filter((o) => o.lines.some((l) => l.productId === productId)).map((o) => structuredClone(o));

  return {
    seed,
    deleteNamespace,
    getProduct,
    getProductBySlug,
    createSession,
    getSession,
    hasCart,
    addToCart,
    getCartLines,
    placeOrder,
    ordersByProduct,
  };
}
