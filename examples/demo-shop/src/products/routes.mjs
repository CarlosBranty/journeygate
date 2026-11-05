// Catalog area: product pages.
import { sendHtml } from '../http.mjs';
import { sendNotFound } from '../views/not-found.mjs';
import { escapeHtml, formatPrice, renderPage } from '../views/layout.mjs';

export function renderProductPage(product) {
  const stock = product.stock > 0 ? `${product.stock} in stock` : 'Out of stock';
  const disabled = product.stock > 0 ? '' : ' disabled';
  return renderPage(product.name, `<h1 data-testid="product-name">${escapeHtml(product.name)}</h1>
<p data-testid="product-price">${formatPrice(product.priceCents)}</p>
<p data-testid="product-stock">${stock}</p>
<form method="post" action="/cart">
<input type="hidden" name="productId" value="${escapeHtml(product.id)}">
<label for="quantity">Quantity</label>
<input type="number" id="quantity" name="quantity" min="1" value="1">
<button type="submit"${disabled}>Add to cart</button>
</form>`);
}

export function registerProducts(router, { store }) {
  router.add('GET', '/products/:slug', (req, res, { params }) => {
    const product = store.getProductBySlug(params.slug);
    if (!product) return sendNotFound(res);
    sendHtml(res, 200, renderProductPage(product));
  });
}
