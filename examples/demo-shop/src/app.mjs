// Composition root: wires the store and every area's routes into one handler.
import { createRouter } from './router.mjs';
import { createStore } from './store.mjs';
import { HttpError, parseCookies, sendJson } from './http.mjs';
import { sendNotFound } from './views/not-found.mjs';
import { registerTestApi } from './test-api/routes.mjs';
import { registerProducts } from './products/routes.mjs';
import { registerCart } from './cart/routes.mjs';
import { registerCheckout } from './checkout/routes.mjs';
import { registerAccount } from './account/routes.mjs';

export const DEFAULT_TEST_TOKEN = 'demo-token';

/**
 * @param {{ token?: string, nodeEnv?: string, store?: ReturnType<typeof createStore> }} [options]
 * @returns {(req: import('node:http').IncomingMessage, res: import('node:http').ServerResponse) => void}
 */
export function createApp(options = {}) {
  const store = options.store ?? createStore();
  const token = options.token || DEFAULT_TEST_TOKEN;
  const production = options.nodeEnv === 'production';

  const router = createRouter();
  router.add('GET', '/healthz', (req, res) => sendJson(res, 200, { status: 'ok' }));
  if (!production) registerTestApi(router, { store, token });
  registerProducts(router, { store });
  registerCart(router, { store });
  registerCheckout(router, { store });
  registerAccount(router, { store });

  return (req, res) => {
    handle(router, req, res).catch((err) => {
      if (res.headersSent) {
        res.destroy(err);
        return;
      }
      if (err instanceof HttpError) {
        sendJson(res, err.status, { error: err.message });
        return;
      }
      console.error(err);
      sendJson(res, 500, { error: 'internal error' });
    });
  };
}

async function handle(router, req, res) {
  const url = new URL(req.url, 'http://localhost');
  const match = router.match(req.method, url.pathname);
  if (!match) {
    req.resume();
    return sendNotFound(res);
  }
  const cookies = parseCookies(req.headers.cookie);
  await match.handler(req, res, { params: match.params, url, cookies });
}
