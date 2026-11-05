// Smoke test of the real entry point: spawns `node server.mjs` like `guard up` does.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { once } from 'node:events';

const serverPath = fileURLToPath(new URL('../server.mjs', import.meta.url));

function startServer(env) {
  const child = spawn(process.execPath, [serverPath], {
    env: { ...process.env, PORT: '0', NODE_ENV: '', GUARD_TEST_TOKEN: '', ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const ready = new Promise((resolve, reject) => {
    let out = '';
    child.stdout.on('data', (chunk) => {
      out += chunk;
      const m = out.match(/listening on http:\/\/0\.0\.0\.0:(\d+)/);
      if (m) resolve(Number(m[1]));
    });
    child.once('exit', (code) => reject(new Error(`server exited early with ${code}: ${out}`)));
  });
  return { child, ready };
}

async function stop(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  const exited = once(child, 'exit');
  child.kill('SIGTERM');
  await exited;
}

test('server.mjs binds 0.0.0.0, serves healthz and uses the default token', async () => {
  const { child, ready } = startServer({});
  try {
    const port = await ready;
    const health = await fetch(`http://127.0.0.1:${port}/healthz`);
    assert.equal(health.status, 200);
    const login = await fetch(`http://127.0.0.1:${port}/__test__/login`, {
      method: 'POST',
      headers: { 'x-test-token': 'demo-token', 'content-type': 'application/json' },
      body: JSON.stringify({ role: 'customer' }),
    });
    assert.equal(login.status, 200);
  } finally {
    await stop(child);
  }
});

test('server.mjs reads GUARD_TEST_TOKEN and NODE_ENV', async () => {
  const { child, ready } = startServer({ GUARD_TEST_TOKEN: 'secret-1', NODE_ENV: 'production' });
  try {
    const port = await ready;
    const login = await fetch(`http://127.0.0.1:${port}/__test__/login`, {
      method: 'POST',
      headers: { 'x-test-token': 'secret-1', 'content-type': 'application/json' },
      body: JSON.stringify({ role: 'customer' }),
    });
    assert.equal(login.status, 404);
  } finally {
    await stop(child);
  }
});

test('server.mjs uses GUARD_TEST_TOKEN outside production', async () => {
  const { child, ready } = startServer({ GUARD_TEST_TOKEN: 'secret-2' });
  try {
    const port = await ready;
    const call = (token) =>
      fetch(`http://127.0.0.1:${port}/__test__/login`, {
        method: 'POST',
        headers: { 'x-test-token': token, 'content-type': 'application/json' },
        body: JSON.stringify({ role: 'customer' }),
      });
    assert.equal((await call('demo-token')).status, 401);
    assert.equal((await call('secret-2')).status, 200);
  } finally {
    await stop(child);
  }
});
