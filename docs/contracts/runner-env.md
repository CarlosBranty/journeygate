# Contract: guard (Rust) <-> Playwright (TypeScript) runner environment

`guard` never builds regexes or Playwright config. It launches `npx playwright test` (or `npx playwright merge-reports`) with the working directory set to the Playwright project and communicates **only** through the environment variables, files and exit codes below. `e2e-playwright/playwright.config.ts` is the single reader of these variables.

Source: sections 3, 9, 10 and 12 of `docs/propuesta-arquitectura.md`.

## Process

| Item | Value |
| --- | --- |
| Playwright project dir | `<repo root>/e2e-playwright` (override: `guard --e2e-dir <path>`). All relative paths below are relative to this dir. |
| Command | `npx playwright test [--headed \| --ui \| --debug] [--shard=<i>/<n>]` |
| Environment | guard's own environment is inherited, then the variables below are set (overriding inherited values). |
| Path separator in values | Forward slashes, even on Windows. |
| Minimum Playwright | `@playwright/test` 1.46 (tags in `test.describe` details, blob `fileName`). Exact pin is owned by `e2e-playwright/package.json`. |

## Environment variables

| Variable | Set by guard when | Format | Unset / empty means | Playwright side behaviour |
| --- | --- | --- | --- | --- |
| `E2E_TAGS` | always (`guard test`, `guard gate shard`) | Comma-separated tags, no spaces, each starting with `@`. Example: `@journey:checkout.customer-card,@smoke,@ticket:SHOP-123` | Run every test (no filter). Only happens when Playwright is run by hand. | `grep` on the non-setup projects = one RegExp per tag, `escapeRe(tag) + '(?=\\s\|$)'`, so each must match a **whole** tag (`@ticket:SHOP-12` never matches `@ticket:SHOP-123`). A test runs if it matches any tag (OR). The `setup` project is never filtered. |
| `GUARD_SHARD_PLAN` | `guard gate shard` only | Absolute path to a JSON file `{"files": ["**/<relative path>", ...]}`; each `<relative path>` is relative to the Playwright project dir, e.g. `**/tests/journeys/checkout/customer-card.spec.ts`. | Use the default `testMatch` (`**/*.spec.ts`). | `testMatch = plan.files` for the non-setup projects (testMatch globs are matched against absolute paths, hence the `**/` prefix). Tag filtering from `E2E_TAGS` still applies on top. guard never launches Playwright with an empty `files` list. |
| `TARGET_BASE_URL` | always | `[app].base_url` from `target.toml`, e.g. `http://localhost:3000`. In `--docker` mode, `localhost`/`127.0.0.1` are rewritten to `host.docker.internal`. | Default `http://localhost:3000`. | `use.baseURL`. |
| `RUN_ID` | always | `^[A-Za-z0-9_.-]{1,64}$`. If `RUN_ID` is already in guard's environment it is forwarded unchanged; otherwise guard generates `r<unix-seconds>-<pid>`, e.g. `r1793452800-4242`. | Treat as `local`. | Prefix of the per-test data namespace: `${RUN_ID}-${testId}-x${repeatEachIndex}-r${retry}`. |
| `GUARD_TEST_TOKEN` | always when the variable named by `[testability].token_env` is set | Opaque string. guard reads the variable **named** by `token_env` (default `GUARD_TEST_TOKEN`) and always exports its value under the fixed name `GUARD_TEST_TOKEN`. | Login and seed calls send an empty token and the app answers `401`. | Sent as header `x-test-token` on `POST <GUARD_LOGIN_PATH>` and on every seed call. |
| `GUARD_LOGIN_PATH` | always | `[testability].login_path` from `target.toml` (default `/__test__/login`). Starts with `/`. | Default `/__test__/login`. | `auth.setup.ts` posts the per-role login to `GUARD_LOGIN_PATH`. |
| `GUARD_SEED_PATH` | always | `[testability].seed_path` from `target.toml` (default `/__test__/seed`). Starts with `/`. | Default `/__test__/seed`. | The `data` fixture posts `{namespace, ...input}` to `GUARD_SEED_PATH` and deletes `GUARD_SEED_PATH/<namespace>` after the test. |
| `GUARD_VIDEO` | always | `off` or `on-first-retry` (from `[evidence].video`). | `on-first-retry`. | `use.video = GUARD_VIDEO === 'off' ? 'off' : 'on-first-retry'`. |
| `GUARD_ROLES` | always | Comma-separated roles from `[testability].roles`, e.g. `customer,admin`. Empty string when no roles. | No roles: `auth.setup.ts` creates no storage state. | `auth.setup.ts` creates one setup test per role and writes `.auth/<role>.json`. |
| `GUARD_BLOCKED_DOMAINS` | always | Comma-separated host patterns from `[network].blocked_domains`, e.g. `*.google-analytics.com,*.intercom.io`. Empty string when none. | Block nothing. | Every request whose URL host matches a pattern is aborted (`route.abort('blockedbyclient')`) in every test. `*.example.com` matches `example.com` and any subdomain; a pattern without `*.` matches that exact host only. Matching is case-insensitive. |
| `GUARD_SHARD_INDEX` | `guard gate shard` only | 1-based integer, e.g. `3`. | Not a gate shard. | Reporter switches to blob (see below) with `fileName: report-<GUARD_SHARD_INDEX>.zip`. |
| `CI` | `guard gate shard` sets `CI=1`; `guard test` forwards whatever it inherited | any non-empty value = true | Local run. | `retries = CI ? 2 : 0`, `forbidOnly = !!CI`, `workers = CI ? 4 : undefined`. |

`GUARD_LOGIN_PATH` and `GUARD_SEED_PATH` let a scaffold honor custom `[testability]` paths without reading `target.toml`. A Playwright project run by hand (no guard) falls back to the defaults, so older scaffolds that hard-code `/__test__/login` keep working against apps that use the default paths.

Values never contain commas inside a single tag, role or domain, so splitting on `,` is always safe. Split results that are empty strings are ignored.

## Output files

| Path | Produced when | Format / consumer |
| --- | --- | --- |
| `results/report.json` | Every run without `GUARD_SHARD_INDEX` (local `guard test`), and by `guard gate finalize` via `merge-reports` | Playwright JSON reporter. Read by guard to print the summary and by `finalize` to build the verdict. |
| `playwright-report/` | Every run without `GUARD_SHARD_INDEX` (`html` reporter, `open: 'never'`), and by `finalize` | Playwright HTML report. Opened by `guard report`. |
| `blob-report/report-<i>.zip` | Runs with `GUARD_SHARD_INDEX=<i>` (`blob` reporter + `list`) | Uploaded by CI as an artifact and merged by `guard gate finalize --blobs <dir>`. |
| `test-results/` | Always | Playwright artifacts (screenshots, traces, videos). |
| `.auth/<role>.json` | Setup project | storageState per role. Git-ignored. |

Reporter matrix that `playwright.config.ts` must implement:

| Condition | Reporters |
| --- | --- |
| `GUARD_SHARD_INDEX` set | `[['blob', { fileName: 'report-<i>.zip' }], ['list']]` |
| otherwise | `[['json', { outputFile: 'results/report.json' }], ['html', { open: 'never' }], ['list']]` |

### Fields guard reads from the JSON report

guard parses only these fields and ignores everything else:

- `suites[]` (recursive through `suites[].suites[]`) -> `specs[]` with `title`, `file` (relative to `testDir`; `playwright.config.ts` sets `testDir` to the Playwright project dir, so this is `tests/...`, the same path used by shard plans and `.guard/durations.json`), `line`, `tags`, `tests[]`.
- `tests[]` with `projectName`, `status` (`expected` \| `unexpected` \| `flaky` \| `skipped`), `results[]` with `duration` (ms), `retry`, `status`, `error.message`.
- top-level `errors[]` (config or global errors) and `stats`.

Tags in the JSON report may appear with or without the leading `@` depending on the Playwright version; guard normalises them to the `@` form.

## Exit codes of `npx playwright test`, as interpreted by guard

| Playwright exit | Report present and parseable | guard interpretation |
| --- | --- | --- |
| `0` | yes | All tests passed (flaky tests also exit `0`; guard reads statuses from the report). |
| `1` | yes, with at least one test and no top-level `errors[]` | Test failures. |
| `1` | missing, unparseable, zero tests, or top-level `errors[]` non-empty | Infrastructure error (config error, no tests found, global setup crash). |
| `130` or terminated by signal | any | Interrupted: infrastructure error. |
| any other | any | Infrastructure error. |

For gate shards "report present" means `blob-report/report-<i>.zip` exists. How these map to guard's own exit codes is defined in [`cli.md`](cli.md).

## Example

```sh
# What guard test --journey checkout.customer-card runs
cd e2e-playwright
E2E_TAGS='@journey:checkout.customer-card' \
TARGET_BASE_URL='http://localhost:3000' \
RUN_ID='r1793452800-4242' \
GUARD_TEST_TOKEN='dev-token' \
GUARD_LOGIN_PATH='/__test__/login' \
GUARD_SEED_PATH='/__test__/seed' \
GUARD_VIDEO='on-first-retry' \
GUARD_ROLES='customer' \
GUARD_BLOCKED_DOMAINS='' \
npx playwright test

# What guard gate shard --index 2 --plan plan/ runs
CI=1 GUARD_SHARD_INDEX=2 GUARD_SHARD_PLAN="$PWD/../plan/shard-2.json" \
E2E_TAGS='@journey:checkout.customer-card,@journey:billing.invoice-pdf,@smoke' \
TARGET_BASE_URL='http://localhost:3000' RUN_ID='12345-1' GUARD_TEST_TOKEN='dev-token' \
GUARD_LOGIN_PATH='/__test__/login' GUARD_SEED_PATH='/__test__/seed' \
GUARD_VIDEO='on-first-retry' GUARD_ROLES='customer' GUARD_BLOCKED_DOMAINS='' \
npx playwright test
```
