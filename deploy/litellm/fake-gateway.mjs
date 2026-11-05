// Fake model gateway for end-to-end tests of the agent lane. NEVER a model:
// it answers the LiteLLM proxy endpoints guard uses (docs/contracts/tokens.md)
// with deterministic data and calls no provider.
//
//   POST /key/generate     (master key)  -> {key, token, key_alias, expires}
//   POST /key/delete       (master key)  -> {deleted_keys}
//   GET  /key/list         (master key)  -> {keys, total_count, current_page, total_pages}
//        (active keys; full objects: token, key_alias, created_at, expires, metadata)
//   GET  /spend/logs/v2    (master key)  -> {data, total, page, page_size, total_pages}
//   POST /v1/chat/completions, /chat/completions, /v1/messages (virtual key)
//        -> a canned completion; logs one spend row for the key
//   GET  /health                          -> 200
//
// Env: PORT (default 4000), FAKE_GATEWAY_MASTER_KEY (required).
// Run: docker run --rm -e FAKE_GATEWAY_MASTER_KEY=... -v <this dir>:/srv:ro \
//        node:24-bookworm-slim node /srv/fake-gateway.mjs
import { createServer } from 'node:http';
import { createHash, randomBytes } from 'node:crypto';

const port = Number(process.env.PORT ?? 4000);
const master = process.env.FAKE_GATEWAY_MASTER_KEY;
if (!master) {
  console.error('FAKE_GATEWAY_MASTER_KEY is required');
  process.exit(2);
}

/** token (hash) -> {key, alias, models, metadata, max_budget, active, spend, created} */
const keys = new Map();
/** spend rows, oldest first */
const spend = [];
let seq = 0;

const sha = (s) => createHash('sha256').update(s).digest('hex');
const bearer = (req) => (req.headers.authorization ?? '').replace(/^Bearer\s+/i, '');
const gatewayTime = (d) => d.toISOString().replace('T', ' ').replace(/\.\d+Z$/, '');

function send(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(text) });
  res.end(text);
}

async function json(req) {
  const chunks = [];
  for await (const c of req) chunks.push(c);
  const raw = Buffer.concat(chunks).toString('utf8');
  return raw ? JSON.parse(raw) : {};
}

function completion(req, res, body) {
  const key = keys.get(sha(bearer(req)));
  if (!key || !key.active) return send(res, 401, { error: { message: 'invalid or revoked virtual key' } });
  const model = body.model ?? key.models[0] ?? 'fake/model';
  if (key.models.length && !key.models.includes(model)) {
    return send(res, 403, { error: { message: `model ${model} not allowed for this key` } });
  }
  const prompt = 1200;
  const completionTokens = 40;
  const cached = 1000;
  const cost = 0.0123;
  if (key.spend + cost > key.max_budget) return send(res, 400, { error: { message: `Budget has been exceeded! Current cost: ${key.spend + cost}, Max budget: ${key.max_budget}`, type: 'budget_exceeded', param: null, code: '400' } });
  key.spend += cost;
  const now = new Date();
  seq += 1;
  spend.push({
    request_id: `fake-req-${seq}-${randomBytes(4).toString('hex')}`,
    model,
    model_group: model,
    spend: cost,
    prompt_tokens: prompt,
    completion_tokens: completionTokens,
    startTime: gatewayTime(now),
    endTime: gatewayTime(now),
    cache_hit: 'False',
    metadata: {
      user_api_key_alias: key.alias,
      user_api_key_metadata: key.metadata,
      usage_object: { prompt_tokens: prompt, completion_tokens: completionTokens, cache_read_input_tokens: cached },
    },
  });
  send(res, 200, {
    id: `chatcmpl-fake-${seq}`,
    object: 'chat.completion',
    model,
    choices: [{ index: 0, message: { role: 'assistant', content: 'fake gateway: no model was called' }, finish_reason: 'stop' }],
    usage: { prompt_tokens: prompt, completion_tokens: completionTokens, total_tokens: prompt + completionTokens },
  });
}

createServer(async (req, res) => {
  try {
    const url = new URL(req.url, 'http://gateway');
    if (req.method === 'GET' && url.pathname === '/health') return send(res, 200, { status: 'ok', fake: true });
    if (['/v1/chat/completions', '/chat/completions', '/v1/messages'].includes(url.pathname) && req.method === 'POST') {
      return completion(req, res, await json(req));
    }
    if (bearer(req) !== master) return send(res, 401, { error: { message: 'master key required' } });
    if (req.method === 'POST' && url.pathname === '/key/generate') {
      const body = await json(req);
      const key = `sk-fake-${randomBytes(12).toString('hex')}`;
      const token = sha(key);
      keys.set(token, {
        key,
        alias: body.key_alias,
        models: body.models ?? [],
        metadata: body.metadata ?? {},
        max_budget: body.max_budget ?? 1,
        active: true,
        spend: 0,
        created: new Date().toISOString(),
      });
      return send(res, 200, { key, token, key_alias: body.key_alias, expires: null });
    }
    if (req.method === 'POST' && url.pathname === '/key/delete') {
      const body = await json(req);
      const deleted = (body.keys ?? []).filter((t) => keys.get(t)?.active);
      for (const t of deleted) keys.get(t).active = false;
      if (!deleted.length) return send(res, 404, { error: { message: 'key not found' } });
      return send(res, 200, { deleted_keys: deleted });
    }
    if (req.method === 'GET' && url.pathname === '/key/list') {
      const size = Math.max(1, Math.min(1000, Number(url.searchParams.get('size') ?? 10)));
      const page = Math.max(1, Number(url.searchParams.get('page') ?? 1));
      const active = [...keys.entries()].filter(([, k]) => k.active);
      const totalPages = Math.max(1, Math.ceil(active.length / size));
      return send(res, 200, {
        keys: active.slice((page - 1) * size, page * size).map(([token, k]) => ({
          token,
          key_alias: k.alias ?? null,
          created_at: k.created,
          expires: null,
          metadata: k.metadata,
        })),
        total_count: active.length,
        current_page: page,
        total_pages: totalPages,
      });
    }
    if (req.method === 'GET' && url.pathname === '/spend/logs/v2') {
      const alias = url.searchParams.get('key_alias');
      const start = url.searchParams.get('start_date') ?? '';
      const end = url.searchParams.get('end_date') ?? '9999';
      const pageSize = Math.max(1, Math.min(1000, Number(url.searchParams.get('page_size') ?? 100)));
      const page = Math.max(1, Number(url.searchParams.get('page') ?? 1));
      const rows = spend.filter(
        (r) => (!alias || r.metadata.user_api_key_alias === alias) && r.startTime >= start && r.startTime < end,
      );
      const totalPages = Math.max(1, Math.ceil(rows.length / pageSize));
      return send(res, 200, {
        data: rows.slice((page - 1) * pageSize, page * pageSize),
        total: rows.length,
        page,
        page_size: pageSize,
        total_pages: totalPages,
      });
    }
    send(res, 404, { error: { message: `no route ${req.method} ${url.pathname}` } });
  } catch (e) {
    send(res, 500, { error: { message: String(e) } });
  }
}).listen(port, '0.0.0.0', () => console.log(`fake gateway listening on :${port}`));
