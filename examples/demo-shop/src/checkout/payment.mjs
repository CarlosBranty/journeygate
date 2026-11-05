// Fake payment provider: validates card details and applies the test-card rules.

export const DECLINED_CARD = '4000000000000002';

const INVALID = { ok: false, status: 400, message: 'Invalid card details' };
const DECLINED = { ok: false, status: 402, message: 'Your card was declined' };

export function evaluatePayment({ cardNumber, expiry, cvc }) {
  const number = typeof cardNumber === 'string' ? cardNumber.replace(/ /g, '') : '';
  if (!/^\d{16}$/.test(number)) return INVALID;
  if (typeof expiry !== 'string' || !/^(0[1-9]|1[0-2])\/\d{2}$/.test(expiry)) return INVALID;
  if (typeof cvc !== 'string' || !/^\d{3,4}$/.test(cvc)) return INVALID;
  if (number === DECLINED_CARD) return DECLINED;
  return { ok: true };
}
