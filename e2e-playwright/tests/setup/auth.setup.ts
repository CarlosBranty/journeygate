import { expect, test as setup } from '@playwright/test';
import { authStatePath, loginPath, rolesFromEnv, testTokenHeaders } from '../../src/fixtures/auth';

// One storageState per role from GUARD_ROLES, created by API login at
// GUARD_LOGIN_PATH (faster and less flaky than the UI). No roles = no setup tests.
for (const role of rolesFromEnv(process.env.GUARD_ROLES)) {
  setup(`authenticate as ${role}`, async ({ request }) => {
    const res = await request.post(loginPath(process.env.GUARD_LOGIN_PATH), { data: { role }, headers: testTokenHeaders() });
    await expect(res, `test login failed for ${role}`).toBeOK();
    await request.storageState({ path: authStatePath(role) });
  });
}
