// Entry point: `node server.mjs`. Runs in the foreground; `guard up` puts it in
// the background and records its PID in .guard/app.pid, `guard down` kills it.
import http from 'node:http';
import { createApp } from './src/app.mjs';

const port = Number(process.env.PORT ?? 3000);
const host = '0.0.0.0';

const server = http.createServer(
  createApp({ token: process.env.GUARD_TEST_TOKEN, nodeEnv: process.env.NODE_ENV }),
);

server.listen(port, host, () => {
  const { port: actual } = server.address();
  console.log(`demo-shop listening on http://${host}:${actual}`);
});

const shutdown = () => {
  server.close(() => process.exit(0));
  server.closeAllConnections();
};
process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
