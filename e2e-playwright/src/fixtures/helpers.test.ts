import { describe, expect, it } from 'vitest';
import { compileBlockedHosts, isBlockedHost } from './blocked-domains';
import { dataNamespace } from './namespace';
import { installSeededRandom, seedFromTestId } from './seeded-random';
import { authStatePath, loginPath, rolesFromEnv } from './auth';

describe('dataNamespace', () => {
  it('combines run id, test id, repeat index and retry', () => {
    expect(dataNamespace({ runId: 'r1793452800-4242', testId: 'a1b2c3-d4e5', repeatEachIndex: 0, retry: 1 })).toBe(
      'r1793452800-4242-a1b2c3-d4e5-x0-r1',
    );
  });

  it('falls back to local when RUN_ID is unset or empty', () => {
    expect(dataNamespace({ runId: undefined, testId: 't', repeatEachIndex: 2, retry: 0 })).toBe('local-t-x2-r0');
    expect(dataNamespace({ runId: '', testId: 't', repeatEachIndex: 0, retry: 0 })).toBe('local-t-x0-r0');
  });

  it('rejects a RUN_ID outside the contract pattern', () => {
    expect(() => dataNamespace({ runId: 'bad id', testId: 't', repeatEachIndex: 0, retry: 0 })).toThrow(/RUN_ID/);
  });

  it('produces a namespace the demo app accepts', () => {
    const ns = dataNamespace({ runId: 'x'.repeat(64), testId: 'f'.repeat(41), repeatEachIndex: 9, retry: 2 });
    expect(ns).toMatch(/^[A-Za-z0-9._-]{1,200}$/);
  });
});

describe('isBlockedHost', () => {
  const blocked = compileBlockedHosts('*.google-analytics.com,cdn.intercom.io,,');

  it('blocks a wildcard apex and any subdomain', () => {
    expect(isBlockedHost('google-analytics.com', blocked)).toBe(true);
    expect(isBlockedHost('www.google-analytics.com', blocked)).toBe(true);
    expect(isBlockedHost('a.b.google-analytics.com', blocked)).toBe(true);
  });

  it('does not block look-alike hosts', () => {
    expect(isBlockedHost('evilgoogle-analytics.com', blocked)).toBe(false);
    expect(isBlockedHost('google-analytics.com.evil.io', blocked)).toBe(false);
  });

  it('matches an exact pattern only exactly', () => {
    expect(isBlockedHost('cdn.intercom.io', blocked)).toBe(true);
    expect(isBlockedHost('x.cdn.intercom.io', blocked)).toBe(false);
    expect(isBlockedHost('intercom.io', blocked)).toBe(false);
  });

  it('is case-insensitive', () => {
    expect(isBlockedHost('WWW.Google-Analytics.COM', blocked)).toBe(true);
    expect(isBlockedHost('cdn.intercom.io', compileBlockedHosts('CDN.Intercom.IO'))).toBe(true);
  });

  it('blocks nothing when no patterns are given', () => {
    expect(compileBlockedHosts('')).toEqual([]);
    expect(isBlockedHost('localhost', compileBlockedHosts(undefined))).toBe(false);
  });
});

describe('seeded random', () => {
  it('derives a stable 32-bit seed from the test id', () => {
    const seed = seedFromTestId('a1b2c3-d4e5');
    expect(seed).toBe(seedFromTestId('a1b2c3-d4e5'));
    expect(seed).not.toBe(seedFromTestId('a1b2c3-d4e6'));
    expect(Number.isInteger(seed) && seed >= 0 && seed < 2 ** 32).toBe(true);
  });

  it('replaces Math.random with a reproducible generator in [0, 1)', () => {
    const run = (seed: number) => {
      const fake = { random: () => 0.5 } as { random: () => number };
      installSeededRandom(seed, fake);
      return Array.from({ length: 5 }, () => fake.random());
    };
    const first = run(42);
    expect(run(42)).toEqual(first);
    expect(run(43)).not.toEqual(first);
    expect(first.every((n) => n >= 0 && n < 1)).toBe(true);
  });
});

describe('auth helpers', () => {
  it('reads roles from GUARD_ROLES', () => {
    expect(rolesFromEnv('customer,admin')).toEqual(['customer', 'admin']);
    expect(rolesFromEnv('')).toEqual([]);
    expect(rolesFromEnv(undefined)).toEqual([]);
  });

  it('rejects a role outside the contract pattern', () => {
    expect(() => rolesFromEnv('Customer')).toThrow(/GUARD_ROLES/);
  });

  it('stores state under .auth/<role>.json', () => {
    expect(authStatePath('customer')).toBe('.auth/customer.json');
  });

  it('posts the login to GUARD_LOGIN_PATH, defaulting to /__test__/login', () => {
    expect(loginPath('/qa/login')).toBe('/qa/login');
    expect(loginPath(undefined)).toBe('/__test__/login');
    expect(loginPath('')).toBe('/__test__/login');
  });
});
