# Deploying guard

This folder holds two images and a Compose stack for `guard serve`. You do not need any of it to use guard as a CLI.

| File | What it is |
| --- | --- |
| `Dockerfile` | Builds guard. Two targets: `runner` (the default) and `server`. |
| `docker-compose.yml` | Runs `guard serve`, Postgres, an S3 evidence store and, under the `agents` profile, the LiteLLM gateway. |
| `docker-compose.agents.yml` | Optional override. It gives guard the Docker socket so the agent lane can start sandboxes. |
| `env.example` | Every variable the stack reads, by name only. Copy it to `deploy/.env`, which git ignores. |
| `litellm/config.yaml` | Gateway models. Provider keys come from the environment. |
| `postgres/init/` | Runs only when the Postgres volume is created for the first time. Creates the `litellm` database. |
| `sandbox/` | The image the agents run in (see `sandbox/README.md`). |

## Which host to use

| Host | Use it for |
| --- | --- |
| Linux with Docker Engine (amd64) | Production. Recommended. |
| Docker Desktop on Windows or macOS | Development. Everything here runs as Linux containers. |
| Windows Server | Only with Linux containers, inside WSL2 or a Linux VM. Postgres, the S3 store, LiteLLM and the agent sandboxes are Linux images, and Windows containers cannot run them. |

## Build the images

Run these from the repository root. BuildKit is required. It also applies `deploy/Dockerfile.dockerignore`, which sends only the Cargo workspace and the files guard embeds.

```sh
# runner (default): guard + Playwright browsers + the Docker CLI and compose plugin.
# Used as the CI gate image (ci/gitlab/templates/gate.yml).
docker build -f deploy/Dockerfile -t guard:dev .

# server: slim Debian image for guard serve. Includes git, curl and the Docker CLI.
# Runs as uid 10001 and checks GET /healthz.
docker build -f deploy/Dockerfile --target server -t guard-server:dev .
```

Both targets build only `guard-cli` (`cargo build --release --locked -p guard-cli --bin guard`). The `guard-gui` crate is copied in so Cargo can load the workspace, but it is never compiled, so the build needs no GUI system libraries.

## Run the stack

1. Copy the template: `cp deploy/env.example deploy/.env`.
2. Fill in the required values. Generate each secret with `openssl rand -hex 32`. Use hex because the passwords and keys go into URLs without percent-encoding.

   | Variable | What to set |
   | --- | --- |
   | `GUARD_REPO_DIR` | Absolute path of the app checkout. It needs `target.toml` and the catalog. |
   | `GUARD_SERVER_TOKEN` | Any secret. The bearer token for `/api/*`. |
   | `POSTGRES_PASSWORD` | Any secret. |
   | `S3_ACCESS_KEY` | Any value. The evidence store's access key. |
   | `S3_SECRET_KEY` | Any secret. |
   | `S3_PUBLIC_HOST` | DNS name that evidence readers (browsers, Jira, the PR) use to reach this host. Not `localhost`. |

3. Start the stack:

   ```sh
   docker compose -f deploy/docker-compose.yml up -d --build --wait
   curl -fsS http://localhost:8080/healthz
   ```

The startup line in `docker compose -f deploy/docker-compose.yml logs guard` lists what was wired, for example `store postgres` and `evidence configured`. Other adapters are set by name in `deploy/.env`: Jev, Jira, GitHub or GitLab, and the dashboard token. `docs/contracts/server-api.md` lists every variable.

| Service | Image | Role |
| --- | --- | --- |
| `guard` | `guard-server:dev` (built) | Runs `guard serve --repo /repo` on port 8080, or on `GUARD_PORT`. |
| `postgres` | `postgres:17.6-bookworm` | Decision log, gate history, memory and the agent queue. Data is kept in the `pg-data` volume. |
| `s3` | `chrislusf/seaweedfs:3.97` | Evidence store. S3 API on `:8333`, path-style addressing. Allows CORS from `https://trace.playwright.dev`. Data is kept in the `s3-data` volume. |
| `s3-init` | same | Runs once. Creates the bucket and sets its TTL (`S3_EVIDENCE_TTL`, default `14d`). Keep the TTL equal to `[evidence].retention_days`. |
| `litellm`, `redis` | `ghcr.io/berriai/litellm:v1.77.7-stable`, `redis:7.4.6-bookworm` | Run only with `--profile agents`. They are the model gateway and its cache. |

The evidence store is SeaweedFS, not MinIO, because MinIO no longer publishes its images on Docker Hub or Quay. guard's S3 adapter was checked against this SeaweedFS version with the live `guard-store` evidence test. Any S3-compatible store that supports path-style addressing works. To use one, change only `GUARD_STORAGE_URL`, which has the form `s3+https://<key>:<secret>@<host>/<bucket>`.

Evidence links only open if readers can reach the S3 host in `GUARD_STORAGE_URL`. guard signs every evidence link (Jira comments, the PR, `report_url`, the trace `viewer_url`) for exactly that host with SigV4, and the signature covers the host, so a link cannot be rewritten afterwards. The stack therefore uses `S3_PUBLIC_HOST:8333` rather than the service name `s3`. It publishes port 8333 on the host, and it adds `S3_PUBLIC_HOST` as a network alias of `s3` so guard resolves the same name inside the stack. Do not use `localhost`: inside the stack it is the guard container. Readers must reach port 8333 over plain HTTP, and `https://trace.playwright.dev` loads traces from that address too. Some browsers block an HTTPS page from fetching an HTTP resource, so put a TLS proxy in front of the store and switch the URL to `s3+https://` if traces must open from there. Object requests still need the credentials or a valid signature.

## Agent lane

The agent lane needs three things:

- `agents.toml` in the checkout.
- thresholds in active mode.
- a Docker engine, so that guard can start the Linux sandboxes (`docs/contracts/sandbox.md`).

It is off by default. To turn it on:

1. Build the sandbox image and push it by digest, as described in `sandbox/README.md`. Reference it from `agents.toml`.
2. In `agents.toml`, set `[gateway].url = "http://litellm:4000"` and `master_key_env = "LITELLM_MASTER_KEY"`.
3. In `deploy/.env`, set these:
   - `LITELLM_MASTER_KEY`
   - `REDIS_PASSWORD`
   - the provider keys you use: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` or `GEMINI_API_KEY`
   - `DOCKER_GID`, the host's docker group id (`getent group docker | cut -d: -f3`)
   - `GUARD_AGENT_WORK_DIR`
4. Create the work directory on the host and give it to uid 10001: `sudo install -d -o 10001 -g 10001 "$GUARD_AGENT_WORK_DIR"`. The host engine resolves the sandboxes' bind mounts on the host, so the path must be the same on the host and in the container. The override mounts it at the same path. The checkout in `GUARD_REPO_DIR` must also be writable by uid 10001, because the lane writes under `.guard/agents/`.
5. Start the stack:

   ```sh
   docker compose -f deploy/docker-compose.yml -f deploy/docker-compose.agents.yml \
     --profile agents up -d --build --wait
   ```

Sandbox egress forwarders start on the `guard-gateway` network (`GUARD_SANDBOX_EGRESS_NETWORK`). Only `litellm` and `guard` are attached to it, so a sandbox can reach the gateway but not Postgres or the evidence store.

### The Docker socket gives control of the host

A container that has `/var/run/docker.sock` controls the host's Docker engine, and that engine runs as root on the host. It can start privileged containers and mount any host path. Whoever compromises `guard serve`, for example through a leaked `GUARD_SERVER_TOKEN`, a webhook bug or a bad dependency, then controls the host. The sandboxes are hardened (read-only, no capabilities, an internal network). The orchestrator that holds the socket is not.

That is why the socket mount lives in a separate override file and is never in the default stack. If you enable it:

- Use a dedicated VM or host, never a shared one.
- Prefer rootless Docker, or gVisor with `GUARD_SANDBOX_RUNTIME=runsc`.
- Keep port 8080 on an internal network.

The alternative is a socket proxy such as docker-socket-proxy. It does not remove the risk, because the lane still needs to create containers, run `exec` and bind-mount host paths.

### Why the socket is a separate file and not a profile

Compose profiles can only turn whole services on or off. They cannot add a mount to a service that is already defined. Compose also interpolates services in profiles that are off. So the gateway services check their required keys (`LITELLM_MASTER_KEY`, `REDIS_PASSWORD`) when the container starts, instead of with `${VAR:?}`.

## Stop the stack

```sh
docker compose -f deploy/docker-compose.yml --profile agents down      # keeps the volumes
docker compose -f deploy/docker-compose.yml --profile agents down -v   # also deletes the data
```

`guard serve` shuts down cleanly on SIGTERM. In-flight work gets 8 seconds, which is within Compose's default 10-second stop timeout.
