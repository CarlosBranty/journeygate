import { test } from 'node:test';
import assert from 'node:assert/strict';
import { escapeHtml, formatPrice, renderPage } from '../src/views/layout.mjs';

test('formatPrice renders dollars with two decimals', () => {
  assert.equal(formatPrice(1999), '$19.99');
  assert.equal(formatPrice(0), '$0.00');
  assert.equal(formatPrice(5), '$0.05');
  assert.equal(formatPrice(100000), '$1000.00');
});

test('escapeHtml escapes markup characters', () => {
  assert.equal(escapeHtml(`<a href="x">'&'</a>`), '&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;');
});

test('renderPage produces the shared layout', () => {
  const html = renderPage('Checkout', '<h1>Checkout</h1>');
  assert.match(html, /^<!doctype html>/);
  assert.match(html, /<html lang="en">/);
  assert.match(html, /<title>Checkout \| Demo Shop<\/title>/);
  assert.match(html, /<nav aria-label="Main">/);
  assert.match(html, /<a href="\/checkout">Checkout<\/a>/);
  assert.match(html, /<a href="\/account">Account<\/a>/);
  assert.match(html, /<h1>Checkout<\/h1>/);
});

test('renderPage escapes the title', () => {
  assert.match(renderPage('<b>', ''), /<title>&lt;b&gt; \| Demo Shop<\/title>/);
});
