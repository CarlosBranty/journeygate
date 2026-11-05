import { parseList } from '../config/env';

const ROLE_PATTERN = /^[a-z][a-z0-9_-]*$/;

/** Roles from GUARD_ROLES; the setup project logs in once per role. */
export function rolesFromEnv(value: string | undefined): string[] {
  const roles = parseList(value);
  for (const role of roles) {
    if (!ROLE_PATTERN.test(role)) {
      throw new Error(`GUARD_ROLES entry "${role}" must match ${ROLE_PATTERN.source}`);
    }
  }
  return roles;
}

/** storageState file of a role, relative to the Playwright project dir. */
export function authStatePath(role: string): string {
  return `.auth/${role}.json`;
}

/** Path of the test login endpoint: GUARD_LOGIN_PATH, else the contract default. */
export function loginPath(value: string | undefined): string {
  return value ? value : '/__test__/login';
}

/** Header sent on every login and seed call. */
export function testTokenHeaders(): Record<string, string> {
  return { 'x-test-token': process.env.GUARD_TEST_TOKEN ?? '' };
}
