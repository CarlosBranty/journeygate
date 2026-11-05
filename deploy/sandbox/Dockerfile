# syntax=docker/dockerfile:1
#
# guard agent sandbox image (docs/contracts/sandbox.md).
#
# Node 24 + Playwright (Chromium and its system deps) + git + the CodeGraph
# CLI + TypeScript + rtk-oss. Every version is pinned; the base by digest.
# The adapter (guard-sandbox `DockerSandbox`) refuses an image that is not
# referenced by digest, so push this image and use `<repo>@sha256:<digest>`.
#
#   docker build -f deploy/sandbox/Dockerfile -t guard-sandbox:dev deploy/sandbox
#
# Runtime flags (--read-only, --cap-drop=ALL, no-new-privileges, tmpfs /tmp
# and $HOME, the internal network) are set by the adapter, not here.

# node:24-bookworm-slim, resolved 2026-11-01.
FROM node:24-bookworm-slim@sha256:c86ecead9e55a5210ac1727de38a4ca840bf03dcaf34dc9749addbf383b1920c

ARG TARGETARCH
# Same Playwright and TypeScript versions as e2e-playwright/package.json.
ARG PLAYWRIGHT_VERSION=1.64.1
ARG TYPESCRIPT_VERSION=6.0.3
# npm @colbymchenry/codegraph (bin `codegraph`; native per-platform package).
ARG CODEGRAPH_VERSION=1.6.2
# rtk-oss = the Apache-2.0 build of rtk (github.com/rtk-ai/rtk release assets
# `rtk-oss-*`). Checksums copied from that release's checksums.txt; the
# x86_64 one was also verified locally on 2026-11-01.
ARG RTK_OSS_VERSION=1.0.1
ARG RTK_OSS_SHA256_AMD64=a44e8ffc43981100b4ddf1710399a1649a6c874591aa6d0c0727fe6cbd983c41
ARG RTK_OSS_SHA256_ARM64=ee24a43dac9162de55fea8460b5efcd56d401ec28b5117c921bffa8a57bc136e

# Telemetry off for every tool in the image (also enforced per sandbox by
# SandboxSpec::validate, REQUIRED_ENV).
ENV CODEGRAPH_TELEMETRY=0 \
    DO_NOT_TRACK=1 \
    NPM_CONFIG_UPDATE_NOTIFIER=false \
    NPM_CONFIG_FUND=false \
    PLAYWRIGHT_BROWSERS_PATH=/ms-playwright \
    NEXT_TELEMETRY_DISABLED=1

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl git \
 && rm -rf /var/lib/apt/lists/*

RUN npm install -g \
      "@playwright/test@${PLAYWRIGHT_VERSION}" \
      "@colbymchenry/codegraph@${CODEGRAPH_VERSION}" \
      "typescript@${TYPESCRIPT_VERSION}" \
 && playwright install --with-deps chromium \
 && npm cache clean --force \
 && rm -rf /var/lib/apt/lists/* /root/.npm

RUN set -eu; \
    case "${TARGETARCH:-amd64}" in \
      amd64) asset=rtk-oss-x86_64-unknown-linux-musl.tar.gz; sum="${RTK_OSS_SHA256_AMD64}" ;; \
      arm64) asset=rtk-oss-aarch64-unknown-linux-gnu.tar.gz; sum="${RTK_OSS_SHA256_ARM64}" ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /tmp/rtk.tgz "https://github.com/rtk-ai/rtk/releases/download/v${RTK_OSS_VERSION}/${asset}"; \
    echo "${sum}  /tmp/rtk.tgz" | sha256sum -c -; \
    tar -xzf /tmp/rtk.tgz -C /usr/local/bin rtk; \
    rm /tmp/rtk.tgz; \
    rtk --version

# Mount points the adapter binds (sandbox.md, "Mounts by role").
RUN mkdir -p /workspace /task /codegraph /agents \
 && chown node:node /workspace /task

# The `node` user (uid 1000) of the base image; the adapter also passes --user.
USER node
WORKDIR /workspace
ENV HOME=/home/node

# Kept alive by the adapter (`sleep infinity`); agents run through `docker exec`.
CMD ["sleep", "infinity"]
