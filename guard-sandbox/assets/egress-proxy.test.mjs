// Tests of egress-proxy.mjs: a single-destination TCP forwarder.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import net from 'node:net';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { readFileSync } from 'node:fs';
import path from 'node:path';

const here = path.dirname(fileURLToPath(import.meta.url));
const source = readFileSync(path.join(here, 'egress-proxy.mjs'), 'utf8');

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve(server.address().port)));
}

function startProxy(env) {
  // Same invocation the Docker adapter uses: the source passed with -e.
  const child = spawn(process.execPath, ['--input-type=module', '-e', source], {
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  return new Promise((resolve, reject) => {
    let out = '';
    child.stdout.on('data', (d) => {
      out += d;
      if (out.includes('guard-egress ready')) resolve(child);
    });
    child.on('exit', (code) => reject(new Error(`proxy exited ${code}: ${out}`)));
  });
}

function roundTrip(port, payload) {
  return new Promise((resolve, reject) => {
    const s = net.connect(port, '127.0.0.1', () => s.end(payload));
    let got = '';
    s.on('data', (d) => { got += d; });
    s.on('end', () => resolve(got));
    s.on('error', reject);
  });
}

test('forwards bytes both ways to the one configured target', async () => {
  const upstream = net.createServer((c) => {
    let buf = '';
    c.on('data', (d) => { buf += d; });
    c.on('end', () => c.end(`echo:${buf}`));
  });
  const upstreamPort = await listen(upstream);
  const free = net.createServer();
  const listenPort = await listen(free);
  await new Promise((r) => free.close(r));
  const proxy = await startProxy({
    GUARD_EGRESS_LISTEN_PORT: String(listenPort),
    GUARD_EGRESS_LISTEN_HOST: '127.0.0.1',
    GUARD_EGRESS_TARGET_HOST: '127.0.0.1',
    GUARD_EGRESS_TARGET_PORT: String(upstreamPort),
  });
  try {
    assert.equal(await roundTrip(listenPort, 'ping'), 'echo:ping');
  } finally {
    proxy.kill();
    upstream.close();
  }
});

test('exits 2 when the target is not configured', async () => {
  const child = spawn(process.execPath, ['--input-type=module', '-e', source], {
    env: { ...process.env, GUARD_EGRESS_TARGET_HOST: '', GUARD_EGRESS_TARGET_PORT: '' },
  });
  const code = await new Promise((r) => child.on('exit', r));
  assert.equal(code, 2);
});
