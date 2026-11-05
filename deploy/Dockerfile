# syntax=docker/dockerfile:1.7
# guard images. Build from the repository root (BuildKit is required; it also
# picks up deploy/Dockerfile.dockerignore, which whitelists the build context):
#
#   docker build -f deploy/Dockerfile -t guard:dev .                       # runner (default)
#   docker build -f deploy/Dockerfile --target server -t guard-server:dev .
#
# Targets:
#   runner  (default, last stage) guard on top of the official Playwright image,
#           plus the Docker CLI and the compose plugin. The CI gate image
#           (ci/gitlab/templates/gate.yml) and anything that runs journeys.
#   server  slim Debian image for `guard serve` (deploy/docker-compose.yml).
#           Ships git, curl and the Docker CLI; the agent lane only works when
#           a Docker engine is reachable (deploy/README.md, "Agent lane").
#
# Keep PLAYWRIGHT_VERSION equal to the exact @playwright/test pin in
# e2e-playwright/package.json (override with --build-arg).

ARG RUST_VERSION=1.88.0
ARG PLAYWRIGHT_VERSION=1.64.1
ARG DEBIAN_VERSION=bookworm-20251020-slim

# ---- build: compile guard in release mode -----------------------------------
FROM rust:${RUST_VERSION}-bookworm AS build
WORKDIR /src
# Cargo loads the whole workspace, so every member's manifest and sources must
# be present even though only guard-cli is built (guard-gui is never compiled,
# so no GUI system libraries are needed). The image already ships the
# toolchain; rust-toolchain.toml is left out so rustup does not download extra
# components inside the build.
COPY Cargo.toml Cargo.lock ./
COPY guard-core ./guard-core
COPY guard-cli ./guard-cli
COPY guard-jev ./guard-jev
COPY guard-impact ./guard-impact
COPY guard-tracker ./guard-tracker
COPY guard-codehost ./guard-codehost
COPY guard-store ./guard-store
COPY guard-server ./guard-server
COPY guard-mcp ./guard-mcp
COPY guard-sandbox ./guard-sandbox
COPY guard-runtimes ./guard-runtimes
COPY guard-agents ./guard-agents
COPY guard-map ./guard-map
COPY guard-gui ./guard-gui
# Files embedded at compile time from outside the crates (include_str!):
# `guard init` scaffolding and the impact queries.
COPY templates ./templates
COPY ci/gitlab/templates ./ci/gitlab/templates
COPY languages ./languages
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    set -eu; \
    cargo build --release --locked -p guard-cli --bin guard; \
    install -D -m 0755 target/release/guard /out/guard

# ---- docker CLI + compose plugin -----------------------------------------------
FROM docker:27.5.1-cli AS docker-cli
FROM docker/compose-bin:v2.32.4 AS compose-bin

# ---- server: `guard serve` ---------------------------------------------------------
FROM debian:${DEBIAN_VERSION} AS server
# git: impact index, default-branch resolution and agent clones. curl: the
# HEALTHCHECK and https readiness probes. The app checkout is bind-mounted at
# /repo and usually owned by another uid, so it is marked safe for git.
RUN set -eu; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl git; \
    rm -rf /var/lib/apt/lists/*; \
    groupadd --system --gid 10001 guard; \
    useradd --system --uid 10001 --gid guard --home-dir /home/guard --create-home --shell /usr/sbin/nologin guard; \
    git config --system --add safe.directory /repo
# The agent lane drives sandbox containers with the docker CLI. It stays unused
# unless a Docker engine is reachable (socket mount or DOCKER_HOST).
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=build /out/guard /usr/local/bin/guard
ENV NO_COLOR=1
USER guard
WORKDIR /repo
RUN guard --version
EXPOSE 8080
# /healthz answers 200 while the process runs (docs/contracts/server-api.md).
HEALTHCHECK --interval=15s --timeout=5s --start-period=30s --retries=3 \
    CMD ["curl", "-fsS", "-o", "/dev/null", "http://127.0.0.1:8080/healthz"]
# guard serve shuts down gracefully on SIGTERM (8 s grace for in-flight work).
STOPSIGNAL SIGTERM
ENTRYPOINT ["guard"]
CMD ["serve", "--repo", "/repo"]

# ---- runner (default): Playwright browsers + Node + guard ------------------------
FROM mcr.microsoft.com/playwright:v${PLAYWRIGHT_VERSION}-noble AS runner
COPY --from=docker-cli /usr/local/bin/docker /usr/local/bin/docker
COPY --from=compose-bin /docker-compose /usr/local/libexec/docker/cli-plugins/docker-compose
COPY --from=build /out/guard /usr/local/bin/guard
ENV NO_COLOR=1
WORKDIR /work
RUN guard --version
CMD ["guard", "--help"]
