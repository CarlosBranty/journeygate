# Contract: `guard` CLI, Phase 1

The command surface of the `guard` binary (`guard-cli` crate) in Phase 1. Anything not listed here is not part of Phase 1 (`guard serve`, `guard repro`, `guard mcp`, `guard agents sync`, impact engine, Jira).

Related contracts: [`target-toml.md`](target-toml.md), [`journeys-catalog.md`](journeys-catalog.md), [`runner-env.md`](runner-env.md), [`detection.md`](detection.md) (what `guard init` detects). Source: sections 3, 8, 9, 10, 12 and 17 of `docs/propuesta-arquitectura.md`.

## Conventions

### Exit codes (every command)

| Code | Meaning | Examples |
| --- | --- | --- |
| `0` | OK | Tests passed, verdict green, command succeeded. |
| `1` | Tests failed / verdict red | At least one unexpected failure; a flaky smoke test in `gate finalize`. |
| `2` | Usage or config error | Unknown flag, conflicting flags, invalid `target.toml` or `journeys.json`, unknown journey id, unknown git ref, missing plan file. |
| `3` | Environment error | `node`/`npx`/`docker`/`git` missing, app never became ready, `[app].start` failed, Playwright crashed (see runner-env exit table), no blob reports. |
| `130` | Interrupted by the user | Ctrl+C at a styled `guard init` prompt (see [Prompts](#prompts)). |

When several apply, the lowest non-zero code that describes the first failure wins; `doctor` aggregates (see below).

### Output

- **stdout** carries the command's result (human text, or JSON with `--json`). Machine-readable output is only promised for `--json`.
- **stderr** carries diagnostics, one per line: `error: <message>`, `warning: <message>`, `hint: <message>`. Playwright's own output is streamed through unchanged.
- No colour when stdout is not a TTY or `NO_COLOR` is set.

### Global flags

| Flag | Default | Meaning |
| --- | --- | --- |
| `--config <path>` | `<repo root>/target.toml` | Repo root = `git rev-parse --show-toplevel`, else the current dir. |
| `--e2e-dir <path>` | `<repo root>/e2e-playwright` | Playwright project dir. The catalog is `<e2e-dir>/catalog/journeys.json`. |
| `-h, --help` / `-V, --version` | | Standard. |

### Local state (`<repo root>/.guard/`, git-ignored)

| File | Written by | Format |
| --- | --- | --- |
| `.guard/app.pid` | `guard up` | PID of the background `[app].start` process, decimal text. |
| `.guard/app.identity` | `guard up` | Start identity of that PID; `guard down` only terminates the PID while it still matches. |
| `.guard/app.log` | `guard up` | stdout+stderr of `[app].start`, appended. |
| `.guard/services/<name>.pid`, `.identity`, `.log` | `guard up` | The same three files per `[[services]]` entry. |
| `.guard/durations.json` | `guard gate finalize` | See `gate plan`. |
| `.guard/verdict.json` | `guard gate finalize` | See `gate finalize`. |

---

## `guard init`

```text
guard init [--yes] [--dry-run] [--force] [--runner self|guard] [--mcp <runtime,...>] [--hook] [--ci github|gitlab] [--action-ref <ref>] [--install]
```

Assisted setup. Runs [detection](detection.md) on the repo, shows the plan, asks who runs the app locally, lets the user confirm or change each item, then writes `target.toml` and scaffolds the Playwright project. Works at a root that is not a git repository (the repo root is then the current dir; see [Global flags](#global-flags)).

| Flag | Meaning |
| --- | --- |
| `--yes` | Accept every detected value without asking. Implied when stdin is not a TTY. |
| `--runner self\|guard` | Who starts the app locally (see [Runner](#runner)). `guard`: guard starts it (`[app].start`, `[[services]]`; also what CI needs). `self`: the developer starts it and guard only needs the URLs. Without the flag the styled prompter asks; with `--yes`, a non-TTY or the plain prompter it is `guard`, so every existing invocation writes the same `target.toml` as before. Any other value = exit `2` (usage error). |
| `--dry-run` | Print the plan and the `target.toml` that would be written, then exit `0`. Writes nothing, asks nothing, runs nothing. |
| `--force` | Overwrite `target.toml`, the hook and the CI file when they exist. Without it, an existing `target.toml` is exit `2` (before anything is detected or written) and an existing hook or CI file is kept with `note: <path> exists, kept (use --force to overwrite)`. An existing `e2e-playwright/` is never overwritten, even with `--force`. |
| `--mcp <runtime,...>` | Also write the `guard mcp` registration for these runtimes (comma-separated, names as in `agents.toml`: `claude-code`, `codex`, `copilot-cli`). Uses the same renderer as `guard agents sync` (only its MCP part): `claude-code` -> `.mcp.json` `mcpServers.guard` (other entries kept), `codex` -> `.guard/agents/mcp/codex.toml`, `copilot-cli` -> `.guard/agents/mcp/copilot-cli.json`. Prints `wrote <path>` per changed file and `kept <path> (unchanged)` otherwise. Unknown runtime = exit `2` before anything is written. |
| `--hook` | Write `pre-push` in the hooks dir (`git rev-parse --git-path hooks`, normally `.git/hooks/`) running `guard test` (executable on Unix). Only inside a git repository; elsewhere `warning: --hook needs a git repository; skipped`. |
| `--ci github` | Write `.github/workflows/guard.yml`: the plan / shard / finalize workflow of `docs/getting-started.md` section 7.3 (embedded from `templates/ci/github-guard.yml`, which a unit test keeps identical to the guide), with `uses: <action-ref>` in its three install steps. |
| `--ci gitlab` | Write `.gitlab-ci.yml` with the content of `ci/gitlab/templates/gate.yml` (embedded in the binary). |
| `--action-ref <ref>` | The `uses:` value for `--ci github`. Default `CarlosBranty/journeygate/ci/github-action@main`. |

`--ci` together with runner `self` still writes the CI file, and stderr gets `warning: CI needs start commands: guard does not start the app with --runner self; uncomment the detected commands in target.toml before CI runs` (after the `wrote`/`created` line of the CI file). The exit code is unchanged.
| `--install` | Run `npm install` and then `npx playwright install chromium` in `e2e-playwright/` (`GUARD_NPM` and `GUARD_NPX` override the two executables, as `GUARD_NPX` does for `guard test`). Without it, interactive mode asks (`Install dependencies now? [y/N]: `, default no) and non-interactive mode skips it. Failure = exit `3` (`error: <command> failed in e2e-playwright (exit <n>)`, or `error: cannot run <command>: ...`) after everything else was written; the second command is not run after the first failed. |

### Flow

1. **Detect** (no network, no writes): languages, code host, ticket pattern and areas exactly as before (table below), plus services, ports, start commands, compose infrastructure, `base_url` and `ready_path` from [`detection.md`](detection.md).
2. **Print the plan** to stdout: infrastructure (`[app].start`), services (name, cwd, framework/package manager, start, ready URL, confidence), `base_url` + `ready_path`, languages, code host, ticket pattern, areas, and every evidence string (indented under its line). Then `note:` lines: ignored compose files (`note: ignored <file> (<reason>)`), lockfile conflicts (the package manager evidence), services also available in compose (`note: <service> is also available in compose (<compose service>); using the native dev command`), skipped roots (`note: skipped <root>: <reason>`), the report's `notes` and files too large to read.
3. **Choose the runner** (see [Runner](#runner)): `--runner`, else the styled select, else `guard`.
4. **Confirm**. On an interactive TTY (stdin and stdout are terminals, no `--yes`), one prompt per item, through the [prompter](#prompts):
   - Runner `guard`, in this order: `start command` (`[app].start`), `stop command` (only when there is one), then per service `service <name> start` and `service <name> ready_url`, then `base_url`, `ready_path` and `ticket project` (only when detected).
   - Runner `self`, in this order: `base_url`, `ready_path`, then per detected service `service <name> url` (its `ready_url`, kept in the commented block, `-` drops it). Nothing else is asked: start/stop commands are not active config, and the ticket pattern is accepted as detected.

   Plain form: each prompt is `<label> [<value or none>]: `. Enter accepts the value, typed text replaces it, a single `-` drops an optional item: the start command (and with it the stop command), a stop command, a service (its `ready_url` prompt is then skipped), a `ready_url` (do not wait), the ticket pattern; `base_url` and `ready_path` cannot be dropped. Every answer is re-validated by rendering and parsing the whole `target.toml`; an invalid one prints `error: <reason>` and repeats the prompt. EOF on stdin = accept the rest. Non-TTY stdin or `--yes`: everything is accepted silently. The styled form asks the same items with the same rules (see [Prompts](#prompts)).
5. **Final confirmation** (styled prompter only): `Write target.toml and the Playwright scaffold?` (default yes). No = `init: nothing written`, exit `0`, nothing written.
6. **Validate** the resulting document with the parser `guard doctor` uses (`guard_core::config::parse_target`, which implements `schemas/target.schema.json` and the semantic rules of [`target-toml.md`](target-toml.md)). A plan that does not validate is a bug and exits `2` without writing.
7. **Write**, never overwriting without `--force`:

| Path | Content |
| --- | --- |
| `target.toml` | The confirmed plan, shaped by the `target_shape` rule of [`detection.md`](detection.md): exactly one service, at the root, and no compose infrastructure keeps the previous single-`[app]` shape (its start command is `[app].start`); otherwise `[app].start`/`[app].stop` hold the compose infrastructure (if any) and every service is a `[[services]]` entry. Runner `self`: no `[app].start`/`[app].stop` and no `[[services]]`; the same commands are written as the commented block of [`target-toml.md`](target-toml.md#self-mode-runner-self). |
| `e2e-playwright/` | The generic scaffold from `templates/e2e-playwright/` of the guard repository, embedded in the binary with `include_str!` (20 files): `.gitignore`, `.npmrc`, `package.json` (a subset of the reference `e2e-playwright/package.json` devDependencies, same pinned versions), `playwright.config.ts` (implements [`runner-env.md`](runner-env.md)), `tsconfig.json`, `eslint.config.mjs`, `AGENTS.md`, `src/config/env.ts`, `src/fixtures/` (seeded `Math.random`, blocked domains, namespace, `authStatePath`, and `data.ts`: a `data` fixture whose `seed(input)` posts `{...input, namespace}` to `GUARD_SEED_PATH` and, when the test seeded anything, deletes `<seed path>/<namespace>` after it), `tests/setup/auth.setup.ts` (uses `GUARD_LOGIN_PATH`), `tests/smoke/home.spec.ts` (`@smoke`: `GET /` answers `< 400` and the page has a non-empty `<title>`), `catalog/journeys.json` (`{"version": 1, "journeys": []}`), `scripts/catalog-lint.ts` with `scripts/lib/`. The runner-env files, the generic fixtures, the setup project and catalog-lint are byte-identical to the reference project (a unit test enforces it). If `e2e-playwright/` already exists it is left untouched (even with `--force`) and stdout gets `note: e2e-playwright/ exists, scaffold skipped`; only a missing `catalog/journeys.json` inside it is created, as before. |
| `.mcp.json` / `.guard/agents/mcp/*` | With `--mcp`. |
| `<hooks dir>/pre-push` | With `--hook`. Body: `#!/bin/sh` + `exec guard test`. |
| `.github/workflows/guard.yml` or `.gitlab-ci.yml` | With `--ci`. |

8. **Install** dependencies (with `--install`, or when the interactive prompt is answered `y`).
9. **Next steps** block on stdout.

### Runner

The first question of an interactive styled `init`, asked right after the plan:

| Option (as shown) | Value | Effect |
| --- | --- | --- |
| `I start it myself (guard only needs the URLs)` | `self` | Self-mode questions of step 4; `target.toml` without active start config, detected commands commented ([`target-toml.md`](target-toml.md#self-mode-runner-self)). `guard up` then only waits for the app, `guard down` stops nothing guard did not start. |
| `guard starts it (start commands, also needed for CI)` | `guard` | Today's behaviour: the per-item confirm flow and `target.toml` shape. |

The highlighted default is `guard`. `--runner` skips the question. The plain prompter never asks it (its prompts are exactly the previous ones, so scripted stdin keeps working); use `--runner self` there. With `--yes`, a non-TTY or no flag in plain mode the runner is `guard`, byte-identical to the previous `init`.

Runner `self` prints, after the `detected start command: none` line, `note: guard does not start the app (--runner self); detected commands are commented in target.toml`, and step 3 of `next steps` reads `3. start your app, then guard up && guard status   (guard up only waits for it)`.

### Prompts

All questions of `init` (runner, the confirm items, the final confirmation, `Install dependencies now?`) go through one prompter, chosen once per run:

| Prompter | Used when | Look |
| --- | --- | --- |
| Styled | stdin reads key presses (a TTY; on Windows a real console input buffer, so an msys pty such as plain mintty without ConPTY does not count), stdout **and** stderr are TTYs (the prompts are drawn and read on stderr, so `guard init 2>log` is plain), neither `--yes` nor `--dry-run` is given and `NO_COLOR` is not set (any value, even empty). | Clack style (`cliclack`): `intro guard init`; a spinner `Detecting your app` while detection runs; a note box `plan` with the same facts as the plain plan block (one line per item, evidence left out); the runner as an arrow-key select; every item as a text input whose label ends with `(Enter keeps)`, or `(Enter keeps, - drops it)` for an optional item, with the detected value shown as its `(default)` placeholder (Enter or the same value keeps it, `-` drops an optional item as in plain, other text replaces it); yes/no as a confirm with the default shown; a spinner `Writing files` while step 7 runs, the `created`/`wrote`/`note:` report lines shown as log lines once it stops; when a write fails the spinner ends as `Writing files failed` and the lines for the files already written are still shown before the `error:` line; `warning:` lines as warning log lines; a `next steps` note box and the outro `guard is set up`. Declining the final confirmation ends with the cancel outro `init: nothing written`. Colours as the terminal supports them. |
| Plain | Everything else: non-TTY stdin, stdout or stderr, an msys pty on Windows, `--yes`, `--dry-run`, `NO_COLOR`, and unit tests that drive `init` with fake input. | Exactly the previous line prompts (`<label> [<value or none>]: `, `Install dependencies now? [y/N]: `) and plain stdout lines; no escape sequences, no final confirmation. |

Rules shared by both: the same items in the same order, the same defaults, the same validation (an invalid answer shows `error: <reason>` and asks again), the same files written. Only the plain output is a stable text format; the styled screen is not (scripts must use `--yes`/`--runner` or a non-TTY).

A prompt that cannot read its answer (any error other than Ctrl+C/Esc) ends `init` with exit `3`, `error: cannot read the answer from the terminal: <reason>` and `hint: set NO_COLOR=1 or pass --yes to use the plain prompts`; nothing more is written.

**Ctrl+C** (or Esc) at a styled prompt cancels: the outro reads `init cancelled, nothing written`, nothing is written and the exit code is `130`. At the `Install dependencies now?` prompt the files are already written; cancelling there skips the install, says `init cancelled, dependencies not installed`, and exits `130`. EOF on stdin in the plain prompter is not a cancel (it accepts the rest, as before).

When the root is not a git repository, stderr gets `warning: not a git repository: --affected, the hook and the ticket pattern need git` (a diagnostic, so stderr as in [Output](#output)), code host and ticket pattern are not detected, and the file listing walks the directory without the ignore list of [`detection.md`](detection.md) and without dot-directories. Inside git, detection lists `git ls-files --cached --others --exclude-standard`; languages and areas keep using the tracked files. Both lists are expanded into initialized submodules and untracked nested repositories, as in [Changed files](#changed-files).

**Detection kept from the previous `init`** (all deterministic, no network):

| What | How | Written as |
| --- | --- | --- |
| Languages | Manifests first, then extensions of `git ls-files` (ignoring `node_modules/`, `target/`, `vendor/`, `dist/`). `package.json`/`tsconfig.json`/`.ts`/`.tsx` -> `typescript`; `.js`/`.mjs`/`.cjs`/`.jsx` -> `javascript`; `go.mod`/`.go` -> `go`; `Cargo.toml`/`.rs` -> `rust`; `pyproject.toml`/`requirements.txt`/`.py` -> `python`; `pom.xml`/`build.gradle`/`.java` -> `java`; `.kt` -> `kotlin`; `Gemfile`/`.rb` -> `ruby`; `.cs`/`*.csproj` -> `csharp`; `composer.json`/`.php` -> `php`. | `[app].languages`, sorted. |
| Code host | `git remote get-url origin`: host `github.com` -> `github`; host containing `gitlab` -> `gitlab`; otherwise omitted. | `[code_host].kind` |
| Ticket pattern | First `[A-Z][A-Z0-9]+-\d+` prefix found in local and remote branch names. | `[tracker]` with `kind = "jira"`, `project`, `ticket_pattern = '<PROJECT>-\d+'` |
| Areas | One area per top-level directory that contains tracked source files (excluding `e2e-playwright`, `.github`, `docs`, dot-dirs), key = directory name lower-cased with non `[a-z0-9_]` replaced by `_`, `paths = ["<dir>/**"]`, `description = "TODO: describe <dir>"`. If none: `[areas.app]` with `paths = ["**"]`. | `[areas.<key>]` |

**Replaced by [`detection.md`](detection.md)**: start/stop (previously only the root compose file or `scripts.start`), `base_url` (previously always `http://localhost:3000`) and `ready_path` (previously always `/healthz`). When detection finds nothing it falls back to those previous values (`http://localhost:3000`, `/healthz`, root compose `docker compose up -d --wait` / `docker compose down -v`, else `npm start`), so a repo with nothing new to detect gets the same `target.toml` as before. `[app].name` is still the repo directory name normalised to `^[a-z0-9][a-z0-9_-]*$`.

stdout (`--yes`, multi-service repo; evidence lines shortened):

```text
plan:
  infra      docker compose -f docker-compose.yml up -d --wait postgres redis mailpit   (high)
               docker-compose.yml services postgres, redis, mailpit
  service    backend  cwd=backend  elysia/bun  start="bun run dev"  ready=http://localhost:3001/health  (high)
               backend/package.json dependencies.elysia
               backend/bun.lock
               ...
  service    frontend  cwd=frontend  next/bun  start="bun run dev"  ready=http://localhost:3000/  (medium)
               ...
  base_url   http://localhost:3000   ready_path /   (first frontend service frontend)
  languages  php, typescript
  code host  github
  areas      backend, frontend (edit their descriptions)
note: ignored frontend/docker-compose.dokploy.yml (production compose file (dokploy))
note: frontend: bun.lock and package-lock.json; using bun
created target.toml
created e2e-playwright/ (20 files)
detected languages: php, typescript
detected code host: github
detected start command: docker compose -f docker-compose.yml up -d --wait postgres redis mailpit
proposed areas: backend, frontend (edit their descriptions)
next steps:
  1. review target.toml (area descriptions start with TODO)
  2. cd e2e-playwright && npm install && npx playwright install chromium   (or guard init --install)
  3. guard up && guard status
  4. guard test --all
  5. guard new journey <area>.<name> --title "..."
```

With a single service at the root the service line is still printed and its start command becomes `[app].start`; with no service, a `start` line shows the fallback start command. Step 2 of `next steps` is left out after a successful install.

The `created ...`, `detected languages:`, `detected code host:` (`none` when absent), `detected start command:` (`[app].start`, `none` when absent) and `proposed areas:` lines keep the format of the previous `init`. The previous single `next: copy the reference scaffold ...` line is replaced by the `next steps:` block, because `init` now writes the scaffold itself.

`--dry-run` prints the same plan, then `would write target.toml:` followed by the full file, then `would create e2e-playwright/ (20 files)` (or the `exists, scaffold skipped` note), `would write <path>` per `--mcp` file, `would create <path>` for the hook and CI file (or their `exists, kept` note), and with `--install` `would run npm install && npx playwright install chromium in e2e-playwright`; nothing else. It asks nothing and runs nothing. With `--runner self` the printed file is the self-mode one; without `--runner` a dry run uses `guard`.

Exit: `0` created (or dry run, or the final confirmation answered no); `2` `target.toml` exists without `--force` (also with `--dry-run`), invalid `--mcp`/`--ci`/`--runner` value, an existing `.mcp.json` that is not a JSON object (checked before anything is written), a generated `target.toml` that does not validate; `3` cannot write, `--install` failed; `130` cancelled with Ctrl+C at a styled prompt.

---

## `guard new journey`

```text
guard new journey <id> [--area <key>] [--title <text>] [--path <glob>]... [--smoke] [--record]
```

Adds one journey to the catalog and creates its spec skeleton.

| Argument / flag | Default | Meaning |
| --- | --- | --- |
| `<id>` | required | Must match `^[a-z0-9]+(\.[a-z0-9-]+)+$` ([`journeys-catalog.md`](journeys-catalog.md)); else exit `2`. Already in the catalog = exit `2`. |
| `--area <key>` | first segment of `<id>` | Must be a key of `[areas]` in `target.toml`; else exit `2` with `hint: areas: <key>, <key>, ...`. |
| `--title <text>` | `<id>` without the area segment, `-` and `.` replaced by spaces, first letter upper-cased | Non-empty. |
| `--path <glob>` | none (empty `paths`) | Repeatable. Repo-relative globs; each must compile (else exit `2`). |
| `--smoke` | off | `smoke = true` in the catalog and `@smoke` on the spec. |
| `--record` | off | After writing, run `npx playwright codegen <base_url>` in the Playwright dir (inherits the terminal). Its exit status is reported but never changes `new journey`'s exit code once the files are written. |

Writes:

1. The catalog entry, appended to `journeys[]` of `<e2e-dir>/catalog/journeys.json` (two-space indent, `paths` on one line, trailing newline): `id`, `area`, `title`, `impact_question = "This diff can directly or indirectly change how " + lowercase(title)`, `paths`, `smoke`, `quarantined: false`. The bytes of the existing entries are kept as they are when `journeys` is the file's last key; any other layout is rewritten whole in that canonical layout, keys in schema order. The result must validate against `schemas/journeys.schema.json` and hold exactly the previous journeys plus the new one.
2. `<e2e-dir>/tests/journeys/<area>/<id without the first segment>.spec.ts` (for `checkout.customer-card`: `tests/journeys/checkout/customer-card.spec.ts`; further dots stay in the file name). A `test.describe(<title>, { tag: [...] })` with exactly the catalog tags, in this order: `@journey:<id>`, `@area:<area>`, plus `@smoke` iff `--smoke`, importing `test`/`expect` from `../../../src/fixtures`, and one `test.fixme` skeleton. An existing spec file is never overwritten: exit `2` before anything is written. The spec is written first; if the catalog then cannot be written, the new spec is removed again so neither file exists without the other.

Checks run in this order, each exit `2` with nothing written: id, title and globs; `target.toml`; area; catalog (schema and cross-file rules, as `guard doctor`); duplicate id; existing spec.

With `--record`, a `codegen` that exits non-zero is a `warning:` line (exit stays `0`); `npx` that cannot be started is exit `3`. `GUARD_NPX` overrides the `npx` program, as for `guard test`.

stdout:

```text
added journey checkout.customer-card to e2e-playwright/catalog/journeys.json
created e2e-playwright/tests/journeys/checkout/customer-card.spec.ts
next: guard test --journey checkout.customer-card
```

Exit: `0`; `2` invalid id, unknown area, duplicate id, existing spec, invalid glob, invalid config or catalog; `3` cannot write, `npx` missing with `--record` (files already written).

---

## `guard status [--json]`

What runs where, read from `target.toml` only (no detection, no starting or stopping).

For `[app]` and each `[[services]]` entry, in file order:

| Field | `[app]` | Service |
| --- | --- | --- |
| `name` | `[app].name` | `name` |
| `cwd` | `.` | `cwd` |
| `start` | `[app].start` or `null` | `start` |
| `ready_url` | `base_url + ready_path` | `ready_url` or `null` |
| `pid` | from `.guard/app.pid`, or `null` | from `.guard/services/<name>.pid`, or `null` |
| `alive` | the PID exists and its identity file still matches (`null` without a PID) | same, with `.guard/services/<name>.identity` |
| `ready` | `GET ready_url` returns `200` within 2 s (`null` without a `ready_url`) | same |
| `log` | `.guard/app.log` | `.guard/services/<name>.log` |

Text:

```text
NAME      CWD       PID   ALIVE  READY  URL                           LOG
saunas    .         4242  yes    yes    http://localhost:3000/        .guard/app.log
backend   backend   4250  yes    yes    http://localhost:3001/health  .guard/services/backend.log
frontend  frontend  -     -      no     http://localhost:3000/        .guard/services/frontend.log
base_url: http://localhost:3000 ready
```

Each column is as wide as its widest cell plus two spaces; trailing spaces are trimmed. `-` = `null`. The last line ends in `ready` or `not ready` (the `[app]` readiness).

`--json` (one object, pretty-printed): `{"base_url": "...", "ready": true, "entries": [{"kind": "app" | "service", "name", "cwd", "start", "ready_url", "pid", "alive", "ready", "log"}, ...]}`, entries in the order above.

`alive` with a PID file but no identity file means only that the process exists. `status` never signals a process, even a stale one.

Exit: `0` always, whatever is running or not; `2` invalid `target.toml`.

### `guard status --integrations [--json]`

The integrations of the E2E flow, one row each, as the GUI's Flow view badges its nodes. Read-only: no file is written, no database is migrated. Every probe is a cheap health or auth check bounded by 2 s, and all of them run in parallel.

| `id` | Flow nodes | Source of truth | Probe |
| --- | --- | --- | --- |
| `jira` | `jira` | `[tracker]` of `target.toml`; `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN` (environment or the credentials file) | `GET /rest/api/3/myself` |
| `server` | `ingest` | `[server].url` | `GET <url>/healthz` |
| `jev` | `jev` | the reachable server, else `TYPESAFE_API_KEY` / `GUARD_JEV_FAKE` | none (a Jev call is billed) |
| `gatekeeper` | `gate` | `thresholds.toml` mode (default Shadow) and the server | the server's |
| `agents` | `qa`, `seal`, `coder` | `agents.toml` and `DATABASE_URL` (Postgres) | `SELECT` of the running `task_queue` rows of the repo |
| `gateway` | `gateway` | `GUARD_GATEWAY_ADMIN_URL`, else `agents.toml` `[gateway].url` | `GET <url>/health/liveliness` (LiteLLM) |
| `skills` | `skills` | `agents.toml`, `AGENTS.md` | none |
| `local` | `local` | `[app]` readiness URL | `GET` it |
| `code_host` | `direct`, `devpr` | `[code_host]`; `GITHUB_TOKEN` / `GITLAB_TOKEN` | `GET /user` |
| `ci` | `guard` | `.github/workflows/*.y(a)ml` and `.gitlab-ci.yml` of the root and each service dir that mention `guard gate` (or the `guard-ci/gate` component) | none |

`state` is one of `connected`, `configured` (set up; nothing cheap to probe), `not_configured`, `disconnected` (no answer), `error` (refused, or an invalid file) and `working` (the agent lane has a running task).

`--json`: `{"integrations": [{"id", "nodes": [...], "label", "state", "detail", "configure"?, "tasks"?: [{"ticket", "role", "branch", "attempts"}]}, ...]}` in the order above. `configure` is the command or edit that sets it up. Details name variables, never their values, and never quote `DATABASE_URL`.

Exit: `0` always; `2` invalid `target.toml`.

---

## `guard connect`

```text
guard connect jira [--base-url <url>] [--email <email>] [--project <KEY>] [--token-stdin]
guard connect github [--api-url <url>] [--token-stdin]
guard connect gitlab [--api-url <url>] [--token-stdin]
guard connect --list
```

Connects guard to a service once per machine: validates the credentials against the real API, stores them in the [user credentials file](#user-credentials-file) and writes the non-secret table to `target.toml`.

| Service | Non-secret fields (prompt / flag / env) | Secret | Validation call | Stored keys | `target.toml` |
| --- | --- | --- | --- | --- | --- |
| `jira` | base URL (`--base-url`, `JIRA_BASE_URL`), email (`--email`, `JIRA_EMAIL`), project (`--project`, optional, `^[A-Z][A-Z0-9_]*$`) | API token | `GET /rest/api/3/myself` through `guard-tracker` | `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN` | `[tracker]` `kind = "jira"` and `project` if given; other `[tracker]` keys kept |
| `github` | API URL (`--api-url`, `GITHUB_API_URL`, default `https://api.github.com`) | token | `GET /user` through `guard-codehost` | `GITHUB_TOKEN`, plus `GITHUB_API_URL` when not the default | `[code_host]` `kind = "github"` |
| `gitlab` | API URL (`--api-url`, `GITLAB_API_URL`, default `https://gitlab.com/api/v4`) | token | `GET /user` through `guard-codehost` | `GITLAB_TOKEN`, plus `GITLAB_API_URL` when not the default | `[code_host]` `kind = "gitlab"` |

Input rules:

- **Interactive** (stdin is a TTY): each non-secret field without a flag is prompted with its current value as the default (`Jira base URL [https://acme.atlassian.net]: `); Enter accepts it, typed text replaces it. The secret is read without echo (`rpassword`); when a value is already set the prompt reads `Jira API token [keep current]: ` and Enter keeps it. It is never printed back.
- **Non-interactive**: every required field must come from a flag or the environment, else exit `2` naming the missing flag. The secret comes from its env var (`JIRA_API_TOKEN`, `GITHUB_TOKEN`, `GITLAB_TOKEN`) or, with `--token-stdin`, from the first line of stdin. There is no flag that takes a secret on the command line.
- Field checks before any call (exit `2`): URLs must be `https://` (plain `http://` only for a loopback host: `localhost`, `127.x.x.x`, `[::1]`, so a token never crosses the network in cleartext) without embedded credentials (`--base-url must not contain credentials`); code host requests never follow a redirect to another origin (GitLab's `PRIVATE-TOKEN` header is not stripped by the HTTP client on such a redirect), the email must contain `@`, the project must match the pattern.
- The validation call goes through a port (`guard-tracker` `JiraTracker::myself`, `guard-codehost` `whoami::github_user` / `gitlab_user`, behind the `Verifier` trait) so tests use fakes or a local wiremock server; no test touches the network. `401`/`403` = exit `2` (`error: jira rejected the credentials (401)`); any other `4xx` = exit `2` (`error: jira answered HTTP 404`, `hint: check the URL`); connection failure, timeout, `429`, `5xx` or an unexpected body = exit `3` (`error: cannot reach jira: <reason>`). Nothing is stored or written unless validation succeeded.
- The API URL is stored (`GITHUB_API_URL`, `GITLAB_API_URL`) only when it is not the default; connecting with the default removes a stored one. Other keys already in the credentials file are kept. An existing credentials file that cannot be read or parsed is not replaced (exit `3`, `hint: fix or delete the file`).
- `target.toml` must exist and be valid (else exit `2`, `hint: run guard init`; checked before any prompt) and still validates after the edit; the rest of the file is preserved byte for byte (`guard_core::config_edit`: keys of the `[tracker]` / `[code_host]` header are replaced in place, missing keys go after the table's last key, a missing table is appended). A table written as an inline table, dotted keys or with sub-tables is exit `2` (`edit it by hand`).
- Secrets never appear in `target.toml`, the repository, `.guard/`, stdout, stderr or error messages; errors name the variable (`JIRA_API_TOKEN`), never the value.

stdout:

```text
jira: authenticated as Ada Lovelace (ada@acme.test)
saved JIRA_BASE_URL, JIRA_EMAIL, JIRA_API_TOKEN to /home/ada/.config/guard/credentials.toml
updated target.toml [tracker] kind = "jira", project = "SHOP"
```

`--list` (needs no `target.toml`) prints every known key, where its effective value comes from, and the value masked for secrets (`****` + last 4 characters when longer than 8, else `****`); URLs and emails are shown in full:

```text
JIRA_BASE_URL    https://acme.atlassian.net   credentials file
JIRA_EMAIL       ada@acme.test                credentials file
JIRA_API_TOKEN   ****9f3a                     credentials file
GITHUB_TOKEN     ****b7c1                     environment
GITLAB_TOKEN     -                            not set
```

Exit: `0`; `2` usage, missing input, credentials rejected, invalid `target.toml`; `3` network failure, cannot write the credentials file.

---

## User credentials file

Per-user secrets for `guard connect`, outside any repository.

| Item | Value |
| --- | --- |
| Location | `GUARD_CREDENTIALS_FILE` when set (tests use it); else Windows `%APPDATA%\guard\credentials.toml`; else `$XDG_CONFIG_HOME/guard/credentials.toml`; else `~/.config/guard/credentials.toml`. |
| Permissions | Created with mode `0600` (directory `0700`) on Unix; on Windows the per-user `%APPDATA%` ACL applies. Written atomically (temp file + rename). |
| Format | UTF-8 TOML, flat string keys named after the environment variables guard already reads: `JIRA_BASE_URL`, `JIRA_EMAIL`, `JIRA_API_TOKEN`, `GITHUB_TOKEN`, `GITHUB_API_URL`, `GITLAB_TOKEN`, `GITLAB_API_URL`. Other keys are ignored with `warning: credentials file: unknown key <KEY>` (so the file can never set arbitrary variables such as `PATH`). |
| Loading | At startup, before any command runs, guard reads the file (missing file = nothing to do; unreadable or invalid TOML = `warning:` and continue, e.g. `warning: credentials file: invalid TOML at line 2`, which names the line but never quotes it; a known key whose value is not a string = `warning: credentials file: <KEY> is not a string`) and sets each known key **only if that variable is not already set** in the environment (set to an empty value counts as set). The environment always wins, so CI secrets override a developer's stored values. Child processes (Playwright, `[app].start`, services) inherit the result. |
| Never | Read from or written to the repository, `target.toml` or `.guard/`; printed; included in error messages. |

```toml
# ~/.config/guard/credentials.toml (written by guard connect)
JIRA_BASE_URL = "https://acme.atlassian.net"
JIRA_EMAIL = "ada@acme.test"
JIRA_API_TOKEN = "<token>"
GITHUB_TOKEN = "<token>"
```

---

## `guard doctor`

Validates configuration and environment. Runs every check, never stops at the first failure.

| Check | Failure kind |
| --- | --- |
| `target.toml` parses and validates against `schemas/target.schema.json` | config error |
| `journeys.json` parses and validates against `schemas/journeys.schema.json` | config error |
| Journey ids unique; every journey `area` is a key of `[areas]` | config error |
| `[tracker].ticket_pattern` compiles | config error |
| Each glob in `[areas.*].paths` and in catalog `paths` matches at least one tracked path (`git ls-files`, expanded into submodules as in [Changed files](#changed-files); an uninitialized submodule is a subtree entry) | warning (dead glob) |
| A tracked file matched by two areas | warning |
| An area `description` still starts with `TODO` | warning |
| `node` on PATH (prints version) and `npx` on PATH | environment error |
| `git` on PATH | environment error |
| `docker` on PATH and `docker info` succeeds | warning (only `--docker` needs it) |
| Variable named by `[testability].token_env` is set (only when `roles` is non-empty) | warning |
| `[[services]]` names unique and every `cwd` an existing directory | config error |
| `GET base_url + ready_path` returns `200` within 5 s | warning, with `hint: run guard up` |

stdout, one line per check, then a summary:

```text
ok    target.toml valid
ok    catalog valid (2 journeys, 0 quarantined)
ok    catalog areas exist in target.toml
warn  dead glob in areas.inventory.paths: services/inventory/**
ok    node v24.14.0
ok    npx
ok    git 2.47.0
warn  docker not available (only needed for --docker)
warn  app not ready: GET http://localhost:3000/healthz -> connection refused
doctor: 6 ok, 3 warnings, 0 errors
```

Line prefixes: `ok` (passed), `warn` (warning), `fail` (config or environment error).

Exit: `0` no errors (warnings allowed); `2` any config error; else `3` any environment error.

---

## `guard up` / `guard down`

### `guard up [--timeout <seconds>]`

**Self mode** = `target.toml` has no `[app].start` and no `[[services]]` (what `guard init --runner self` writes). Self mode never spawns anything and writes nothing under `.guard/`:

- `GET base_url + ready_path` already `200`: `up: already ready at <url>`, exit `0` (step 1).
- Otherwise stdout gets one line `up: waiting for your app at <url> (start it yourself; guard does not start it)`, then step 4 polls; ready = `up: ready at <url> in <s>s`, exit `0`.
- Timeout = exit `3`: `error: app not ready at <url> after <n>s: <reason>`, followed by the [self-mode hint](#self-mode-hint).

Otherwise:

1. If `GET base_url + ready_path` already returns `200` and every `[[services]]` entry with a `ready_url` is ready too, print `up: already ready at <url>` and exit `0` without starting anything.
2. If `[app].start` is set, spawn it from the repo root through the platform shell (`sh -c` on Unix, `cmd /C` on Windows) in the background, appending output to `.guard/app.log` and writing its PID to `.guard/app.pid`. A command that exits `0` (for example `docker compose up -d --wait`) is fine; one that exits non-zero before readiness is exit `3`, with the last 20 log lines on stderr. With `[[services]]`, step 3 starts only once `[app].start` has exited `0` or the base URL answers `200`; a launcher still running after `--timeout` is exit `3` (`[app].start did not finish within <n>s; ...`), since the services usually need what it brings up. Also with `[[services]]`, `[app].start` is not run again while the base URL already answers `200` or the PID in `.guard/app.pid` is still the process an earlier `guard up` started (same identity rule as `guard down`), so a long-running `[app].start` is never orphaned. Without `[[services]]` this step is unchanged.
3. For each `[[services]]` entry, in file order: if its `ready_url` already returns `200`, print `up: <name> already ready` and skip it. If `.guard/services/<name>.pid` still holds the process an earlier `guard up` started (its `.identity` matches), it is not spawned again (that would orphan it and run two copies): print `up: <name> already running`, after waiting for its `ready_url` if set (a timeout is exit `3` asking for `guard down` first). Otherwise check that `cwd` exists, spawn `start` from `cwd` detached in its own process group (same mechanism as step 2), appending output to `.guard/services/<name>.log` and writing `.guard/services/<name>.pid` and `.guard/services/<name>.identity`; then, if `ready_url` is set, poll it like step 4. A service that exits non-zero before readiness, a missing `cwd` or a readiness timeout is exit `3` with the last 20 lines of that service's log on stderr.
4. Poll `GET base_url + ready_path` every 500 ms (2 s per-request timeout) until it returns `200`, for at most `--timeout` seconds (default `120`). The timeout applies to each wait separately.

On any exit `3` from steps 2 to 4, `guard up` first terminates the process trees **this call** spawned that are still running, in reverse order (services, then the `[app].start` process, which is today's behavior for `[app]` alone), and deletes their PID files. Processes it did not start are never touched, and `stop` commands are not run: infrastructure that a launcher such as `docker compose up -d --wait` already brought up stays up until `guard down`.

stdout: `up: ready at http://localhost:3000/healthz in 3.2s`, preceded, per service in file order, by `up: <name> ready at <ready_url> in <s>s` (started and ready), `up: <name> started` (started, no `ready_url`), `up: <name> already ready` (skipped) or `up: <name> already running` (recorded process still running). Exit: `0` ready; `2` invalid config; `3` start failed or timeout.

### Self-mode hint

Printed on stderr by `guard up` (timeout) and `guard test` (not ready) in self mode. guard reads the commented detected-commands block of `target.toml` ([`target-toml.md`](target-toml.md#self-mode-runner-self)): the comment lines after the marker line `# Detected commands, uncomment to let guard start it (needed in CI):`, each with its leading `# ` (or `#`) removed, parsed as a TOML fragment of `[app]` keys followed by `[[services]]` tables. With at least one command found:

```text
hint: guard does not start the app (no [app].start in target.toml); start it yourself:
hint:   docker compose -f docker-compose.yml up -d --wait postgres redis mailpit
hint:   backend (in backend): bun run dev
hint:   frontend (in frontend): bun run dev
hint: then run guard up again (or uncomment those lines so guard starts them)
```

(`[app].start` first, then each service in block order; a service at `cwd = "."` has no `(in ...)` part.) Without the block, or when it does not parse: `hint: start your app, then run guard up again`.

### `guard down`

1. For each `[[services]]` entry, in **reverse** file order: if `stop` is set, run it in the foreground from `cwd`; then, if `.guard/services/<name>.pid` exists and `.guard/services/<name>.identity` still matches that process, terminate its process group (the same safety rules as `app.pid`: only a group led by that PID, never guard's own), then delete the files. A failing `stop` is remembered and does not prevent the remaining steps.
2. If `[app].stop` is set, run it in the foreground from the repo root. Non-zero exit is exit `3`.
3. If `.guard/app.pid` exists, terminate that process and its children, then delete the file.

stdout: `down: stopped`. With no `stop` command and no PID file anywhere: `down: nothing to stop`, exit `0`; in self mode (see `guard up`) that line reads `down: nothing to stop: guard did not start the app`. Self mode does not skip `down`: a configured `[app].stop` still runs and a PID file left by an earlier `guard up` (for example before `guard init --force --runner self`) is still terminated as above, printing `down: stopped`. Any failing `stop` (service or `[app]`) is exit `3` after every step ran; the first failure is the `error:` line and later ones follow it.

---

## `guard test`

```text
guard test [--affected | --all | --journey <id>...] [--base <ref>] [--headed | --ui | --debug] [--docker] [--shard <i>/<n>]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--affected` | yes, when no selection flag is given | Selection rule below. |
| `--all` | | Every active journey: `@smoke` ∪ `@journey:<id>` for every journey with `quarantined == false`. |
| `--journey <id>` | | Repeatable (`--journey a.b --journey c.d`). Exactly those `@journey:<id>` tags, quarantined allowed. Unknown id = exit `2`. |
| `--base <ref>` | detected ([Base resolution](#base-resolution)) | Base for `--affected`. Ignored otherwise; with `--all` or `--journey` no base is resolved, so those never fail for lack of one. |
| `--headed` / `--ui` / `--debug` | headless | Passed as-is to Playwright. Mutually exclusive. |
| `--docker` | off | Run inside `mcr.microsoft.com/playwright:v<version>-noble`, `<version>` = installed `@playwright/test` version. Repo root mounted at `/work`, workdir `/work/e2e-playwright`, `--ipc=host`, `--add-host=host.docker.internal:host-gateway`, all runner env vars passed with `-e`. Incompatible with `--ui`, `--debug` and `--headed` (exit `2`). Docker missing = exit `3`. |
| `--shard <i>/<n>` | none | Passed as `--shard=<i>/<n>` to Playwright (count-based, for local reproduction). Requires `1 <= i <= n` (else exit `2`). |

`--affected`, `--all` and `--journey` are mutually exclusive (exit `2`).

### Selection rule for `--affected`

```text
tags = { @smoke }
     ∪ { @ticket:<KEY> }                     if [tracker].ticket_pattern matches the current branch name
     ∪ { @journey:<j.id> : j active, some changed file matches some glob in j.paths }
```

- **Changed files**: see [Changed files](#changed-files).
- **Branch name** = `git rev-parse --abbrev-ref HEAD`; a detached `HEAD` has no ticket. `<KEY>` is the first regex match in the branch name, e.g. `SHOP-123-fix-cart` -> `SHOP-123`.
- Globs use globset semantics (`literal_separator = true`, so `*` does not cross `/`; `**` does).
- Quarantined journeys are never selected by `--affected`.
- The impact engine (Jev) is not part of Phase 1.

### Base resolution

`<base>` is the first candidate below that resolves to a commit (`git rev-parse --verify <c>^{commit}`).

1. `--base <ref>`. Strict: the value must be non-empty, must not start with `-`, and must resolve, else exit `2` (`error: unknown git ref: <ref>`, plus `hint: detected default base: <ref>` when steps 4-6 find one). No later step replaces it.
2. `GUARD_BASE` (environment). Same strictness as `--base`.
3. CI (each candidate that does not resolve is skipped with `warning: base candidate <c> from <VAR> is not a commit of this clone`):
   - GitHub: `<remote>/$GITHUB_BASE_REF` for each remote of step 4, then `pull_request.base.sha` from the JSON at `GITHUB_EVENT_PATH`.
   - GitLab: `$CI_MERGE_REQUEST_DIFF_BASE_SHA`, then `<remote>/$CI_MERGE_REQUEST_TARGET_BRANCH_NAME` for each remote of step 4.
4. Remote default. Remotes are tried in this order, without duplicates: `origin`, then `branch.<current>.remote` (skipped when HEAD is detached), then the only remote when there is exactly one. For each remote `R`: the target of `refs/remotes/R/HEAD` (`git symbolic-ref`); a target that does not resolve, such as a deleted branch, is skipped.
5. For each remote `R` of step 4: `R/main`, then `R/master`.
6. Local branches: the value of `init.defaultBranch`, then `main`, `master`, `trunk`.
7. Nothing resolved: exit `2` with `error: no base branch found; tried: <comma-separated candidates of steps 4-6>` and `hint: pass --base <ref> or set GUARD_BASE`. In a shallow clone (`git rev-parse --is-shallow-repository` = `true`, such as a CI checkout with the default fetch depth of 1), where the base was most likely never fetched, the lines read `error: no base branch found in this shallow clone; tried: <candidates>` and `hint: fetch full history (fetch-depth: 0 / GIT_DEPTH: 0), or pass --base <ref> or set GUARD_BASE`.

The current branch is not added as a candidate (it is one only when it is one of the names above). guard never fetches and never runs `git remote set-head`.

After resolution: `<merge-base>` = `git merge-base <base-sha> HEAD`. Errors:

- HEAD has no commit: exit `2`, `error: repository has no commits yet`, `hint: commit once, or use --all`.
- No merge base in a shallow clone (`git rev-parse --is-shallow-repository` = `true`): exit `2`, `error: shallow clone: <base> is not reachable from HEAD`, `hint: fetch full history (fetch-depth: 0 / GIT_DEPTH: 0) or pass --base <sha>`.
- No merge base otherwise: exit `2`, `error: <base> and HEAD share no history`.

### Changed files

Changed files are collected for the superproject and then, recursively, for every repository nested in it. All paths are relative to the superproject root, use `/`, are deduplicated, and keep the order in which they were collected. Deleted files count.

For one repository level `R`, mounted at prefix `P` (empty at the top), compared from commit `F` (`<merge-base>` at the top):

1. **Committed**: `git diff --raw -z --no-renames --no-abbrev --ignore-submodules=none <F> HEAD`.
2. **Working tree**: `git diff --raw -z --no-renames --no-abbrev --ignore-submodules=none HEAD` (staged and unstaged).
3. **Untracked**: `git ls-files -z --others --exclude-standard`.

Every entry whose modes are not `160000` is reported as `P<path>`. Entries with mode `160000` on either side are submodule pointers (gitlinks) and are expanded; below, `P<path>/` + a list means each listed path prefixed with `P<path>/`:

| Pointer change between `F` and HEAD (step 1) | Reported paths |
| --- | --- |
| modified `old -> new` | `P<path>/` + `git -C <path> diff --raw -z --no-renames <old> <new>`, recursively. Two-dot, so a rollback or diverged commits work the same way. |
| added (`old` = zeros) | `P<path>/` + every path of `git -C <path> ls-tree -r -z <new>`, recursively |
| deleted (`new` = zeros) | `P<path>/` + every path of `<old>` from the submodule's object store, `$(git rev-parse --git-common-dir)/modules/<name>`, where `<name>` comes from `.gitmodules` at `<F>` (the path itself when `.gitmodules` has no entry) |
| file <-> submodule (type change) | the file path, plus the added or deleted rule for the submodule side |
| moved | the deleted rule at the old path plus the added rule at the new path (`--no-renames`) |

A submodule that is dirty in the working tree (a gitlink entry of step 2: modified, staged or untracked inner files, a staged pointer, or inner commits not yet added to the superproject) is treated as its own level. It is compared from the pointer recorded in HEAD, so that covers unstaged, staged and untracked inner files and inner commits. A submodule staged as new, with no pointer in HEAD, reports its index (`git -C <path> ls-files -s -z`) followed by its untracked files; each gitlink in that index is expanded the same way, recursively (an uninitialized one is the subtree entry `P<path>/<sub>/` with the not-initialized warning). A submodule removed from the index uses the deleted rule from HEAD's pointer.

An untracked directory that contains `.git` (an unregistered nested repository, which `ls-files --others` reports as `<dir>/`) is listed like a submodule staged as new: its index (`git -C <dir> ls-files -s -z`) with every gitlink in it expanded recursively, then its untracked files. A submodule of that nested repository is therefore reported under `P<dir>/<sub>/` like any other.

**Subtree entry.** When a submodule's inner change can't be read, the submodule is reported as the single entry `P<path>/` (trailing `/`). A subtree entry stands for every path under it. Each case prints one `warning:` line on stderr and never changes the exit code:

- not initialized (no `<path>/.git`): `warning: submodule <P><path> is not initialized; treating it as entirely changed`
- commit missing (`git cat-file -e <sha>^{commit}` fails): `warning: submodule <P><path>: commit <sha7> not present; treating it as entirely changed`
- object store of a deleted submodule gone: `warning: submodule <P><path>: removed and its object store is gone; treating it as entirely changed`
- nesting deeper than 8 levels: `warning: submodule <P><path>: nested deeper than 8 levels; treating it as entirely changed`

A subtree entry `D/` matches a glob `G` when `G` matches `D/`, or when `G`'s literal prefix (the text before its first `*`, `?`, `[` or `{`, or all of `G` when it has none) starts with `D/`, or when `G` contains a glob metacharacter and `D/` starts with that literal prefix. The journey's reason is `{"kind": "path", "file": "D/", "glob": G}`.

An unchanged submodule contributes nothing, initialized or not. guard never runs `git submodule update` for the change set.

`--ignore-submodules=none` is always passed, so `submodule.<name>.ignore` and `diff.ignoreSubmodules` cannot hide changes. A repository with no gitlink in either diff runs exactly the three commands above.

### Flow

1. Load and validate `target.toml` and the catalog (exit `2` on error).
2. Compute tags; print the selection to stdout (same text as `guard affected`).
3. Check readiness once (`GET base_url + ready_path`), before Playwright starts; not ready = exit `3` (`error: app not ready: GET <url> -> <reason>`) with `hint: run guard up`. In self mode (no `[app].start`, no `[[services]]`) the hint is the [self-mode hint](#self-mode-hint) instead, since `guard up` cannot start anything.
4. If `roles` is non-empty and the token variable is unset: `warning:` and continue.
5. Run Playwright with the [runner env](runner-env.md).
6. Unless `--ui`/`--debug`, parse `results/report.json` and print:

```text
passed 12  flaky 0  failed 1  skipped 0  (38.4s)
FAILED @journey:checkout.customer-card  tests/journeys/checkout/customer-card.spec.ts:9  creates exactly one confirmed order
  Error: expect(locator).toBeVisible() failed
report: guard report
```

Exit: `0` no unexpected failures; `1` at least one; `2` usage/config; `3` environment or Playwright infrastructure error (runner-env exit table). With `--ui`/`--debug`, Playwright's exit `0`/`1` map to `0`/`1`, anything else to `3`.

---

## `guard affected [--base <ref>] [--json]`

Prints the `--affected` selection without running anything. Exit `0`, `2` (bad config, unknown or undetectable base, no commits, no merge base), `3` (not a git repo, `git` missing). `--base` is resolved as in [Base resolution](#base-resolution).

Text (default):

```text
base: main (merge-base 1a2b3c4; local_branch)
branch: SHOP-123-fix-cart
changed files: 3
@smoke                            smoke
@ticket:SHOP-123                  ticket from branch SHOP-123-fix-cart
@journey:checkout.customer-card   path web/src/checkout/cart.ts matches web/src/checkout/**
```

`--json` (one object, pretty-printed, keys in this order, `tags` sorted lexicographically):

```json
{
  "base": "main",
  "base_source": "local_branch",
  "merge_base": "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
  "branch": "SHOP-123-fix-cart",
  "ticket": "SHOP-123",
  "changed_files": ["web/src/checkout/cart.ts", "README.md", "docs/x.md"],
  "tags": ["@journey:checkout.customer-card", "@smoke", "@ticket:SHOP-123"],
  "journeys": [
    {
      "id": "checkout.customer-card",
      "tag": "@journey:checkout.customer-card",
      "reasons": [
        { "kind": "path", "file": "web/src/checkout/cart.ts", "glob": "web/src/checkout/**" },
        { "kind": "smoke" }
      ]
    }
  ]
}
```

The text header is `base: <ref> (merge-base <sha7>; <base_source>)`. `base` is the candidate that resolved (its text, for example `origin/trunk` or a sha); `base_source` is one of `flag`, `env`, `ci`, `remote_head`, `remote_branch`, `local_branch`.

- `ticket` and `branch` are `null` when absent.
- `journeys[]` lists every active journey that will run: selected by path (`kind: "path"`, one entry per matching file, first matching glob) and/or because it is a smoke journey (`kind: "smoke"`, it runs through `@smoke`). Sorted by `id`.
- The ticket reason is not per journey: it shows up as the `@ticket:<KEY>` tag.

---

## `guard gate plan --shards <N> --out <dir>`

Writes one plan per shard, balanced by historical duration (LPT: heaviest file first, each to the currently lightest shard).

- **Files** = every `tests/**/*.spec.ts` under the Playwright dir, excluding `tests/setup/**`, as forward-slash paths relative to the Playwright dir.
- **Durations** from `<repo root>/.guard/durations.json`. Missing file = no history (not an error); malformed = exit `2`. A file without history gets the median of the known durations; with no history at all every file weighs `1000` ms.
- **Determinism**: sort by duration descending, ties by path ascending; ties for the lightest shard go to the lowest index.
- `N >= 1` (else exit `2`). Shards are 1-based. When `N` exceeds the number of files, the extra shards get `{"files": []}`.

Outputs `<dir>/shard-1.json` ... `<dir>/shard-<N>.json`, each exactly:

```json
{ "files": ["**/tests/journeys/checkout/customer-card.spec.ts", "**/tests/smoke/healthz.spec.ts"] }
```

`.guard/durations.json`:

```json
{
  "version": 1,
  "files": {
    "tests/journeys/checkout/customer-card.spec.ts": { "duration_ms": 12450, "samples": 7 }
  }
}
```

stdout:

```text
shard 1/2: 2 files, est 12.5s
shard 2/2: 1 file, est 9.8s
```

Exit: `0`, `2` (bad args, malformed durations), `3` (cannot write `<dir>`).

---

## `guard gate shard --index <i> --plan <dir>`

Runs one shard, headless, with the blob reporter.

1. Read `<dir>/shard-<i>.json` (missing or invalid = exit `2`). If `files` is empty, print `shard <i>: nothing to run` and exit `0` without launching Playwright.
2. If the app or any `[[services]]` entry is not ready (step 1 of `guard up`), run the `guard up` procedure; remember whether this command started anything. In self mode that procedure only waits (CI needs the commented start commands enabled).
3. Run Playwright with `CI=1`, `GUARD_SHARD_INDEX=<i>`, `GUARD_SHARD_PLAN=<absolute path of shard-<i>.json>`, `E2E_TAGS` = the `--all` selection, plus the rest of the [runner env](runner-env.md). Output: `blob-report/report-<i>.zip`.
4. If step 2 started anything, run the `guard down` procedure for exactly what it started: the services it spawned (their `stop`, then their recorded PID, in reverse order) and, only if it spawned `[app].start`, `[app].stop` and `.guard/app.pid`. Services and infrastructure it found already running are left alone.

Exit: `0` even when tests fail; `2` usage/plan error; `3` infrastructure error only (app never ready, Playwright exit not in `{0, 1}`, or no blob produced).

---

## `guard gate finalize --blobs <dir>`

The only gate step that decides.

1. `<dir>` must exist (else exit `2`) and contain at least one `*.zip` (else exit `3`).
2. Run `npx playwright merge-reports --reporter=json,html <dir>` in the Playwright dir with `PLAYWRIGHT_JSON_OUTPUT_NAME=<abs>/results/report.json`. Failure or zero tests in the merged report = exit `3`.
3. Classify every test (every `tests[]` entry of every spec, setup project included):

| JSON status | Classification | Blocking |
| --- | --- | --- |
| `expected` | `passed` | no |
| `flaky` (failed, then passed on a retry) | `flaky` | **yes** if the test has the `@smoke` tag, otherwise no |
| `unexpected` (failed all 3 attempts) | `failed` | yes |
| `skipped` | `skipped` | no |

4. Verdict = `red` if any test is blocking, else `green`. Write `.guard/verdict.json` and update `.guard/durations.json`.
5. Print the summary.

`.guard/verdict.json`:

```json
{
  "version": 1,
  "run_id": "12345-1",
  "generated_at": "2026-10-31T12:00:00Z",
  "verdict": "red",
  "counts": { "passed": 40, "flaky": 2, "failed": 1, "skipped": 0 },
  "tests": [
    {
      "title": "Checkout: customer pays by card > creates exactly one confirmed order",
      "file": "tests/journeys/checkout/customer-card.spec.ts",
      "line": 9,
      "project": "chromium",
      "tags": ["@area:checkout", "@journey:checkout.customer-card", "@smoke"],
      "journey": "checkout.customer-card",
      "status": "flaky",
      "blocking": true,
      "attempts": 2,
      "duration_ms": 18230,
      "error": "expect(locator).toBeVisible() failed"
    }
  ],
  "baseline": null,
  "not_implemented": ["baseline", "tracker"]
}
```

- `tests[]` sorted by `file`, then `line`, then `project`. `journey` is the id from the `@journey:` tag, or `null`. `attempts` = number of results. `duration_ms` = sum over attempts. `error` = first line of the last failing attempt's message, or `null`.
- `run_id` = `RUN_ID` from the environment, else generated as in runner-env.

**Durations update**: for each spec file, the run's duration is the sum of `duration_ms` of its tests. New value = that duration if the file had no history, else `round((old + new) / 2)`; `samples += 1`. Files not in this run keep their entry.

stdout:

```text
verdict: red  (passed 40, flaky 2, failed 1, skipped 0)
FAILED  tests/journeys/billing/invoice-pdf.spec.ts:7  billing.invoice-pdf
FLAKY (blocking: smoke)  tests/journeys/checkout/customer-card.spec.ts:9  checkout.customer-card
not yet implemented: baseline run against the base commit, Jira reporting
```

Exit: `0` green, `1` red, `2` usage, `3` infrastructure.

**Not yet implemented (Phase 2+)**: re-running confirmed failures against the base commit (baseline: "already broken on main" vs "regression of this change"), evidence upload, and Jira/PR reporting. In Phase 1 every confirmed failure blocks; `baseline` is always `null`. Phase 2 behavior is specified in [`finalize-phase2.md`](finalize-phase2.md); `--affected` with impact, `guard serve`, `guard mcp` and `guard repro` in [`ports.md`](ports.md) ("Phase 2 changes to Phase 1 contracts").

---

## `guard report`

Runs `npx playwright show-report playwright-report` in the Playwright dir (serves and opens the HTML report of the last local run or `finalize`). Exit `0`; `3` when `playwright-report/index.html` does not exist (`hint: run guard test first`).

## `guard explain`

`guard explain [--journey <id>] [--report <path>] [--model <id>] [--lang <code>] [--format table|md|json|xlsx] [--out <path>] [--json]` asks Claude (or any Anthropic-compatible Messages API: `GUARD_EXPLAIN_BASE_URL`) to explain the last run's failing and flaky journeys, grouped by root cause. Key: `GUARD_EXPLAIN_API_KEY`, else `ANTHROPIC_API_KEY`; never printed, logged or echoed (error bodies are redacted). Only the report's failure fields (journey id, title, file, line, status, error) and each journey's top `guard suspects` are sent. Nothing failing: a green message, no API call, no key needed. `guard explain --list-models [--json]` lists the provider's models.

Exit `0`; `2` usage (`--json` with `--format`, `--format xlsx` without `--out`, `--out` with `--list-models`); `3` no report, no key, API error, or `--out` not writable.

The model is asked for one JSON object; ```` ```json ```` fences and surrounding prose are tolerated. When the reply is not that object (free text), every format falls back to the model's Markdown as is.

```json
{"rows":[{"tests":"<label (N tests)>","journeys":["<id>"],"what_happened":"<symptom, key error quoted>","probable_cause":"<verdict sentence, then detail>","kind":"app_bug|suite_maintenance|test_data|environment|unknown"}],
 "verdict":"<which rows are real app bugs vs suite, data or environment>"}
```

| `--format` | Output |
| --- | --- |
| `table` (default) | `Explained N failing/flaky journey(s) with <model>.`, then a box-drawing table (`┌─┬─┐`) with `#`, test, what happened, probable cause, wrapped to `COLUMNS` (default 160), then the verdict. |
| `md` | `# guard explain — <YYYY-MM-DD> · <model>`, a GitHub Markdown table with five columns (`|` escaped, newlines as `<br>`, the cause's first sentence in bold), `**<Verdict>:** …`, then a `Journeys:` list of journey ids per row. |
| `json` (= `--json`) | One line: `{model, rows, verdict, summary, journeys}`. `summary` is the `md` rendering (free text: the raw reply), `journeys` the failures sent; free text gives `rows: []`, `verdict: null`. |
| `xlsx` | Workbook with sheet `Explain` (bold frozen header with autofilter, wrapped cells; `#`, test, what happened, probable cause, kind, journeys) and sheet `Resumen` (model, date, verdict). Needs `--out`. |

Headers follow `--lang`: `es` → `#`, `Test`, `Qué pasó`, `Causa probable`, `Tipo` (kinds: `Bug de la app`, `Mantenimiento del suite`, `Datos de prueba`, `Ambiente`, `Sin determinar`); any other code → `#`, `Test`, `What happened`, `Probable cause`, `Kind`.

`--out <path>` writes the rendered output to the file instead of stdout and prints `wrote <path>` to stderr. The GUI uses `--format md --out <path>` and `--format xlsx --out <path>`.

## Phase 4: `guard agents`, `guard memory`

| Command | Contract |
| --- | --- |
| `guard agents check \| sync [--check] \| run <TICKET> \| status \| ab-report` | agents-toml.md, "`guard agents` subcommands" |
| `guard memory export --engram <file>` | memory.md, "Engram export": the repo's facts (`PgMemoryFacts::stored_facts`) as an Engram import file; needs `DATABASE_URL`, never writes memory |

The Phase 2 `guard agents` placeholder is gone; unknown subcommands and flags are usage errors (exit 2).
