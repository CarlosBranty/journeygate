// Shared page shell and formatting helpers for every server-rendered page.

const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

export function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => ESCAPES[ch]);
}

export function formatPrice(cents) {
  const dollars = Math.floor(cents / 100);
  const rest = String(cents % 100).padStart(2, '0');
  return `$${dollars}.${rest}`;
}

export function renderPage(title, body) {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${escapeHtml(title)} | Demo Shop</title>
</head>
<body>
<header>
<nav aria-label="Main">
<a href="/checkout">Checkout</a>
<a href="/account">Account</a>
</nav>
</header>
<main>
${body}
</main>
</body>
</html>
`;
}
