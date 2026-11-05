---
name: guard-e2e-authoring
description: Use when writing or changing a Playwright end-to-end test, journey, Page Object or fixture in this repository, so it follows the guard conventions and passes the deterministic test-quality checks.
---

# Writing guard end-to-end tests

## Layout

| Path | What lives there |
| --- | --- |
| `src/pages/` | One Page Object per screen: locators in the constructor, intent-named actions and `expect...` helpers |
| `src/fixtures/` | `test.extend` fixtures: Page Objects, namespaced seed data (`data`), auth |
| `tests/journeys/<area>/` | Catalogued journeys, one `@journey:<id>` each (owned by QA via CODEOWNERS) |
| `tests/smoke/` | The vital-signs suite (`@smoke`), never quarantined |
| `tests/features/<KEY>.spec.ts` | The spec for one ticket, written red-first by the QA agent |

## Shape of a test

```ts
import { test, expect } from '../../src/fixtures';

test.describe('Catalog: out-of-stock badge', { tag: ['@ticket:SHOP-101'] }, () => {
  test('a product with stock 0 shows the badge and cannot be added', async ({ page, data, catalog }) => {
    const product = await data.product({ stock: 0 });
    await page.goto(`/products/${product.slug}`);
    await expect(catalog.stockBadge).toHaveText('Out of stock');
    await expect(catalog.addToCart).toBeDisabled();
  });
});
```

1. Arrange with fixtures: create the records the test needs through `data` (seed endpoint, namespaced per run/test/retry). Never depend on records another test created.
2. Act through Page Object methods named after user intent (`checkout.payWithCard(card)`), not raw clicks spread across the test.
3. Assert observable behaviour with web-first assertions (`await expect(locator).toBeVisible()`, `toHaveText`, `toHaveURL`). Each test asserts what its title promises.

## Locators

- Prefer `getByRole(role, { name })`, `getByLabel`, `getByText`; fall back to `getByTestId` (`data-testid`).
- Never CSS classes, XPath, `nth-child` or layout-dependent selectors.
- Put locators in the Page Object; tests read like the acceptance criteria.

## Tags

- Ticket specs: `@ticket:<KEY>` on every test (title or `{ tag }` option). The tag must match whole: `@ticket:SHOP-12` never selects `SHOP-123`.
- Journeys: exactly one `@journey:<id>` from the catalog, optionally `@area:<key>`.
- Only `@ticket:<KEY>`, `@journey:<id>`, `@area:<key>` and `@smoke` formats are valid.

## Rejected before any review (deterministic checks)

| Check | Avoid |
| --- | --- |
| `no_focus` | `test.only`, `describe.only`, `it.only`, `fit`, `fdescribe` |
| `no_skip` | `test.skip`, `test.fixme`, `describe.skip`, `xit`, conditional `test.skip(cond)` |
| `no_empty_test` | A test whose body has no statements |
| `has_expect` | A test without any `expect(...)` (including inside `test.step`) |
| `no_trivial_expect` | `expect(<literal>)` such as `expect(true).toBe(true)` |
| `no_wait_for_timeout` | `page.waitForTimeout(...)`: wait for a state with an assertion instead |
| `no_self_mock` | `page.route` / `context.route` mocking the app's own API (`/api/`, the app base URL) |
| `no_swallowed_assertions` | `try { expect(...) } catch {}` without rethrow |
| `no_host_access` | Importing Node built-ins (`node:*`, `fs`, `child_process`, ...), `process`, `eval` or `new Function`: drive the app through Playwright only |
| `ticket_tag`, `journey_tag_format` | Missing or malformed tags |

## Determinism

- No sleeps, no reliance on wall-clock time, randomness or ordering between tests; the fixtures seed `Math.random` and record the seed.
- Each test cleans up through its fixture; shards run in parallel against the same environment.

## Run it

- `guard test` (or the `guard_test` tool) runs the affected journeys plus `@smoke`; `guard_failure` explains a failure with its trace.
- Changing a flow on purpose means updating its journey in the same PR; journey changes are reviewed by QA.
