import { renderPage } from './layout.mjs';
import { sendHtml } from '../http.mjs';

export function renderNotFound() {
  return renderPage('Not found', '<h1>Not found</h1>\n<p data-testid="not-found">Page not found</p>');
}

export function sendNotFound(res) {
  sendHtml(res, 404, renderNotFound());
}
