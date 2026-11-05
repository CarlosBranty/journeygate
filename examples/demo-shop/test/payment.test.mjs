import { test } from 'node:test';
import assert from 'node:assert/strict';
import { evaluatePayment } from '../src/checkout/payment.mjs';

const card = (overrides = {}) => ({
  cardNumber: '4242 4242 4242 4242',
  expiry: '12/34',
  cvc: '123',
  ...overrides,
});

test('approves a valid card, ignoring spaces', () => {
  assert.deepEqual(evaluatePayment(card()), { ok: true });
  assert.deepEqual(evaluatePayment(card({ cardNumber: '4242424242424242' })), { ok: true });
});

test('declines the decline test card', () => {
  assert.deepEqual(evaluatePayment(card({ cardNumber: '4000 0000 0000 0002' })), {
    ok: false,
    status: 402,
    message: 'Your card was declined',
  });
});

const invalid = { ok: false, status: 400, message: 'Invalid card details' };

for (const [label, overrides] of [
  ['short number', { cardNumber: '4242' }],
  ['letters in number', { cardNumber: '4242 4242 4242 424x' }],
  ['missing number', { cardNumber: undefined }],
  ['bad expiry format', { expiry: '1234' }],
  ['month 13', { expiry: '13/34' }],
  ['month 00', { expiry: '00/34' }],
  ['single-digit month', { expiry: '1/34' }],
  ['cvc too short', { cvc: '12' }],
  ['cvc too long', { cvc: '12345' }],
  ['cvc letters', { cvc: '12a' }],
]) {
  test(`rejects ${label}`, () => {
    assert.deepEqual(evaluatePayment(card(overrides)), invalid);
  });
}

test('accepts a 4-digit cvc and month 01', () => {
  assert.deepEqual(evaluatePayment(card({ cvc: '1234', expiry: '01/30' })), { ok: true });
});
