// Account area: shows the session created by POST /__test__/login.
import { sendHtml } from '../http.mjs';
import { escapeHtml, renderPage } from '../views/layout.mjs';

export function renderAccountPage(session) {
  return renderPage('Account', `<h1>Your account</h1>
<dl>
<dt>Email</dt>
<dd data-testid="account-email">${escapeHtml(session.email)}</dd>
<dt>Role</dt>
<dd data-testid="account-role">${escapeHtml(session.role)}</dd>
</dl>`);
}

export function renderLoginRequired() {
  return renderPage('Account', `<h1>Sign in required</h1>
<p data-testid="login-required">Please sign in</p>`);
}

export function registerAccount(router, { store }) {
  router.add('GET', '/account', (req, res, { cookies }) => {
    const session = store.getSession(cookies.sid);
    if (!session) return sendHtml(res, 401, renderLoginRequired());
    sendHtml(res, 200, renderAccountPage(session));
  });
}
