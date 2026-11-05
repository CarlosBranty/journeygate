# AGENTS.md — writing Playwright tests in this repo

Read this before you add or change a test. Tests that ignore these rules may
pass today, but they bypass the architecture, and `catalog-lint`, ESLint or
review will reject them.

## Where things live

| Path | What goes there |
| --- | --- |
| `src/pages/` | Page Objects. The only place that knows locators. |
| `src/flows/` | Multi-page user flows built from Page Objects (e.g. `buyWithCard`). |
| `src/fixtures/` | `test` and `expect`. Always import them from here, never from `@playwright/test`. |
| `src/data/` | `TestData`: seeds data through `POST /__test__/seed` inside the test's namespace. |
| `tests/journeys/<area>/<id-without-area>.spec.ts` | One catalog journey per file. |
| `tests/features/**` | Specs written for a ticket. Every one needs `@ticket:<KEY>`. |
| `tests/smoke/` | Cheap "is it up" checks tagged `@smoke`. |
| `tests/setup/` | Setup project (API login per role). Do not add tests here. |
| `catalog/journeys.json` | Journey catalog; the source of truth for routing. |

## Rules

1. **Locators** — use `getByRole`, `getByLabel`, `getByText` or `getByTestId`,
   and only inside `src/pages/`. Never use CSS or XPath selectors
   (`page.locator('.btn')`). ESLint `playwright/no-raw-locators` fails the build.
2. **No arbitrary waits** — never `waitForTimeout`, `waitForSelector` or
   `networkidle`. Use web-first assertions (`await expect(locator).toBeVisible()`)
   or `expect.poll`.
3. **No skipped or focused tests** — `test.skip`, `test.fixme` and `test.only`
   are banned. A broken journey is quarantined in the catalog
   (`"quarantined": true`), not skipped in code.
4. **Tags** — a journey test's `test.describe` carries exactly
   `@journey:<id>` and `@area:<area>` from the catalog, plus `@smoke` if and
   only if the catalog says `"smoke": true`. Ticket specs carry `@ticket:<KEY>`.
   Run `node scripts/catalog-lint.ts --target ../target.toml` after any change.
5. **Data** — create everything you need with the `data` fixture
   (`await data.product({ stock: 3 })`). Never depend on data another test
   created, never hard-code ids or slugs. The namespace
   (`RUN_ID-testId-x<repeat>-r<retry>`) is deleted after each test.
6. **Sessions** — log in with `test.use({ storageState: authStatePath('<role>') })`.
   Never log in through the UI unless the login itself is the journey.
7. **Determinism** — do not override the timezone, locale, viewport or colour
   scheme. `Math.random` is seeded per test; if a test depends on the date use
   `page.clock.setFixedTime()`. Third-party domains from `target.toml` are
   blocked; do not unblock them.
8. **Independence** — `fullyParallel` is on. Each test must pass alone, in any
   order, on any shard.
9. **Assertions mean the business outcome** — assert what the user and the
   system end up with (one confirmed order, the cart unchanged), not that a
   click happened. Never weaken an assertion to make a test pass.

## Healer rule

If a journey fails, assume the app broke until proven otherwise. You may
propose a fix in a PR; you may not loosen assertions in `tests/journeys/**` or
`tests/smoke/**`. Those changes are approved by QA through CODEOWNERS.

## Commands

```sh
pnpm typecheck                                   # tsc --noEmit
pnpm test:unit                                   # vitest: pure helpers and catalog-lint
pnpm lint                                        # eslint with eslint-plugin-playwright
npx playwright test --list                       # proves the suite parses
node scripts/catalog-lint.ts --target ../target.toml
guard test --journey <id>                        # run against the app (sets the env contract)
```

The runner environment (`E2E_TAGS`, `GUARD_SHARD_PLAN`, `RUN_ID`, ...) is
defined in `docs/contracts/runner-env.md` and read only by `playwright.config.ts`.
