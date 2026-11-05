# Contract: reference app `examples/demo-shop`

A tiny shop that implements the testability contract (section 1 of `docs/propuesta-arquitectura.md`) so the Playwright suite and `guard` can be proven end to end. The Playwright stage codes against this document only; it never needs to read the server source.

## Runtime

| Item | Value |
| --- | --- |
| Location | `examples/demo-shop/` (pnpm workspace member, package name `demo-shop`, `"private": true`, `"type": "module"`). |
| Dependencies | None. Node standard library only (`node:http`, `node:crypto`, `node:url`, `node:test`). Node >= 24. |
| Entry point | `examples/demo-shop/server.mjs`; `npm start` = `node server.mjs`. The HTTP handler is exported from `examples/demo-shop/src/app.mjs` as `createApp(options) -> (req, res) => void` so tests can start it on an ephemeral port. |
| Port | `PORT` env var, default `3000`. Binds all interfaces (`0.0.0.0`) so `--docker` runs reach it through `host.docker.internal`. |
| Test token | `GUARD_TEST_TOKEN` env var; default `demo-token` when unset. |
| Test routes | Every `/__test__/*` route returns `404` when `NODE_ENV=production`. |
| Storage | In memory, lost on restart. All seeded data and orders are keyed by **namespace**, so parallel tests never collide. |
| Pages | Server-rendered HTML, `Content-Type: text/html; charset=utf-8`, no JavaScript required. |
| JSON | `Content-Type: application/json; charset=utf-8`. Error bodies are `{"error": "<message>"}`. |

`target.toml` for the demo (at the repo root of this project):

```toml
version = 1

[app]
name = "demo-shop"
languages = ["javascript"]
start = "node examples/demo-shop/server.mjs"
base_url = "http://localhost:3000"
ready_path = "/healthz"

[testability]
token_env = "GUARD_TEST_TOKEN"
roles = ["customer"]

[areas.checkout]
description = "Cart, payment and order confirmation"
paths = ["examples/demo-shop/src/checkout/**", "examples/demo-shop/src/cart/**"]
sensitive = true

[areas.catalog]
description = "Product pages and stock display"
paths = ["examples/demo-shop/src/products/**"]

[areas.account]
description = "Test login, session and account page"
paths = ["examples/demo-shop/src/account/**"]
```

## Data model

| Entity | Fields |
| --- | --- |
| Product | `id` (`"p_<n>"`, globally unique, server-assigned), `namespace`, `slug` (globally unique), `name`, `priceCents` (integer >= 0), `stock` (integer >= 0). |
| Session | `sid` (32 hex chars), `role`, `email` = `<role>@demo.test`. |
| Cart | `cartId` (32 hex chars) -> list of `{ productId, quantity }`. Identified by the `cart` cookie, **not** by the session, so parallel tests that share one role's `storageState` never share a cart. |
| Order | `id` (`"o_<n>"`), `number` (`"ORD-"` + 6-digit zero-padded sequence, e.g. `ORD-000001`), `namespace` (of its first line's product), `status` (`"confirmed"`), `lines[]` (`{ productId, quantity, unitPriceCents }`), `totalCents`, `email` (session email or `null`), `createdAt` (ISO 8601). |

Validation patterns: `namespace` and `slug` match `^[A-Za-z0-9._-]{1,200}$`; `role` matches `^[a-z][a-z0-9_-]*$`.

## Cookies

| Cookie | Set by | Attributes |
| --- | --- | --- |
| `sid` | `POST /__test__/login` | `Path=/; HttpOnly; SameSite=Lax` (no `Secure`, no `Expires`: session cookie). |
| `cart` | first `POST /cart` without a valid `cart` cookie | `Path=/; HttpOnly; SameSite=Lax`. Cleared (`Max-Age=0`) after a successful checkout. |

## Routes

### Test-only routes (require header `x-test-token: <token>`; wrong or missing token -> `401 {"error":"invalid test token"}`)

#### `POST /__test__/login`

Request JSON: `{"role": "customer"}`.

| Response | When |
| --- | --- |
| `200 {"role":"customer","email":"customer@demo.test"}` + `Set-Cookie: sid=...` | valid |
| `400 {"error":"..."}` | body not JSON, `role` missing or not matching the pattern |
| `401` | bad token |

#### `POST /__test__/seed`

Request JSON:

```json
{
  "namespace": "r1793452800-4242-a1b2c3-x0-r0",
  "products": [{ "slug": "r1793452800-4242-a1b2c3-x0-r0-mug", "name": "Blue mug", "priceCents": 1999, "stock": 3 }]
}
```

- `products` may be empty. `slug` is optional: when omitted the server uses `<namespace>-p<n>`. `name` is required (non-empty string). `priceCents` and `stock` are required non-negative integers.
- A `slug` already used by any product (any namespace) -> `409 {"error":"slug already exists: <slug>"}`; nothing from the request is stored (all or nothing).

Response `201`:

```json
{
  "namespace": "r1793452800-4242-a1b2c3-x0-r0",
  "products": [{ "id": "p_17", "slug": "r1793452800-4242-a1b2c3-x0-r0-mug", "name": "Blue mug", "priceCents": 1999, "stock": 3 }]
}
```

Products are returned in request order. `400` on validation errors.

#### `DELETE /__test__/seed/:namespace`

Deletes every product and order of that namespace. Always `204` (also when the namespace is unknown). `400` if `:namespace` does not match the pattern.

#### `GET /__test__/orders?productId=<id>`

`200 {"orders":[ ...Order objects... ]}`: every order that has a line with that `productId`, oldest first. Unknown id -> `200 {"orders":[]}`. Missing `productId` -> `400`.

```json
{
  "orders": [
    {
      "id": "o_3",
      "number": "ORD-000003",
      "namespace": "r1793452800-4242-a1b2c3-x0-r0",
      "status": "confirmed",
      "lines": [{ "productId": "p_17", "quantity": 1, "unitPriceCents": 1999 }],
      "totalCents": 1999,
      "email": "customer@demo.test",
      "createdAt": "2026-10-31T12:00:00.000Z"
    }
  ]
}
```

### Public routes

| Route | Response |
| --- | --- |
| `GET /healthz` | `200 {"status":"ok"}`. |
| `GET /products/:slug` | `200` product page; unknown slug -> `404` not-found page. |
| `POST /cart` | Form (`application/x-www-form-urlencoded`): `productId` (required), `quantity` (optional, integer >= 1, default `1`). Adds to the cart (same product accumulates quantity) and answers `303 See Other`, `Location: /checkout`. Unknown product -> `404` not-found page. Requested total quantity above stock -> `409` page with `cart-error`. |
| `GET /checkout` | `200` checkout page (cart summary + payment form, or empty-cart message). |
| `POST /checkout` | Form: `cardNumber`, `expiry`, `cvc`. See payment rules. Success -> `200` confirmation page. |
| `GET /account` | Logged in (valid `sid`) -> `200` account page. Otherwise `401` page with `login-required`. |
| anything else | `404` not-found page. |

### Payment rules for `POST /checkout`

Spaces in `cardNumber` are ignored.

| Input | Status | Result |
| --- | --- | --- |
| Empty cart | `400` | Checkout page with `payment-error` = `Your cart is empty`. |
| `cardNumber` not 16 digits, `expiry` not `MM/YY` (month 01-12), or `cvc` not 3-4 digits | `400` | Checkout page with `payment-error` = `Invalid card details`. Form keeps no values. |
| `cardNumber` = `4000000000000002` | `402` | Checkout page with `payment-error` = `Your card was declined`. Cart unchanged, no order. |
| Any other valid card, e.g. `4242424242424242` | `200` | Exactly one order created, stock decremented per line, cart cleared, confirmation page. |
| A line's quantity exceeds current stock | `409` | Checkout page with `payment-error` = `Not enough stock`. No order. |

Test cards for fixtures: approved `{ number: "4242 4242 4242 4242", expiry: "12/34", cvc: "123" }`, declined `{ number: "4000 0000 0000 0002", expiry: "12/34", cvc: "123" }`.

## Page contents: every `data-testid` and accessible name

Every page has `<html lang="en">`, a `<title>` of `<Page> | Demo Shop`, and a header `<nav aria-label="Main">` with links named **Checkout** (`/checkout`) and **Account** (`/account`). Prices render as `$` + dollars with two decimals (`1999` -> `$19.99`).

### Product page (`GET /products/:slug`) — title `<product name> | Demo Shop`

| Element | Locator | Content |
| --- | --- | --- |
| `<h1>` | `getByRole('heading', { level: 1 })`, `getByTestId('product-name')` | Product name, e.g. `Blue mug`. |
| Price | `getByTestId('product-price')` | `$19.99` |
| Stock | `getByTestId('product-stock')` | `3 in stock`, or `Out of stock` when `stock = 0`. |
| Add form | `<form method="post" action="/cart">` with hidden `productId` | |
| Quantity | `getByLabel('Quantity')` (`<input type="number" name="quantity" min="1" value="1">`) | |
| Add button | `getByRole('button', { name: 'Add to cart' })` | Disabled when `stock = 0`. |

### Checkout page (`GET /checkout`, and `POST /checkout` errors) — title `Checkout | Demo Shop`

| Element | Locator | Content |
| --- | --- | --- |
| `<h1>` | `getByRole('heading', { name: 'Checkout' })` | `Checkout` |
| Empty cart message | `getByTestId('cart-empty')` | `Your cart is empty`. Present only when the cart is empty; the payment form is then absent. |
| Cart line (one per product) | `getByTestId('cart-line')` | Contains `getByTestId('cart-line-name')` (product name) and `getByTestId('cart-line-quantity')` (integer, e.g. `1`). |
| Total | `getByTestId('cart-total')` | e.g. `$19.99` |
| Payment form | `getByRole('form', { name: 'Payment' })` (`<form aria-label="Payment" method="post" action="/checkout">`) | |
| Card number | `getByLabel('Card number')` (`name="cardNumber"`, `autocomplete="cc-number"`, `inputmode="numeric"`) | |
| Expiry | `getByLabel('Expiry')` (`name="expiry"`, placeholder `MM/YY`) | |
| CVC | `getByLabel('CVC')` (`name="cvc"`) | |
| Pay button | `getByRole('button', { name: 'Pay now' })` | |
| Payment error | `getByTestId('payment-error')`, `role="alert"` | One of the messages in the payment rules table. Absent on `GET`. |

### Order confirmation (`POST /checkout` success) — title `Order confirmed | Demo Shop`

| Element | Locator | Content |
| --- | --- | --- |
| Container | `getByTestId('order-confirmation')` | Wraps the elements below. |
| `<h1>` | `getByRole('heading', { name: 'Order confirmed' })` | `Order confirmed` |
| Order number | `getByTestId('order-number')` | e.g. `ORD-000003` (text only, no label inside the element). |
| Total | `getByTestId('order-total')` | e.g. `$19.99` |

### Cart error (`POST /cart` 409) — title `Cart | Demo Shop`

| Element | Locator | Content |
| --- | --- | --- |
| Error | `getByTestId('cart-error')`, `role="alert"` | `Not enough stock` |

### Account page (`GET /account`) — title `Account | Demo Shop`

| Element | Locator | Content |
| --- | --- | --- |
| `<h1>` | `getByRole('heading', { name: 'Your account' })` | `Your account` |
| Email | `getByTestId('account-email')` | e.g. `customer@demo.test` |
| Role | `getByTestId('account-role')` | e.g. `customer` |
| Not logged in (`401`) | `getByTestId('login-required')` | `Please sign in`; heading `Sign in required`. |

### Not found (`404`) — title `Not found | Demo Shop`

| Element | Locator | Content |
| --- | --- | --- |
| Message | `getByTestId('not-found')` | `Page not found` |

## Reference flow (what `checkout.customer-card` exercises)

```text
POST /__test__/login {"role":"customer"}          -> sid cookie, saved to .auth/customer.json
POST /__test__/seed {namespace, products:[{stock:3, priceCents:1999, name:"Blue mug"}]} -> p_17
GET  /products/<slug>                               -> click "Add to cart"
POST /cart productId=p_17&quantity=1               -> 303 /checkout (cart cookie set)
GET  /checkout                                      -> fill Card number / Expiry / CVC, click "Pay now"
POST /checkout                                      -> order-confirmation visible, order-number ORD-...
GET  /__test__/orders?productId=p_17                -> exactly 1 order
DELETE /__test__/seed/<namespace>                   -> 204
```
