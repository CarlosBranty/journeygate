# Contract: `target.toml`

`target.toml` is the only coupling point between the platform and the app under test. It lives at the root of the app repository, is created by `guard init` and is validated by `guard doctor`.

- Machine-readable schema: [`schemas/target.schema.json`](../../schemas/target.schema.json) (JSON Schema draft 2020-12, applied to the TOML document after parsing it into JSON).
- Valid examples: [`schemas/examples/target.example.json`](../../schemas/examples/target.example.json) (full), [`schemas/examples/target.minimal.json`](../../schemas/examples/target.minimal.json) (smallest valid file) and [`schemas/examples/target.services.json`](../../schemas/examples/target.services.json) (multi-service app).
- Source: section 18 of `docs/propuesta-arquitectura.md`.

## General rules

| Rule | Detail |
| --- | --- |
| Encoding | UTF-8 TOML 1.0. |
| Unknown keys | Rejected everywhere (`additionalProperties: false`). A typo such as `base_ulr` is a config error, not a silently ignored key. |
| Secrets | Never stored. Fields ending in `_env` hold the **name** of an environment variable and must match `^[A-Z_][A-Z0-9_]*$`. |
| Paths and globs | Repo-relative, forward slashes, [globset](https://docs.rs/globset) syntax (`**` crosses directories). Matched against paths as printed by `git diff --name-only`. |
| Defaults | A missing optional field takes the default below. A missing optional table behaves as if every field in it took its default. |
| Errors | Any schema violation makes `guard` exit with code `2` (see [`cli.md`](cli.md)). |

## Phase 1 consumers

`guard-cli` reads `[app]`, `[[services]]`, `[testability]`, `[areas]`, `[network]`, `[evidence]` and `[code_host]`, plus `[tracker].ticket_pattern` (used by `guard test --affected` / `guard affected`). `[hard_rules]`, `[server]` and the rest of `[tracker]` are parsed and validated but have no effect in Phase 1.

## Top level

| Key | Type | Required | Default | Notes |
| --- | --- | --- | --- | --- |
| `version` | integer | yes | — | Must be `1`. |
| `[app]` | table | yes | — | |
| `[[services]]` | array of tables | no | absent | Extra processes of a multi-service app. Absent (or empty) = single-process behavior, unchanged. |
| `[testability]` | table | no | all defaults | |
| `[areas.<key>]` | tables | yes, 1..255 | — | |
| `[hard_rules]` | table | no | all defaults | Phase 3. |
| `[network]` | table | no | all defaults | |
| `[evidence]` | table | no | all defaults | |
| `[tracker]` | table | no | absent | Phase 2 (except `ticket_pattern`). |
| `[code_host]` | table | no | absent | `guard init` always writes it when a git remote is detected. |
| `[server]` | table | no | absent | Phase 2+. |

## `[app]`

| Key | Type | Required | Default | Constraint / meaning |
| --- | --- | --- | --- | --- |
| `name` | string | yes | — | `^[a-z0-9][a-z0-9_-]*$`. |
| `languages` | string[] | no | `[]` | Lowercase names, unique (`typescript`, `go`, `rust`, `python`, `csharp`, `php`, `java`, ...). Impact analysis, Phase 2+: `typescript`, `javascript`, `python`, `go`, `rust`, `csharp` and `php` have a guard-impact plugin; any other name falls back to the `[areas]` globs. An empty list enables every plugin. |
| `start` | string | no | absent | Shell command run by `guard up` from the repo root. Absent: `guard up` only waits for readiness; absent together with `[[services]]` is [self mode](#self-mode-runner-self). |
| `stop` | string | no | absent | Shell command run by `guard down`. Absent: `guard down` does nothing and exits `0`. |
| `base_url` | string | yes | — | `http://` or `https://`, no trailing slash expected. Exported as `TARGET_BASE_URL`. |
| `ready_path` | string | no | `"/healthz"` | Starts with `/`. Readiness = `GET base_url + ready_path` returns `200`. |
| `preview_url` | string | no | absent | Template; `{pr}` is replaced with the PR number. Phase 2+. |

## `[[services]]`

Optional. One table per extra process of a multi-service app (an API, a worker, a second frontend), each started natively next to the infrastructure `[app].start` brings up. Written by `guard init` from [detection](detection.md); edited by hand afterwards.

| Key | Type | Required | Default | Constraint / meaning |
| --- | --- | --- | --- | --- |
| `name` | string | yes | — | `^[a-z][a-z0-9_-]*$`. Unique across `[[services]]`. Used as the file stem in `.guard/services/`. |
| `cwd` | string | no | `"."` | Repo-relative directory the commands run in: forward slashes, no leading or trailing slash, no segment starting with `.` (so `..` cannot escape the repo), `.` alone = repo root. Must exist (checked by `guard up` and `guard doctor`). |
| `start` | string | yes | — | Non-empty shell command, run detached by `guard up` from `cwd` through the platform shell, exactly like `[app].start`. |
| `stop` | string | no | absent | Non-empty shell command run in the foreground by `guard down` from `cwd` before the recorded process tree is terminated. |
| `ready_url` | string | no | absent | `http://` or `https://` URL. `guard up` waits for `GET ready_url` to return `200` before it starts the next service. Absent = do not wait for this service. |

### Lifecycle

| Step | `guard up` | `guard down` |
| --- | --- | --- |
| 1 | `[app].start` if set (typically the compose infrastructure), as today. | Each service in **reverse** file order: its `stop` if set, then terminate the PID recorded in `.guard/services/<name>.pid` (only when `.guard/services/<name>.identity` still matches, same rule as `app.pid`). |
| 2 | Each service in file order: detached in its own process group, output appended to `.guard/services/<name>.log`, PID in `.guard/services/<name>.pid` plus `.guard/services/<name>.identity`; then wait for its `ready_url` (if any). A service whose `ready_url` already returns `200`, or whose recorded PID is still the process an earlier `guard up` started, is not started again. | `[app]` as today (`[app].stop`, then `.guard/app.pid`). |
| 3 | Wait for `[app].base_url + ready_path`, as today. | |

Any failing `guard up` step (start exits non-zero before readiness, readiness timeout, missing `cwd`) is exit `3` with the last 20 lines of that service's log on stderr, and `guard up` terminates the process trees **this call** spawned (in reverse order) before exiting; `stop` commands are left to `guard down`. Details in [`cli.md`](cli.md).

Backward compatibility: a file without `[[services]]` (or with `services = []`) behaves byte for byte as before; nothing is written under `.guard/services/`.

## Self mode (runner `self`)

A file with no `[app].start` and no `[[services]]` entry is in **self mode**: the developer starts the app, guard only needs `base_url` and `ready_path`. `guard init --runner self` writes it (see [`cli.md`](cli.md#runner)). It is not a new key: the schema already allows both to be absent, so self mode needs no schema change and validates with `guard_core::config::parse_target` like any other file.

| Command | Self-mode behaviour |
| --- | --- |
| `guard up` | Never spawns anything. Already ready = unchanged; else `up: waiting for your app at <url> (start it yourself; guard does not start it)` and wait up to `--timeout`, then exit `3` with the self-mode hint ([`cli.md`](cli.md#self-mode-hint)). |
| `guard down` | Nothing guard started in self mode, so normally nothing to stop: `down: nothing to stop: guard did not start the app`, exit `0`. Leftovers are still cleaned up as in [`cli.md`](cli.md#guard-up--guard-down): an `[app].stop` the file sets still runs, and a `.guard/app.pid` (or service PID) recorded by an earlier runner-`guard` `guard up` is still terminated (identity-checked), so switching to self mode never orphans a process guard started; either one prints `down: stopped`. |
| `guard test` | App not ready before Playwright = exit `3` with the same hint. |

`guard init --runner self` keeps what detection found as a **commented block** directly after the last `[app]` key, so uncommenting the lines below its two header lines (removing the leading `# `; the header lines stay comments) turns the file into the runner-`guard` shape of [`detection.md`](detection.md) without moving any line: `start`/`stop` land in `[app]`, the `[[services]]` tables follow `[app]` and precede `[areas]`. The two header lines are fixed text; the block ends at the first line that does not start with `#`. Service `ready_url` values are the ones confirmed during `init`. A block line left commented is ignored by every parser; `guard up` and `guard test` read it only to print the hint.

```toml
version = 1

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

[areas.backend]
description = "TODO: describe backend"
paths = ["backend/**"]

[areas.frontend]
description = "TODO: describe frontend"
paths = ["frontend/**"]
```

For a single-service repo (the single-`[app]` shape) the block holds only `# start = ...` (and `# stop = ...` when detected). CI cannot use a self-mode file as it is: enable the block first (`guard init --ci` with `--runner self` warns about it).

## `[testability]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `login_path` | string | `"/__test__/login"` | Starts with `/`. Session per role. |
| `seed_path` | string | `"/__test__/seed"` | Starts with `/`. Namespaced test data. |
| `token_env` | string | `"GUARD_TEST_TOKEN"` | Env var name holding the test token. guard forwards that variable to Playwright under the same name (see [`runner-env.md`](runner-env.md)). |
| `roles` | string[] | `[]` | `^[a-z][a-z0-9_-]*$`, unique. Exported as `GUARD_ROLES`; one `.auth/<role>.json` each. Empty = no auth setup. |
| `test_id_attribute` | string | `"data-testid"` | HTML attribute used by `getByTestId`. |

## `[areas.<key>]`

`<key>` must match `^[a-z][a-z0-9_]*$` (for example `checkout`, `ui_general`). At least one area, at most 255.

| Key | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `description` | string | yes | — | Non-empty. The plain-language criterion for the area. |
| `paths` | string[] | yes | — | At least one glob, unique. |
| `sensitive` | bool | no | `false` | Sensitive areas always go to the human lane (Phase 3). |

## `[hard_rules]` (Phase 3)

| Key | Type | Default |
| --- | --- | --- |
| `human_only_paths` | string[] (globs) | `[]` |
| `max_fan_out_for_agent` | integer >= 1 | `10` |

## `[network]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `blocked_domains` | string[] | `[]` | Host patterns `^(\*\.)?[a-z0-9-]+(\.[a-z0-9-]+)+$`. `*.example.com` matches `example.com` and any subdomain. Exported as `GUARD_BLOCKED_DOMAINS`; aborted in every test. |
| `recorded_hosts` | string[] | `[]` | Hosts served from HAR files. Parsed in Phase 1, not yet enforced. |

## `[evidence]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `video` | `"on-first-retry"` \| `"off"` | `"on-first-retry"` | `"off"` is exported as `GUARD_VIDEO=off`. |
| `retention_days` | integer >= 1 | `14` | Phase 2+ (remote storage). |
| `storage_url_env` | string | `"GUARD_STORAGE_URL"` | Env var name. Phase 2+. |

## `[tracker]`

| Key | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `kind` | `"jira"` | yes (if table present) | — | |
| `project` | string | no | absent | `^[A-Z][A-Z0-9_]*$`. Phase 2. |
| `ticket_pattern` | string | no | absent | Regex in Rust `regex` syntax. The first match in the current branch name is the ticket key, which adds `@ticket:<KEY>` to `--affected`. In TOML use a literal string: `'SHOP-\d+'`. |
| `require_ticket` | `"warn"` \| `"block"` | no | `"warn"` | Phase 2. |

## `[code_host]`

| Key | Type | Required | Meaning |
| --- | --- | --- | --- |
| `kind` | `"github"` \| `"gitlab"` | yes (if table present) | Detected by `guard init` from the `origin` remote (`github.com` -> `github`, `gitlab` in the host -> `gitlab`). |

## `[server]` (Phase 2+)

| Key | Type | Required |
| --- | --- | --- |
| `url` | string (`http(s)://...`) | yes (if table present) |

## Semantic rules checked by `guard doctor` (beyond the schema)

| Rule | Severity |
| --- | --- |
| Every `area` in `e2e-playwright/catalog/journeys.json` is a key of `[areas]`. | error |
| Journey ids in the catalog are unique. | error |
| Every glob in `[areas.*].paths` (and catalog `paths`) matches at least one tracked file. A dead glob protects nothing. | warning |
| A file matched by two areas. If one is sensitive, the sensitive one wins. | warning |
| `ticket_pattern` compiles as a Rust regex. | error |
| `[[services]].name` values are unique. | error |
| Every `[[services]].cwd` is an existing directory. | error |
| No value looks like a secret (fields are env var names only; enforced by the schema pattern). | error |

## Full example

```toml
# target.toml, at the root of the app repo
version = 1

[app]
name = "shop"
languages = ["typescript", "go"]
start = "docker compose up -d --wait"
stop = "docker compose down -v"
base_url = "http://localhost:3000"
ready_path = "/healthz"
preview_url = "https://pr-{pr}.preview.example.com"

[testability]
login_path = "/__test__/login"
seed_path = "/__test__/seed"
token_env = "GUARD_TEST_TOKEN"
roles = ["customer", "admin"]
test_id_attribute = "data-testid"

[areas.checkout]
description = "Cart, payment and order confirmation"
paths = ["web/src/checkout/**", "services/orders/**", "services/payments/**"]
sensitive = true

[areas.inventory]
description = "Stock levels, reservations and restock"
paths = ["services/inventory/**"]

[areas.ui_general]
description = "Shared UI components and copy"
paths = ["web/src/components/**"]

[hard_rules]
human_only_paths = ["**/migrations/**", "infra/**"]
max_fan_out_for_agent = 10

[network]
blocked_domains = ["*.google-analytics.com", "*.intercom.io"]
recorded_hosts = ["api.payments-sandbox.example.com"]

[evidence]
video = "on-first-retry"
retention_days = 14
storage_url_env = "GUARD_STORAGE_URL"

[tracker]
kind = "jira"
project = "SHOP"
ticket_pattern = 'SHOP-\d+'
require_ticket = "warn"

[code_host]
kind = "github"

[server]
url = "https://guard.example.com"
```

## Multi-service example

Infrastructure from the root compose file, three services run natively (JSON form: [`target.services.json`](../../schemas/examples/target.services.json)).

```toml
version = 1

[app]
name = "saunas"
languages = ["php", "typescript"]
start = "docker compose -f docker-compose.yml up -d --wait postgres redis mailpit"
stop = "docker compose -f docker-compose.yml down"
base_url = "http://localhost:3000"
ready_path = "/"

[[services]]
name = "backend"
cwd = "backend"
start = "bun run dev"
ready_url = "http://localhost:3001/health"

[[services]]
name = "billing"
cwd = "billing"
start = "php artisan serve --port 8000"
ready_url = "http://localhost:8000/"

[[services]]
name = "frontend"
cwd = "frontend"
start = "bun run dev"
stop = "bun run clean"
ready_url = "http://localhost:3000/"

[areas.backend]
description = "TODO: describe backend"
paths = ["backend/**"]

[areas.billing]
description = "TODO: describe billing"
paths = ["billing/**"]

[areas.frontend]
description = "TODO: describe frontend"
paths = ["frontend/**"]
```

## Minimal example (the demo app)

```toml
version = 1

[app]
name = "demo-shop"
start = "node examples/demo-shop/server.mjs"
base_url = "http://localhost:3000"

[testability]
roles = ["customer"]

[areas.checkout]
description = "Cart, payment and order confirmation"
paths = ["examples/demo-shop/**"]
```
