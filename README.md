# e2e-agentdev

An open-source, language-agnostic end-to-end testing platform. It tests any web app from the browser with [Playwright](https://playwright.dev), is configured per app through a single `target.toml`, and ships one CLI, `guard`, that runs exactly the same way on a developer machine and in CI.

The full design (in Spanish) is in [`docs/propuesta-arquitectura.md`](docs/propuesta-arquitectura.md).

**Using guard on your own app?** Start with [`docs/getting-started.md`](docs/getting-started.md): install, `guard init`, connect Jira or GitHub, write the first journeys, the local loop with a coding agent over MCP, and the GitHub workflow with a required `guard` check.

## Getting started

Install the prebuilt binary (GitHub releases for Linux, macOS and Windows; the checksum is verified):

```sh
curl -fsSLO https://raw.githubusercontent.com/CarlosBranty/journeygate/main/install/install.sh
sh install.sh                  # Windows: install/install.ps1; private repo: set GITHUB_TOKEN
```

Then run your app as usual and, from the root of its repository (one service or several, with or without Docker Compose):

```sh
guard init                     # detects services, ports and compose infra; asks who runs the app:
                               #   "I start it myself" -> only the URLs; "guard starts it" -> start commands
guard doctor                   # fix every error it lists
guard connect jira             # optional, once per machine: also github, gitlab; --list shows what is set
guard new journey checkout.pay-card --title "Customer pays by card" --smoke
guard up && guard status       # waits for your app (or starts it with runner guard), then shows what runs where
guard test --all               # every journey, headless
guard test                     # day to day: only the journeys your diff affects, plus smoke
```

The questions are styled like `create-next-app` on a terminal and plain lines in scripts; `--runner self|guard` skips the runner question (`--yes` alone means `guard`). Choose **I start it myself** for local work; choose **guard starts it** when `guard up` should manage the processes. CI always needs start commands: with runner `self` they are kept commented in `target.toml`, ready to uncomment ([getting-started, section 2.4](docs/getting-started.md#24-enable-the-start-commands-for-ci)).

`guard init --mcp claude-code --hook --ci github` also registers the MCP server, a pre-push hook and the GitHub workflow. Releases are cut by pushing a `v*` tag that matches the workspace version ([`release.yml`](.github/workflows/release.yml)).

## Phase 1 scope

Phase 1 delivers the deterministic core, with no AI and no external services:

- **`guard` CLI** (Rust): `init`, `doctor`, `up`, `down`, `test`, `affected`, `gate plan|shard|finalize`, `report`.
- **Journey catalog** (`e2e-playwright/catalog/journeys.json`) and tag convention (`@journey:<id>`, `@area:<area>`, `@smoke`, `@ticket:<KEY>`).
- **Affected-journey selection** from the git diff: `@smoke` ∪ `@ticket:<KEY>` ∪ journeys whose `paths` match changed files.
- **CI gate** in three steps: duration-balanced shards (LPT), headless blob runs, and a final verdict where a flaky smoke test blocks.
- **Playwright suite** (`e2e-playwright/`) with API login, namespaced seed data and a fixed, deterministic browser environment.
- **Reference app** (`examples/demo-shop/`): a zero-dependency Node server that implements the testability contract.

## Phases 2-3 scope

- **Impact engine**: `guard test --affected` / `guard affected` ask `POST /api/impact` on `guard serve` when `[server].url` is set (or Jev directly with `TYPESAFE_API_KEY`). No engine configured keeps the path rules; an engine failure runs every active journey (fail closed).
- **`guard gate finalize`**: re-runs 3/3 failures against the base commit in a second worktree (regression vs pre-existing), uploads the evidence bundle (screenshot, trace, video, HTML report) to S3/MinIO, writes `.guard/gate-report.json`, reports to the server (or Jira directly) and publishes the PR comment and `guard` status.
- **`guard serve`**: Jira/GitHub/GitLab webhooks, triage (Shadow Mode by default), `/api/*` for the CLI and a read-only dashboard.
- **`guard mcp`**: MCP stdio server for coding assistants (run and read tests, never edit or bypass the guard).
- **`guard repro <test-id> --pr <n>`**: reruns a remote failure locally with the same seed and compares fingerprints.

## Phase 4 scope (agent lane)

- **`guard agents check | sync | run <TICKET> | status | ab-report`**: `agents.toml`, skills rendering, running a lane-A ticket through QA → red-first → seal → Coder attempts → gate → branch `agent/<KEY>` (pushed as a draft PR / MR labelled `agent` when `[code_host]` and its token are set; a local branch with `--fake` or without a token), and the 4-arm token A/B report. Red-first and the gate run in check sandboxes (no mounts, no egress); `GUARD_UNSAFE_HOST_CHECKS=1` is a development-only escape hatch.
- **`guard serve`** works audited lane-A tickets in thresholds **active** mode only (never in Shadow Mode): a Postgres queue (`FOR UPDATE SKIP LOCKED`), host / provider slots, a daily USD cap, Docker sandboxes whose only egress is the model gateway, and a human approval on every agent PR (nothing auto-merges).
- **Memory**: facts are written only by the orchestrator (the lane, and `guard serve`'s gate-result / label-override writers); a staleness job marks facts `needs_review` when their files change on the default branch, and periodic reapers clean up expired sandboxes, leftover gateway keys and empty sandbox networks; agents read a byte-capped brief in `task.json` and the read-only `guard_history` MCP tool (a `guard_memory_reader` login). `guard memory export --engram <file>`.
- **`guard mcp`** adds `guard_history` and `guard_map` (token-budgeted repo map over the guard-impact graph).
- Runtimes: claude-code, codex, mini-swe-agent (admitted), copilot-cli, openhands (provisional), and a scripted `fake` for tests. All runtime and gateway behavior is verified only with fakes and recorded fixtures: no provider credentials were available, and no model was called.

### End-to-end dry run (live Docker + Postgres, fakes for every model)

```sh
GUARD_DOCKER_TESTS=1 \
GUARD_SANDBOX_TEST_IMAGE=<repo>@sha256:<digest of deploy/sandbox/Dockerfile> \
DATABASE_URL=postgres://<user>:<password>@localhost:55432/postgres \
  cargo test -p guard-cli --test agent_lane_e2e -- --include-ignored --nocapture
```

It starts `guard serve --fake` on a copy of examples/demo-shop, a fake gateway container (`deploy/litellm/fake-gateway.mjs`), sends a Jira webhook, and checks the sealed spec, the rejected and the accepted Coder attempts, egress, the PR branch, `agent_runs`, `spend_records`, memory facts and `guard_history`.

## Repository layout

| Path | What |
| --- | --- |
| `guard-core/`, `guard-cli/` | Rust workspace members: pure domain rules and ports, and the `guard` binary. |
| `guard-jev/`, `guard-impact/`, `guard-tracker/`, `guard-codehost/`, `guard-store/` | Adapters: Jev, tree-sitter import graph, Jira, GitHub/GitLab, Postgres + S3. |
| `guard-server/`, `guard-mcp/` | `guard serve` (axum + dashboard) and `guard mcp` (stdio). |
| `guard-agents/`, `guard-sandbox/`, `guard-runtimes/`, `guard-map/` | Phase 4: queue, scheduler, pipeline and A/B harness; Docker sandbox and egress; runtime adapters and the LiteLLM client; the `guard_map` repo map. |
| `agents/`, `deploy/sandbox/`, `deploy/litellm/` | Skill sources, the sandbox image, gateway config and the test-only fake gateway. |
| `e2e-playwright/` | Playwright project, catalog and tests. |
| `templates/` | The scaffold `guard init` embeds: the generic `e2e-playwright/` and the GitHub workflow. |
| `install/` | `install.sh` and `install.ps1`, the release installers. |
| `examples/demo-shop/` | Reference app under test. |
| `schemas/` | JSON Schemas for `target.toml` and `journeys.json`, with contract tests. |
| `docs/contracts/` | Source of truth for every shared interface. |

## Contracts

| Contract | Defines |
| --- | --- |
| [`target-toml.md`](docs/contracts/target-toml.md) | The `target.toml` schema, required fields and defaults. |
| [`journeys-catalog.md`](docs/contracts/journeys-catalog.md) | The catalog format and the tag convention. |
| [`runner-env.md`](docs/contracts/runner-env.md) | Environment, files and exit codes between `guard` and Playwright. |
| [`cli.md`](docs/contracts/cli.md) | Every Phase 1 command, flag, exit code and output format. |
| [`detection.md`](docs/contracts/detection.md) | What `guard init` detects: services, ports, start commands and compose infrastructure. |
| [`demo-app.md`](docs/contracts/demo-app.md) | Routes, selectors and data of the reference app. |
| [`ports.md`](docs/contracts/ports.md) | Ports, crate ownership and the Phase 2 changes to Phase 1 commands. |
| [`finalize-phase2.md`](docs/contracts/finalize-phase2.md) | Baseline, evidence and reporting of `guard gate finalize`. |
| [`server-api.md`](docs/contracts/server-api.md), [`mcp-tools.md`](docs/contracts/mcp-tools.md) | `guard serve` HTTP API and `guard mcp` tools. |

## Developing this repository

Requirements: Rust 1.88 (pinned by `rust-toolchain.toml`), Node 24, pnpm 9; Docker only for `--docker`.

```sh
pnpm install                                        # Playwright suite and demo app
pnpm --filter e2e-playwright exec playwright install chromium
cargo build --release                               # builds target/release/guard

guard doctor                                        # validate target.toml, catalog and tooling
guard up                                            # start the demo app and wait for /healthz
guard test                                          # affected journeys + smoke, headless
guard test --headed --journey checkout.customer-card
guard test --all                                    # every active journey
guard report                                        # open the HTML report
guard down
```

Exit codes are the same for every command: `0` ok, `1` tests failed or verdict red, `2` usage or config error, `3` environment error.

### `guard serve`

```sh
export GUARD_SERVER_TOKEN=...              # required: bearer token of /api/*
export DATABASE_URL=postgres://...         # decision log + gate history (memory:// = in-memory)
export TYPESAFE_API_KEY=...                # Jev; without it triage goes to the human lane
export JIRA_BASE_URL=... JIRA_EMAIL=... JIRA_API_TOKEN=... GUARD_JIRA_WEBHOOK_SECRET=...
export GUARD_STORAGE_URL='s3+https://<key>:<secret>@<host>/<bucket>'   # evidence
guard serve --listen 0.0.0.0:8080          # dashboard at /, health at /healthz
```

Every variable is listed in [`server-api.md`](docs/contracts/server-api.md). For a local demo without any account, fake mode wires a deterministic engine, an in-memory Jira (issues come from the webhook payloads) and an in-memory store:

```sh
GUARD_SERVER_TOKEN=dev GUARD_JIRA_WEBHOOK_SECRET=hook guard serve --fake --listen 127.0.0.1:8099
curl -X POST -H 'Authorization: Bearer hook' -H 'Content-Type: application/json'   --data-binary @guard-cli/tests/fixtures/jira_issue_created.json http://127.0.0.1:8099/webhooks/jira
open http://127.0.0.1:8099/decisions        # the Shadow Mode decision for SHOP-7
```

Point the CLI at it with `[server] url = "http://127.0.0.1:8099"` in `target.toml` and the same `GUARD_SERVER_TOKEN`; `guard gate finalize` then posts its gate report and `/guards` shows it.

To run it in containers (guard + Postgres + S3 evidence store, optional LiteLLM gateway and agent lane), see [`deploy/README.md`](deploy/README.md).

### `guard mcp`

Register the binary as a stdio MCP server in the assistant, run from the repository root:

```json
{"mcpServers": {"guard": {"command": "guard", "args": ["mcp"]}}}
```

Tools: `guard_affected`, `guard_test`, `guard_failure`, `guard_repro`, `guard_catalog` ([`mcp-tools.md`](docs/contracts/mcp-tools.md)).

### `guard repro`

```sh
git checkout <the PR head>
guard up
guard repro 'chromium::tests/journeys/checkout/customer-card.spec.ts::Checkout: customer pays by card > creates exactly one confirmed order' --pr 12
```

Needs `[server]` and `GUARD_SERVER_TOKEN`. It downloads the screenshot and trace into `.guard/repro/<fingerprint>/`, runs the test (inside the Playwright image when Docker is available), and exits `0` when the failure reproduces with the same fingerprint, `1` otherwise.

### `guard explain`

Summarises the last run's failing and flaky journeys with a model. Only the failure fields of the Playwright report (journey id, title, file, line, status, error message) are sent.

```sh
export GUARD_EXPLAIN_API_KEY=...           # guard's own key variable
guard explain                              # every failing/flaky journey, summary in Spanish
guard explain --journey checkout.customer-card --lang en --json
```

| Variable | Default | Purpose |
|---|---|---|
| `GUARD_EXPLAIN_API_KEY` | - | API key. Falls back to `ANTHROPIC_API_KEY` when unset. Prefer the guard variable: a user-level `ANTHROPIC_API_KEY` is also read by Claude Code and switches it to API billing. |
| `GUARD_EXPLAIN_BASE_URL` | `https://api.anthropic.com` | Any Anthropic-compatible Messages API (`POST {base}/v1/messages`). |
| `GUARD_EXPLAIN_MODEL` | `claude-haiku-5-5` | Model id; `--model` wins. |
| `GUARD_EXPLAIN_LANG` | `es` | Summary language: `es`, `en` or any other code; `--lang` wins. Journey ids, paths, identifiers and error text stay verbatim. |

`guard-gui` sets these from **Settings > AI settings** (provider preset, base URL, model, language) and keeps one key per endpoint host in the Windows Credential Manager; it passes them to the `guard explain` child through its environment only, never on the command line. A saved key is only sent to the host it was saved for, and an inherited `GUARD_EXPLAIN_API_KEY` / `ANTHROPIC_API_KEY` only to `api.anthropic.com`: for any other host the GUI blanks both.

Run the schema contract tests:

```sh
node --test "schemas/*.test.mjs"
```

## License

[Apache-2.0](LICENSE).
