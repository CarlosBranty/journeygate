// Egress forwarder of one sandbox allowlist rule (docs/contracts/sandbox.md, "Network").
//
// The Docker adapter starts one forwarder container per `EgressRule`. It sits
// on the sandbox's internal network (no route out) and on the egress network,
// and relays TCP from GUARD_EGRESS_LISTEN_PORT to exactly one destination,
// GUARD_EGRESS_TARGET_HOST:GUARD_EGRESS_TARGET_PORT. The sandbox resolves the
// rule host to this container through `--add-host`; nothing else is relayed.
//
// The adapter passes this file's source with `node --input-type=module -e`,
// so it must stay a self-contained module with no imports outside node:.
import net from 'node:net';
import process from 'node:process';

const targetHost = process.env.GUARD_EGRESS_TARGET_HOST ?? '';
const targetPort = Number(process.env.GUARD_EGRESS_TARGET_PORT ?? '');
const listenPort = Number(process.env.GUARD_EGRESS_LISTEN_PORT ?? targetPort);
const listenHost = process.env.GUARD_EGRESS_LISTEN_HOST || '0.0.0.0';

if (!targetHost || !Number.isInteger(targetPort) || targetPort <= 0 || !Number.isInteger(listenPort) || listenPort <= 0) {
  process.stderr.write('guard-egress: GUARD_EGRESS_TARGET_HOST and GUARD_EGRESS_TARGET_PORT are required\n');
  process.exit(2);
}

// Half-open on both legs so a client that ends its request still gets the reply.
const server = net.createServer({ allowHalfOpen: true }, (client) => {
  const upstream = net.connect({ port: targetPort, host: targetHost, allowHalfOpen: true });
  client.pipe(upstream);
  upstream.pipe(client);
  const close = () => {
    client.destroy();
    upstream.destroy();
  };
  client.on('error', close);
  upstream.on('error', close);
});

server.listen(listenPort, listenHost, () => {
  process.stdout.write(`guard-egress ready ${listenHost}:${listenPort} -> ${targetHost}:${targetPort}\n`);
});

for (const sig of ['SIGTERM', 'SIGINT']) process.on(sig, () => server.close(() => process.exit(0)));
