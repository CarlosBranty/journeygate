import { test } from 'node:test';
import assert from 'node:assert/strict';
import { parseCookies, serializeCookie } from '../src/http.mjs';

test('parseCookies reads name=value pairs', () => {
  assert.deepEqual(parseCookies('sid=abc; cart=def'), { sid: 'abc', cart: 'def' });
});

test('parseCookies tolerates a missing header and junk', () => {
  assert.deepEqual(parseCookies(undefined), {});
  assert.deepEqual(parseCookies('junk; =x; a=1'), { a: '1' });
});

test('parseCookies keeps the first occurrence of a name', () => {
  assert.deepEqual(parseCookies('a=1; a=2'), { a: '1' });
});

test('serializeCookie writes session cookie attributes', () => {
  assert.equal(serializeCookie('sid', 'abc'), 'sid=abc; Path=/; HttpOnly; SameSite=Lax');
});

test('serializeCookie can expire a cookie', () => {
  assert.equal(
    serializeCookie('cart', '', { maxAge: 0 }),
    'cart=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0',
  );
});
