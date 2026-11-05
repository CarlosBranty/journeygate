# Getting started: guard on your own app

This guide takes an existing app repository, with one service or several, from zero to two working loops:

- **Local**: you (or your coding agent) change code, and `guard test` runs the affected Playwright journeys against your app on your machine.
- **Remote**: every pull request on GitHub runs the full suite through `guard gate`, and a required `guard` check blocks the merge when a journey breaks.

It describes what the code does today. Where the design in [`propuesta-arquitectura.md`](propuesta-arquitectura.md) promises more than is built, the guide says so.

## Quick path

```sh
sh install.sh                          # 1. install guard (install.ps1 on Windows)
                                       #    run your app as usual (bun run dev, docker compose up, ...)
guard init --mcp claude-code --hook    # 2. choose "I start it myself"; writes target.toml,
                                       #    e2e-playwright/, .mcp.json and the hook
guard connect jira --project SHOP      # 3. optional: Jira, GitHub or GitLab, once per machine
guard new journey checkout.pay-card --title "Customer pays by card" --smoke   # 4. first journey
guard up && guard status               # 5. waits for your app and shows what runs where
guard test --all                       #    run the suite
git push                               # 7. open a PR: the guard workflow runs and publishes the check
```

CI has to start the app itself, so step 7 needs the start commands that `init` kept commented in `target.toml` ([section 2.4](#24-enable-the-start-commands-for-ci)).

| Step | You do | Done when |
| --- | --- | --- |
| 1 | [Install `guard`](#1-install-guard) | `guard --version` prints a version |
| 2 | [Run `guard init`](#2-guard-init) and [add test-only routes](#23-make-your-app-testable) | `guard doctor` shows `0 errors` |
| 3 | [Connect Jira, GitHub or GitLab](#3-connect-your-services) | `guard connect --list` shows the keys |
| 4 | [Write your first journeys](#4-write-your-first-journeys) | `guard test --all` is green |
| 5 | [Run it: `guard up`, `test`, `status`](#5-run-it-guard-up-test-status) | `guard status` shows every entry ready |
| 6 | [Work locally with your agent](#6-the-local-loop-with-a-coding-agent) | the agent runs `guard_test` through MCP |
| 7 | [Set up GitHub](#7-run-the-guard-on-github) | the `guard` check shows up on a PR and is required |

## Terms

| Term | Meaning |
| --- | --- |
| Journey | One business flow a user cares about, such as "a customer pays by card and gets one order". One spec file per journey. |
| Catalog | `e2e-playwright/catalog/journeys.json`. The list of journeys and the file globs that route a change to each one. |
| Area | A part of the app (`[areas.<key>]` in `target.toml`) with the globs of the code that belongs to it. |
| Service | One process of your app, such as a backend and a frontend. With runner `guard`, `guard up` starts each one (`[[services]]` in `target.toml`). |
| Runner | Who starts the app locally: `self` (you) or `guard`. Chosen by the first question of `guard init` (section 2.1). |
| Smoke | Cheap journeys tagged `@smoke`. They run on every change. |
| Affected | `@smoke`, plus the ticket tag from the branch name, plus every journey whose `paths` match a changed file. |

---

## 1. Install guard

Prebuilt binaries are published as GitHub releases by [`.github/workflows/release.yml`](../.github/workflows/release.yml) on every `v*` tag, for Linux (x86_64, arm64), macOS (Intel, Apple silicon) and Windows (x86_64). Each release asset is `guard-<target>.tar.gz` with a `guard-<target>.tar.gz.sha256` next to it; both installers verify that checksum before they extract anything.

Linux, macOS, or Git Bash on Windows:

```sh
curl -fsSLO https://raw.githubusercontent.com/CarlosBranty/journeygate/main/install/install.sh
sh install.sh                          # installs to ~/.local/bin
```

Windows PowerShell:

```powershell
Invoke-WebRequest -UseBasicParsing https://raw.githubusercontent.com/CarlosBranty/journeygate/main/install/install.ps1 -OutFile install.ps1
powershell -ExecutionPolicy Bypass -File install.ps1   # installs to %LOCALAPPDATA%\guard\bin
```

| Variable | Default | Meaning |
| --- | --- | --- |
| `GUARD_VERSION` | `latest` | Release to install, `0.1.0` or `v0.1.0`. |
| `GUARD_REPO` | `CarlosBranty/journeygate` | `owner/name` of the repository with the releases. |
| `GUARD_INSTALL_DIR` | `~/.local/bin`, `%LOCALAPPDATA%\guard\bin` | Where the binary goes. |
| `GITHUB_TOKEN` or `GH_TOKEN` | unset | Sent as an `Authorization` header, never printed. Required while the repository is private. |

Both installers print a hint when the install directory is not on your `PATH`.

**While the guard repository is private**, the download links need a token with read access to it. With the GitHub CLI:

```sh
gh api repos/CarlosBranty/journeygate/contents/install/install.sh -H "Accept: application/vnd.github.raw" > install.sh
GITHUB_TOKEN="$(gh auth token)" sh install.sh
```

With a token the installers resolve the assets through the REST API (`/repos/<repo>/releases/latest` or `/releases/tags/v<version>`), because github.com download links of a private repository need a browser session.

**From source** (before the first release, or to run unreleased code). Requirements: Rust 1.88 (`rust-toolchain.toml` pins it; `rustup` installs it on first build) and Git:

```sh
git clone <url of this repository> e2e-agentdev
cd e2e-agentdev
cargo build --release -p guard-cli --bin guard
```

Then put `target/release/` on your `PATH`, or copy `target/release/guard` (`guard.exe` on Windows) into a directory that is already on it.

Every way ends with:

```sh
guard --version
```

Your app also needs Node 24 for the Playwright project, and Docker when its infrastructure runs with Docker Compose.

## 2. guard init

### 2.1 Detect and write

From the root of your app repository (a git repository is best; a plain directory works too, but `--affected`, the hook and the ticket pattern need git):

```sh
guard init                    # asks who runs the app, then about each detected value
guard init --runner self      # you start the app; guard only asks for the URLs
guard init --dry-run          # only prints the plan and the target.toml it would write
guard init --yes --install    # accepts everything (runner guard), then installs the Playwright dependencies
```

It detects how your app runs ([`contracts/detection.md`](contracts/detection.md)), without network and without reading secrets (the only dotenv file it reads is `.env.example`):

| Detected | From |
| --- | --- |
| Services | The repo root and directories up to two levels deep that hold a manifest (`package.json`, `composer.json`, `*.csproj`, `pyproject.toml`, `go.mod`, `Cargo.toml`, ...). |
| Framework and package manager | Dependencies and lockfiles (`bun.lock` > `pnpm-lock.yaml` > `yarn.lock` > `package-lock.json`; a conflict is reported). |
| Start command, port, ready path | The `dev` script, `php artisan serve`, `dotnet run`, ...; ports from script flags, `.env.example`, `launchSettings.json`, a literal in the entry file, else the framework default; `/health` when the backend has such a route. |
| Infrastructure | Compose services with infra images (Postgres, Redis, Mailpit, ...) become one `docker compose ... up -d --wait <services>` in `[app].start`. Production compose files (`prod`, `production`, `dokploy`, `deploy`, `staging` in the name) are ignored. |
| The rest | `base_url` and `ready_path` (the first frontend), languages, the code host from the `origin` remote, a Jira ticket pattern from branch names, one area per top-level directory. |

Every guess is printed with its evidence (`backend/src/config/env.ts:9 PORT || '3001'`) and a confidence. Then you choose who runs the app and confirm each value: Enter accepts it, typed text replaces it, `-` drops an optional item.

**Who runs the app.** The first question:

| Choose | When | Asks | `target.toml` |
| --- | --- | --- | --- |
| `I start it myself (guard only needs the URLs)` | Local work: you already run `bun run dev`, `docker compose up` or your IDE. | `base_url`, `ready_path`, the URL of each service | No start commands; the detected ones are kept as comments (section 2.4). `guard up` only waits for your app, `guard down` stops nothing. |
| `guard starts it (start commands, also needed for CI)` (default) | You want `guard up` to start and stop everything, or this file goes straight to CI. | start/stop command, each service's start and ready URL, `base_url`, `ready_path`, ticket project | `[app].start` and `[[services]]`, as in the example below. |

On a terminal the questions look like `create-next-app`: arrow keys to pick, the detected value already filled in, spinners while it works. Ctrl+C writes nothing (exit `130`). For example, in a repo with a backend, a frontend and Compose infrastructure:

```text
┌  guard init
│
◇  Detected your app
│
◇  plan ─────────────────────────────────────────────────────────────╮
│  infra      docker compose -f docker-compose.yml up -d --wait ...  │
│  service    backend  cwd=backend  elysia/bun  start="bun run dev"  │
│  service    frontend  cwd=frontend  next/bun  start="bun run dev"  │
│  base_url   http://localhost:3000   ready_path /                   │
│  areas      backend, frontend (edit their descriptions)            │
├────────────────────────────────────────────────────────────────────╯
│
◆  How will you run the app locally?
│  ● I start it myself (guard only needs the URLs)
│  ○ guard starts it (start commands, also needed for CI)
│
◇  base_url  (Enter keeps)
│  http://localhost:3000
◇  ready_path  (Enter keeps)
│  /
◇  service backend url  (Enter keeps, - drops it)
│  http://localhost:3001/health
◇  service frontend url  (Enter keeps, - drops it)
│  http://localhost:3000/
│
◇  Write target.toml and the Playwright scaffold?
│  Yes
│
◇  Files written
│  created target.toml
│  created e2e-playwright/ (20 files)
│
◇  next steps ─────────────────────────────────────────────────────────────────────╮
│  1. review target.toml (area descriptions start with TODO)                       │
│  2. cd e2e-playwright && npm install && npx playwright install chromium   (...)  │
│  3. start your app, then guard up && guard status   (guard up only waits for it) │
│  4. guard test --all                                                             │
│  5. guard new journey <area>.<name> --title "..."                                │
├──────────────────────────────────────────────────────────────────────────────────╯
│
└  guard is set up
```

The exact screen depends on your terminal. Scripts, CI, `--yes`, `NO_COLOR` and piped stdin get the plain line prompts (`<label> [<value>]: `) instead; they never ask the runner question and use runner `guard`, so pass `--runner self` there.

Then `guard init` writes:

| Path | What |
| --- | --- |
| `target.toml` | Validated before it is written. One service at the root keeps the single `[app]` shape; otherwise `[app].start` holds the compose infrastructure and each service is a `[[services]]` entry. Runner `self`: neither, the detected commands are comments. |
| `e2e-playwright/` | The generic Playwright project (section 2.2). An existing `e2e-playwright/` is never touched. |
| `.mcp.json` and friends | With `--mcp claude-code,codex,copilot-cli`: the `guard mcp` registration for each runtime. |
| `.git/hooks/pre-push` | With `--hook`: runs `guard test` before every push (`git push --no-verify` skips it). |
| `.github/workflows/guard.yml` or `.gitlab-ci.yml` | With `--ci github` or `--ci gitlab` (section 7). |

Nothing that exists is overwritten without `--force` (and `e2e-playwright/` not even then). All flags: [`contracts/cli.md`](contracts/cli.md#guard-init).

A multi-service result with runner `guard` looks like this (a Bun backend, a Next.js frontend and Compose infrastructure; the runner `self` version is in section 2.4):

```toml
version = 1

[app]
name = "saunas"
languages = ["typescript"]
start = "docker compose -f docker-compose.yml up -d --wait postgres redis"
stop = "docker compose -f docker-compose.yml down"
base_url = "http://localhost:3000"
ready_path = "/"

[[services]]
name = "backend"
cwd = "backend"
start = "bun run dev"
ready_url = "http://localhost:3001/health"

[[services]]
name = "frontend"
cwd = "frontend"
start = "bun run dev"
ready_url = "http://localhost:3000/"

[testability]
roles = ["customer", "admin"]          # one API login per role; [] = no login

[areas.backend]
description = "TODO: describe backend"
paths = ["backend/**"]

[areas.frontend]
description = "TODO: describe frontend"
paths = ["frontend/**"]
```

Then review it:

- Replace every `TODO: describe ...` description; `guard doctor` warns while one is left. Split areas further when one directory holds several business areas (`checkout`, `account`, ...).
- Set `[testability].roles` to the roles your test-only login knows.
- A glob that matches no tracked file protects nothing; `guard doctor` warns about it.
- The full reference, with every key and default, is [`contracts/target-toml.md`](contracts/target-toml.md).

```sh
guard doctor     # 0 errors is the goal; warnings tell you what is still missing
```

### 2.2 The Playwright project

`guard init` writes a generic Playwright project to `e2e-playwright/` (from `templates/e2e-playwright/` of the guard repository, embedded in the binary):

| Path | Why |
| --- | --- |
| `playwright.config.ts`, `src/config/` | The only reader of the runner environment (`E2E_TAGS`, `TARGET_BASE_URL`, shards, reporters). |
| `src/fixtures/` | Seeded `Math.random`, blocked third-party domains, per-test data namespace, `authStatePath`, and a `data` fixture that posts `{ ...input, namespace }` to `GUARD_SEED_PATH` and deletes the namespace after the test. |
| `tests/setup/auth.setup.ts` | API login per role at `GUARD_LOGIN_PATH` into `.auth/<role>.json`. |
| `tests/smoke/home.spec.ts` | First smoke test: the base URL answers below 400 and the page has a `<title>`. |
| `scripts/catalog-lint.ts`, `eslint.config.mjs`, `tsconfig.json` | Keep tags and catalog 1:1, and ban raw locators and arbitrary waits. |
| `AGENTS.md` | The rules your coding agent must follow when it writes tests. |
| `catalog/journeys.json` | The empty journey catalog. |

Add Page Objects under `src/pages/` and flows under `src/flows/` as you write journeys. The reference project of the guard repository (`e2e-playwright/`, written for `examples/demo-shop`) shows a complete suite.

Install it as a standalone npm project (or run `guard init --install`). A committed `package-lock.json` in `e2e-playwright/` is also what CI installs from.

```sh
cd e2e-playwright
npm install
npx playwright install chromium          # on Linux CI: --with-deps chromium
npx playwright test --list               # proves the suite parses
cd ..
```

The scaffold ships its own `.gitignore`. Also ignore guard's local state at the repo root:

```gitignore
.guard/
```

### 2.3 Make your app testable

guard does not log in through the UI or create data with clicks. Your app exposes a few **test-only routes** that exist only when a test token is configured. The reference implementation is [`examples/demo-shop`](../examples/demo-shop/), and its routes are specified in [`contracts/demo-app.md`](contracts/demo-app.md).

| Route | Required | What it does |
| --- | --- | --- |
| `[app].ready_path` (and each service's `ready_url`) | yes | Answers `200` when the app is ready. `guard up` polls it. |
| `POST /__test__/login` | if you have roles | Body `{"role": "<role>"}`. Answers `200` and sets your app's normal session cookie for a test user of that role. |
| `POST /__test__/seed` | when tests need data | Body `{"namespace": "<ns>", ...}`. Creates the records a test needs, all tagged with that namespace. |
| `DELETE /__test__/seed/:namespace` | when you seed | Deletes everything of that namespace. Always `204`. |

Rules that every test-only route follows:

- It requires the header `x-test-token: <token>`. A missing or wrong token answers `401`.
- It is registered **only** when the token is configured (for example an `E2E_TEST_TOKEN` setting). In production the token is unset and the routes do not exist.
- Other paths are fine: set `[testability].login_path` and `seed_path` in `target.toml`. guard exports them to Playwright as `GUARD_LOGIN_PATH` and `GUARD_SEED_PATH` ([`contracts/runner-env.md`](contracts/runner-env.md)), and the scaffold reads those.

Sketch for an ASP.NET Core minimal API (the same idea works as an Elysia or Express plugin, or a Laravel or Symfony route group behind middleware):

```csharp
// Program.cs: test-only routes, registered only when a token is configured.
if (app.Configuration["E2E_TEST_TOKEN"] is { Length: > 0 } testToken)
{
    var test = app.MapGroup("/__test__").AddEndpointFilter(async (ctx, next) =>
        ctx.HttpContext.Request.Headers["x-test-token"] == testToken
            ? await next(ctx)
            : Results.Json(new { error = "invalid test token" }, statusCode: 401));

    test.MapPost("/login", async (LoginRequest body, HttpContext http) =>
    {
        // Find or create the test user for body.Role, then sign in with your
        // normal scheme (for example HttpContext.SignInAsync) so the response
        // sets the same cookie a real login sets.
        return Results.Ok(new { role = body.Role });
    });

    // MapPost("/seed", ...) and MapDelete("/seed/{ns}", ...) for your own entities.
}
```

The token value is a secret. Keep it in an environment variable, never in a file you commit. By default guard reads it from `GUARD_TEST_TOKEN` and passes it to Playwright, so start your app with the same value:

```sh
export GUARD_TEST_TOKEN=dev-only-token
# Your app reads it under its own name, e.g. E2E_TEST_TOKEN=$GUARD_TEST_TOKEN
```

### 2.4 Enable the start commands for CI

With runner `self`, `target.toml` has no `[app].start` and no `[[services]]`. The detected commands sit right after the last `[app]` key as a commented block:

```toml
[app]
name = "saunas"
languages = ["php", "typescript"]
base_url = "http://localhost:3000"
ready_path = "/"

# guard does not start the app: you run it yourself (guard init --runner self).
# Detected commands, uncomment to let guard start it (needed in CI):
# start = "docker compose -f docker-compose.yml up -d --wait postgres redis mailpit"
# stop = "docker compose -f docker-compose.yml down"
#
# [[services]]
# name = "backend"
# cwd = "backend"
# start = "bun run dev"
# ready_url = "http://localhost:3001/health"
#
# [[services]]
# name = "frontend"
# cwd = "frontend"
# start = "bun run dev"
# ready_url = "http://localhost:3000/"
```

CI runners have nobody to start the app, so `guard gate` needs those commands. To enable them, remove the leading `# ` from every line below the two header lines (leave the header lines as comments) and run `guard doctor`. No line has to move: `start`/`stop` land in `[app]` and the `[[services]]` tables come before `[areas]`. From then on `guard up` also starts the app on your machine; keep running it yourself if you prefer, since `guard up` leaves an app that is already ready alone.

While the block stays commented, `guard up` and `guard test` print these commands as a hint when your app is not reachable. `guard init --ci github --runner self` writes the workflow anyway and warns that CI needs the block enabled. Full rules: [`contracts/target-toml.md`](contracts/target-toml.md#self-mode-runner-self).

## 3. Connect your services

Optional, once per machine. guard reports failures to Jira and talks to GitHub or GitLab with your credentials:

```sh
guard connect jira --project SHOP     # asks for base URL, email and API token
guard connect github                  # asks for a token (GitHub Enterprise: --api-url)
guard connect gitlab                  # asks for a token (self-managed: --api-url)
guard connect --list                  # what is configured, secrets masked
```

- The token is read without echo. guard checks it against the real API (`GET /rest/api/3/myself` for Jira, `GET /user` for GitHub and GitLab) and stores nothing when the check fails.
- Secrets go to your user credentials file, outside every repository: `%APPDATA%\guard\credentials.toml` on Windows, `$XDG_CONFIG_HOME/guard/credentials.toml` or `~/.config/guard/credentials.toml` elsewhere (mode `0600` on Unix).
- `target.toml` only gets the non-secret part: `[tracker]` with `kind` and `project`, or `[code_host]` with `kind`.
- Variables set in the environment always win over the file, so CI secrets are unaffected.
- Non-interactive (scripts): pass the fields as flags or environment variables and the token through its variable (`JIRA_API_TOKEN`, `GITHUB_TOKEN`, `GITLAB_TOKEN`) or `--token-stdin`. No flag takes a secret on the command line.

Details: [`cli.md`](contracts/cli.md#guard-connect).

## 4. Write your first journeys

Start with 3 to 5 journeys: the flows that cost money or users when they break (sign-in, checkout, the main form).

```sh
guard new journey checkout.customer-card \
  --title "Customer pays by card and gets one confirmed order" \
  --path "src/Checkout/**" --path "web/src/checkout/**" --smoke
```

It checks the id and that the area (the first segment of the id, or `--area`) exists in `target.toml`, refuses duplicates, and then:

1. **Appends the catalog entry** to `e2e-playwright/catalog/journeys.json`:

   ```json
   {
     "id": "checkout.customer-card",
     "area": "checkout",
     "title": "Customer pays by card and gets one confirmed order",
     "impact_question": "This diff can directly or indirectly change how customer pays by card and gets one confirmed order",
     "paths": ["src/Checkout/**", "web/src/checkout/**"],
     "smoke": true,
     "quarantined": false
   }
   ```

   Rewrite `impact_question` into a full sentence when the generated one reads poorly; Jev reads it to decide whether a diff touches the journey.

2. **Writes the spec skeleton** at `tests/journeys/<area>/<id without the area>.spec.ts`, with exactly the catalog tags and one `test.fixme`. Replace it with the real test:

   ```ts
   // `data` is the scaffold's seed fixture; `checkout` is your own Page Object fixture.
   import { authStatePath, expect, test } from '../../../src/fixtures';

   test.use({ storageState: authStatePath('customer') });

   test.describe(
     'Customer pays by card and gets one confirmed order',
     { tag: ['@journey:checkout.customer-card', '@area:checkout', '@smoke'] },
     () => {
       test('creates exactly one confirmed order', async ({ data, checkout }) => {
         const { product } = await data.seed<{ product: { id: string } }>({
           product: { name: 'Blue mug', priceCents: 1999 },
         });
         await checkout.payWithCard(product, 'approved');
         await expect(checkout.confirmation).toBeVisible();
       });
     },
   );
   ```

`--record` also opens `npx playwright codegen <base_url>` so you can click through the flow and copy the generated steps into Page Objects. Nothing is ever overwritten. Details: [`cli.md`](contracts/cli.md#guard-new-journey).

Check it:

```sh
cd e2e-playwright
node scripts/catalog-lint.ts --target ../target.toml   # tags and catalog match 1:1
npm run lint && npm run typecheck
cd ..
guard up
guard test --journey checkout.customer-card --headed
```

The rules that keep the suite stable are in `e2e-playwright/AGENTS.md`. The ones people break most:

| Rule | Instead |
| --- | --- |
| No CSS or XPath selectors | `getByRole`, `getByLabel`, `getByTestId`, and only inside `src/pages/` |
| No `waitForTimeout` or `networkidle` | Web-first assertions: `await expect(locator).toBeVisible()` |
| No `test.skip` / `test.only` | Set `"quarantined": true` in the catalog |
| No shared data between tests | Seed it per test with the `data` fixture |
| No UI login | `test.use({ storageState: authStatePath('<role>') })` |
| Assert the business outcome | "One confirmed order", not "the button was clicked" |

## 5. Run it: guard up, test, status

**Runner `self`** (you start the app):

```sh
bun run dev      # or however you run it: your terminal, your IDE, docker compose up
guard up         # never starts anything: waits until base_url + ready_path answers 200
guard status
```

```text
up: waiting for your app at http://localhost:3000/ (start it yourself; guard does not start it)
up: ready at http://localhost:3000/ in 4.1s
```

If the app never answers, `guard up` exits `3` after `--timeout` (default 120 s) and lists the commented commands from `target.toml` as a hint. `guard test` checks the same URL before Playwright starts and fails fast with that hint, instead of letting every test time out. `guard down` says `down: nothing to stop: guard did not start the app`; stop your app the way you started it.

**Runner `guard`** (guard starts the app):

```sh
guard up         # [app].start (infrastructure), then every [[services]] entry, waiting for each ready_url
guard status     # what runs where: PID, alive, ready, URL and log per entry
guard down       # stops the services in reverse order, then [app]
```

`guard up` starts each service detached in its own process group and appends its output to `.guard/services/<name>.log`; `[app]` logs to `.guard/app.log`. When any step fails or never gets ready, `guard up` exits `3` with the tail of that log and stops what it started. `guard status --json` gives the same table as JSON.

```text
NAME      CWD       PID   ALIVE  READY  URL                           LOG
saunas    .         4242  yes    yes    http://localhost:3000/        .guard/app.log
backend   backend   4250  yes    yes    http://localhost:3001/health  .guard/services/backend.log
frontend  frontend  4251  yes    yes    http://localhost:3000/        .guard/services/frontend.log
base_url: http://localhost:3000 ready
```

```sh
guard test --all                         # every active journey, headless
guard test                               # day to day: affected journeys + smoke
guard report                             # HTML report with traces of the last run
```

## 6. The local loop with a coding agent

### One-time setup

`guard init --mcp claude-code` (or `codex`, `copilot-cli`, comma-separated) registers `guard mcp` as a stdio MCP server. For another assistant, add it to its MCP config yourself:

| Assistant | Where | Entry |
| --- | --- | --- |
| Claude Code | `.mcp.json` at the repo root (written by `--mcp claude-code`) | `{"mcpServers": {"guard": {"command": "guard", "args": ["mcp"]}}}` |
| Codex | `~/.codex/config.toml` (`--mcp codex` renders `.guard/agents/mcp/codex.toml` to copy from) | `[mcp_servers.guard]` with `command = "guard"` and `args = ["mcp"]` |
| Others | their MCP config | command `guard`, args `["mcp"]` |

Point the agent at `e2e-playwright/AGENTS.md` (for example, reference it from your root `AGENTS.md` or `CLAUDE.md`), so it writes tests with the same rules.

### Every change

```sh
git switch -c SHOP-123-card-retry        # ticket key in the branch adds @ticket:SHOP-123
guard up                                 # runner self: start your app first, guard up only waits
```

1. **Ask the agent for the change.** It codes as usual.
2. **The agent checks itself through MCP.** Ask it in plain language, for example "run the affected guard tests and fix what fails". It uses:

   | Tool | Returns |
   | --- | --- |
   | `guard_affected` | Journeys selected by the current diff, and why |
   | `guard_test` | Structured result: passed, failed, exact line, screenshot path |
   | `guard_failure` | One failure in detail: error, steps, screenshot, trace |
   | `guard_catalog` | Journeys by area, tag or text |

   There is no tool to update snapshots, edit protected journeys or skip the guard.
3. **You verify** what the agent claims:

   ```sh
   guard affected            # what will run, and the reason for each journey
   guard test                # affected + smoke, headless
   guard test --headed       # watch it
   guard report              # HTML report with traces of the last run
   ```

4. **If the flow changed on purpose**, update the journey in the same PR. The healer rule in `AGENTS.md` applies: the agent may fix a test, never loosen its assertions.
5. **New feature, new journey**: `guard new journey` (section 4) in the same PR.
6. `guard down` when you finish (runner `guard`; with runner `self` stop your app yourself).

| Command | Use it for |
| --- | --- |
| `guard test --all` | Every active journey, before a large merge |
| `guard test --journey <id>` | One journey (quarantined ones too) |
| `guard test --ui` / `--debug` | Playwright UI mode or inspector |
| `guard test --docker` | Same browser image as CI |
| `guard test --affected --base develop` | Compare against another base branch |

Exit codes are the same everywhere: `0` ok, `1` tests failed, `2` config error, `3` environment error (app not up, Node missing).

## 7. Run the guard on GitHub

The CI job starts the app with `guard up`, so `target.toml` needs start commands: with runner `self`, enable the commented block first ([section 2.4](#24-enable-the-start-commands-for-ci)).

`guard init --ci github` writes the workflow of section 7.3 to `.github/workflows/guard.yml`. Pass it on the first `guard init`: once `target.toml` exists, `guard init` needs `--force`, which also re-renders `target.toml` from a fresh detection. To add CI later, either copy the workflow from section 7.3 by hand, or commit first, run `guard init --ci github --force` and restore your config with `git checkout -- target.toml`. `--action-ref <owner>/<repo>/ci/github-action@<ref>` changes the `uses:` line; the default is `CarlosBranty/journeygate/ci/github-action@main`. For GitLab, `guard init --ci gitlab` writes `.gitlab-ci.yml` from [`ci/gitlab/templates/gate.yml`](../ci/gitlab/templates/gate.yml).

### 7.1 Where the action lives

The workflow installs `guard` with the composite action of the guard repository, `ci/github-action`.

| `version:` input | What it does | When |
| --- | --- | --- |
| `source` (default) | Builds `guard` with cargo from the action's own repository on every job; a few minutes per job, so start with 2 shards. | The guard repository is private, or no release exists yet. |
| `0.1.0` (a release, without `v`) | Downloads `guard-<target>.tar.gz` of that release and verifies its `.sha256`, in seconds. | The guard repository is public and has that release. Add `with: { version: 0.1.0 }` to the three install steps. |

The release download sends no token, so it works only for a public guard repository; with a private one keep `version: source`.

- Push the guard repository to GitHub (the default is `CarlosBranty/journeygate`).
- If it is **private**, open its *Settings → Actions → General → Access* and allow access from your other repositories; otherwise the app repo cannot use the action.

### 7.1.1 While both repositories are private

| Topic | What changes |
| --- | --- |
| Using the action | In the guard repository: *Settings → Actions → General → Access* → accessible from repositories owned by the same user. Both repositories must belong to that user. Collaborators on the app repo can then read the action's workflow logs. |
| Installing locally | The installers need `GITHUB_TOKEN` or `GH_TOKEN` with read access to the guard repository (section 1). |
| Required `guard` check | Rulesets and branch protection on **private** repositories need GitHub Pro, Team or Enterprise. On GitHub Free the workflow and the `guard` check still run and show on every PR, but nothing enforces them: you merge only when `guard` is green. |
| Actions minutes | Private repositories consume the plan's monthly minutes. Each PR runs 4 jobs and, with `version: source`, each builds `guard`: measure one run and budget from it. |

### 7.2 Secrets in the app repository

*Settings → Secrets and variables → Actions → New repository secret*:

| Secret | Required | For |
| --- | --- | --- |
| `GUARD_TEST_TOKEN` | yes, if you have roles or seed data | Your app's test-only routes in CI. Use a different value from your local one. |
| `GUARD_STORAGE_URL` | no | Upload evidence (screenshots, traces) to S3-compatible storage: `s3+https://<key>:<secret>@<host>/<bucket>` |
| `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN` | no | Report failures to Jira (with `[tracker]` in `target.toml`) |

`GITHUB_TOKEN` is provided by Actions. The workflow below grants it the permissions to publish the check and the PR comment. Your local credentials file (section 3) is never used in CI.

Make sure the app CI starts reads the token. With Docker Compose, pass it through, for example `E2E_TEST_TOKEN: ${GUARD_TEST_TOKEN}` in the service's `environment`.

### 7.3 The workflow

This is the file `guard init --ci github` writes to `.github/workflows/guard.yml` (a unit test keeps the two identical). Here `<owner>/e2e-agentdev` stands for wherever the guard repository lives; pin `@main` to a tag or commit once you have one.

```yaml
name: guard
on:
  pull_request:

# plan and shard run code from the PR: they only read. Write permissions and
# the test token go to the jobs and steps that need them.
permissions:
  contents: read

jobs:
  plan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: <owner>/e2e-agentdev/ci/github-action@main
      - uses: actions/cache/restore@v4          # durations from earlier runs balance the shards
        with:
          path: .guard/durations.json
          key: guard-durations-${{ github.run_id }}
          restore-keys: guard-durations-
      - run: guard gate plan --shards 2 --out plan/
      - uses: actions/upload-artifact@v4
        with: { name: shard-plan, path: plan/ }

  shard:
    needs: plan
    runs-on: ubuntu-latest
    strategy:
      fail-fast: false
      matrix:
        index: [1, 2]
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 24 }
      - uses: <owner>/e2e-agentdev/ci/github-action@main
      - run: npm ci && npx playwright install --with-deps chromium
        working-directory: e2e-playwright
      - uses: actions/download-artifact@v4
        with: { name: shard-plan, path: plan/ }
      # Starts the app with [app].start, runs this shard headless, stops the app.
      - run: guard gate shard --index ${{ matrix.index }} --plan plan/
        env:
          GUARD_TEST_TOKEN: ${{ secrets.GUARD_TEST_TOKEN }}
      - uses: actions/upload-artifact@v4
        with:
          name: blob-${{ matrix.index }}
          path: e2e-playwright/blob-report/

  finalize:
    needs: shard
    if: always()
    runs-on: ubuntu-latest
    permissions:
      contents: read
      checks: write          # publishes the "guard" check run
      pull-requests: write   # comments on the PR
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }                # the baseline run needs the base commit
      - uses: actions/setup-node@v4
        with: { node-version: 24 }
      - uses: <owner>/e2e-agentdev/ci/github-action@main
      - run: npm ci && npx playwright install --with-deps chromium
        working-directory: e2e-playwright
      - uses: actions/download-artifact@v4
        with: { pattern: blob-*, path: all-blobs/, merge-multiple: true }
      # Merges the shards, re-runs 3/3 failures on the base commit, decides,
      # publishes the "guard" check and the PR comment.
      - run: guard gate finalize --blobs all-blobs/
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          GUARD_TEST_TOKEN: ${{ secrets.GUARD_TEST_TOKEN }}
          GUARD_STORAGE_URL: ${{ secrets.GUARD_STORAGE_URL }}
      - uses: actions/upload-artifact@v4        # the merged HTML report, to open locally
        if: always()
        with: { name: guard-report, path: e2e-playwright/playwright-report/ }
      - uses: actions/cache/save@v4
        if: always()
        with:
          path: .guard/durations.json
          key: guard-durations-${{ github.run_id }}
```

What each step decides:

| Job | Fails the workflow when | Never fails for |
| --- | --- | --- |
| `plan` | Bad config, no spec files | — |
| `shard` | The app never got ready, Playwright crashed, no blob | Red tests (they are decided later) |
| `finalize` | Verdict red: a failure that is a regression on the base commit, or a flaky `@smoke` test | Failures that also fail on the base commit (pre-existing) |

### 7.4 Make the check required

Open a PR once so the `guard` check exists, then in the app repository:

1. *Settings → Rules → Rulesets → New branch ruleset* (or *Settings → Branches → Add rule* on older plans), target `main`.
2. Enable **Require status checks to pass** and add **`guard`**. That is the check run `finalize` publishes, not the job names.
3. Enable **Require a pull request before merging**.

From then on a PR with a broken journey cannot be merged.

### 7.5 When the guard fails on a PR

1. Read the `guard` check and the PR comment: test, line and whether it is a regression.
2. Download the `guard-report` artifact and open it with `npx playwright show-report <dir>`; the trace shows every step.
3. Reproduce locally: `git switch <branch> && guard up && guard test --journey <id> --headed`.

`guard repro <test-id> --pr <n>` downloads the evidence and replays the failure with the same seed, but it needs `guard serve` (`[server]` in `target.toml`). Without a server, use the steps above.

## What is not there yet

| Missing | Effect for you | Workaround |
| --- | --- | --- |
| Authenticated release download in the GitHub action | With a private guard repository every CI job builds guard from source | `version: source` and few shards; make the repository public, or install with `install.sh` and a token |
| `guard init --ci` on an initialised repo | `--force` rewrites `target.toml` too | Commit first and restore `target.toml`, or copy section 7.3 by hand |
| `guard init --mcp` writing Codex or Copilot config in your home directory | Only `.mcp.json` is wired directly | Copy the rendered file (section 6) |
| Jev impact, Jira, GitHub with real credentials in CI | Verified only against fakes so far | `--affected` falls back to path rules; reporting failures are warnings, never change the verdict |
| A `guard serve` deployment checked end to end with real Jira and code host credentials (`deploy/docker-compose.yml` runs it, see `deploy/README.md`) | Without a server: no dashboard, no `guard repro` | Run the CLI-only setup above; it is complete without a server |

## Checklist

- [ ] `guard --version` works from a new terminal.
- [ ] `guard doctor` shows `0 errors`.
- [ ] `guard up && guard status` shows every entry alive and ready.
- [ ] Test-only routes return `401` without the token and do not exist in production.
- [ ] `node scripts/catalog-lint.ts --target ../target.toml` passes.
- [ ] `guard test --all` is green locally.
- [ ] The assistant lists the `guard_*` MCP tools.
- [ ] With runner `self`: the commented start block in `target.toml` is enabled before CI runs.
- [ ] A PR shows the `guard` check, and the ruleset requires it.
