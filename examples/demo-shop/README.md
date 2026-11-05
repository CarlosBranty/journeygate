# demo-shop

A tiny shop that implements the testability contract, so the Playwright suite and `guard` can be proven end to end. The full HTTP and page contract lives in [`docs/contracts/demo-app.md`](../../docs/contracts/demo-app.md); this README only covers running it.

- Node >= 24, zero dependencies (`node:http`, `node:crypto`).
- In-memory storage: everything is lost on restart.

## Run

| How | Command (from the repo root) |
| --- | --- |
| Foreground | `node examples/demo-shop/server.mjs` |
| npm | `npm --prefix examples/demo-shop start` |
| Docker | `docker compose -f examples/demo-shop/docker-compose.yml up -d --wait` |
| Through guard | `guard up` from the repo root (uses the root `target.toml`) |

The server listens on `0.0.0.0:$PORT` (default `3000`) and prints `demo-shop listening on http://0.0.0.0:<port>`. Readiness: `GET /healthz` -> `200 {"status":"ok"}`.

### Environment

| Variable | Default | Meaning |
| --- | --- | --- |
| `PORT` | `3000` | Listening port. `0` picks a free one. |
| `GUARD_TEST_TOKEN` | `demo-token` | Value expected in the `x-test-token` header of `/__test__/*` routes. |
| `NODE_ENV` | unset | `production` removes every `/__test__/*` route (they answer `404`). |

## Starting and stopping with guard

`target.toml` declares `start = "node examples/demo-shop/server.mjs"`, a foreground process. `guard up` runs it in the background from the repo root, writes its PID to `.guard/app.pid`, appends its output to `.guard/app.log` and waits for `GET /healthz` to return `200`. There is no `stop` command: `guard down` terminates the PID in `.guard/app.pid` (the server exits cleanly on `SIGTERM`/`SIGINT`).

For the container instead, set in `target.toml`:

```toml
start = "docker compose -f examples/demo-shop/docker-compose.yml up -d --wait"
stop = "docker compose -f examples/demo-shop/docker-compose.yml down -v"
```

## Test

```sh
cd examples/demo-shop
npm test        # node --test "test/*.test.mjs"
```

## Layout

| Path | Role | Area in `target.toml` |
| --- | --- | --- |
| `server.mjs` | Entry point: reads env, listens. | — |
| `src/app.mjs` | `createApp(options)`: composition root, returns the request handler. | — |
| `src/router.mjs`, `src/http.mjs` | Routing, cookies, bodies, responses. | — |
| `src/store.mjs` | In-memory products, sessions, carts and orders. | — |
| `src/views/` | Shared layout, price formatting, not-found page. | — |
| `src/products/` | Product pages. | `catalog` |
| `src/cart/` | `POST /cart`. | `checkout` |
| `src/checkout/` | Checkout page, fake payment, confirmation. | `checkout` |
| `src/account/` | Account page. | `account` |
| `src/test-api/` | `/__test__/*` login, seed, cleanup, orders. | — |
