# Agent sandbox image and network

The image every QA and Coder agent runs in, and how `guard-sandbox`'s `DockerSandbox` isolates it. The contract is `docs/contracts/sandbox.md`.

## What the image contains

| Component | Version | Source |
| --- | --- | --- |
| Base | `node:24-bookworm-slim@sha256:c86ecead…920c` | Docker Hub, pinned by digest |
| Playwright + Chromium (with system deps) | `@playwright/test` 1.64.1 | npm, the same version as `e2e-playwright` (load-bearing: check sandboxes replace the candidate's copy with this global one) |
| TypeScript | 6.0.3 | npm, the same version as `e2e-playwright` |
| CodeGraph CLI | `@colbymchenry/codegraph` 1.6.2 | npm |
| rtk-oss (the Apache-2.0 build of rtk) | 1.0.1 | `github.com/rtk-ai/rtk` release assets `rtk-oss-*`, checked by SHA-256 |
| git, curl, ca-certificates | Debian bookworm | apt |

`CODEGRAPH_TELEMETRY=0` and `DO_NOT_TRACK=1` are set in the image. `SandboxSpec::validate` also requires them on every sandbox.

rtk 1.1.0 is the latest release, but it was published on 2026-10-25, less than two weeks ago, so the image pins 1.0.1 (published 2026-10-14). The x86_64 checksum was verified against a local download. The arm64 checksum was copied from that release's `checksums.txt` and has not been tested.

## Build

```sh
docker build -f deploy/sandbox/Dockerfile -t guard-sandbox:dev deploy/sandbox
```

Built on 2026-11-01 (Docker 24.0.7, amd64) in about 4 minutes. The result is 1.7 GB, mostly Chromium.

The adapter only accepts an image referenced by digest (`repo@sha256:<64 hex>`). A local-only image has no digest, so push it to a registry first and use the digest the push reports. A local `registry:2` is enough:

```sh
docker run -d --name guard-registry -p 127.0.0.1:55000:5000 registry:2.8.3
docker tag guard-sandbox:dev localhost:55000/guard-sandbox:dev
docker push localhost:55000/guard-sandbox:dev
```

## Egress: how a sandbox reaches only the allowlist

The adapter uses a dedicated internal network plus one forwarder container per allowlist rule. It does not use an HTTP proxy.

1. `docker network create --internal <sandbox>-net`. An internal network has no default route, so nothing on it can reach the internet, the host, or `host.docker.internal`.
2. For each `EgressRule {host, port}`, a forwarder container `<sandbox>-egress-<i>` starts on the egress network (`DockerConfig::egress_network`, the network where the gateway lives). It is then attached to the internal network. It runs `guard-sandbox/assets/egress-proxy.mjs`, a TCP relay from `:port` to exactly `host:port`, passed with `node -e`. The forwarder is hardened like the sandbox: `--read-only`, `--cap-drop ALL`, `no-new-privileges`, non-root, and pid and memory caps.
3. The sandbox joins only the internal network and gets the following:
   - `--add-host <rule.host>:<forwarder ip>` for each rule, so the allowed names resolve to their forwarders.
   - `--dns 127.0.0.1`, so Docker's embedded resolver has no working upstream. No other name resolves, which closes the DNS exfiltration path.

The forwarder relays raw TCP, so TLS stays end-to-end between the agent and the real host.

Verified live by `guard-sandbox/tests/docker_live.rs::egress_reaches_only_the_allowed_host`:

- The allowed upstream (an nginx container on the egress network) answers 200 through its forwarder.
- `http://example.com`, `http://1.1.1.1` and `http://host.docker.internal` all fail from inside the sandbox.
- `example.com` does not resolve.

As a control, the same image on the default bridge network reaches all three hosts.

## Other isolation (set by the adapter, not the image)

- `--read-only` root filesystem, with size-capped tmpfs at `/tmp` and `$HOME`.
- `--cap-drop ALL` and `--security-opt no-new-privileges`. Verified live: `CapEff` is all zeros and `NoNewPrivs` is 1.
- `--pids-limit`, `--cpus` and `--memory`/`--memory-swap` from `ResourceLimits`.
- `--user 1000:1000` and `--init`.
- `--runtime=runsc` when `docker info` lists gVisor. It is not installed on the dev machine, so this path was tested only against a fake CLI.
- Secrets (the virtual key) are never in `docker` argv or in the container config. They are set on the `docker exec` process environment and forwarded by name (`-e NAME`). Verified live with `docker inspect`.
- Hidden mounts (the Coder's `tests/`) are a read-only bind of an empty host "skeleton" directory. It holds only the mount points of the mounts nested under it, so the Coder sees `tests/features/<KEY>.spec.ts` (the read-only sealed copy) and nothing else. A read-only tmpfs cannot be used here, because Docker cannot create the nested mount point inside it.

Rootless mode is a daemon setting. The adapter cannot enforce it, and Docker Desktop on the dev machine is not rootless.

## Live tests

```sh
GUARD_DOCKER_TESTS=1 \
GUARD_SANDBOX_TEST_IMAGE=localhost:55000/guard-sandbox@sha256:<digest> \
  cargo test -p guard-sandbox --test docker_live -- --include-ignored
```
