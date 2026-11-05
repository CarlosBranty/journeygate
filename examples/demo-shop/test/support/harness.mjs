// Starts the app on an ephemeral port and offers a tiny fetch wrapper with a
// per-client cookie jar, so HTTP tests read like the browser flows they mimic.
import http from 'node:http';
import { createApp } from '../../src/app.mjs';

export const TOKEN = 'test-token';

export async function startApp(options = {}) {
  const server = http.createServer(createApp({ token: TOKEN, nodeEnv: 'test', ...options }));
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  const baseUrl = `http://127.0.0.1:${port}`;
  return {
    baseUrl,
    client: () => createClient(baseUrl),
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

export function createClient(baseUrl) {
  const jar = new Map();
  const request = async (method, path, { headers = {}, json, form, body } = {}) => {
    const finalHeaders = { ...headers };
    let payload = body;
    if (json !== undefined) {
      finalHeaders['content-type'] = 'application/json';
      payload = JSON.stringify(json);
    }
    if (form !== undefined) {
      finalHeaders['content-type'] = 'application/x-www-form-urlencoded';
      payload = new URLSearchParams(form).toString();
    }
    if (jar.size > 0) {
      finalHeaders.cookie = [...jar].map(([k, v]) => `${k}=${v}`).join('; ');
    }
    const res = await fetch(baseUrl + path, { method, headers: finalHeaders, body: payload, redirect: 'manual' });
    const setCookies = res.headers.getSetCookie();
    for (const line of setCookies) {
      const [pair, ...attrs] = line.split(';').map((s) => s.trim());
      const eq = pair.indexOf('=');
      const name = pair.slice(0, eq);
      const value = pair.slice(eq + 1);
      if (attrs.some((a) => a.toLowerCase() === 'max-age=0')) jar.delete(name);
      else jar.set(name, value);
    }
    const text = await res.text();
    return { status: res.status, headers: res.headers, setCookies, text, json: () => JSON.parse(text) };
  };
  const testApi = (method, path, options = {}) =>
    request(method, path, { ...options, headers: { 'x-test-token': TOKEN, ...(options.headers ?? {}) } });
  return { request, testApi, jar };
}

/** Extracts the text content of the first element carrying data-testid="<id>". */
export function testIdText(html, id) {
  const re = new RegExp(`<([a-z0-9]+)[^>]*data-testid="${id}"[^>]*>([\\s\\S]*?)</\\1>`);
  const m = html.match(re);
  return m ? m[2].replace(/<[^>]+>/g, '').trim() : undefined;
}

export function countTestId(html, id) {
  return (html.match(new RegExp(`data-testid="${id}"`, 'g')) ?? []).length;
}
