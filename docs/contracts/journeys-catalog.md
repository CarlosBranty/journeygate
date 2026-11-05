# Contract: journey catalog and tags

`e2e-playwright/catalog/journeys.json` is the source of truth for test routing. `guard` (Rust) reads it to choose which tags to pass to Playwright; Playwright specs carry the matching tags; `scripts/catalog-lint.ts` keeps both sides 1:1.

- Machine-readable schema: [`schemas/journeys.schema.json`](../../schemas/journeys.schema.json).
- Valid example: [`schemas/examples/journeys.example.json`](../../schemas/examples/journeys.example.json).
- Source: section 3 of `docs/propuesta-arquitectura.md`.

## File format

```json
{
  "version": 1,
  "journeys": [
    {
      "id": "checkout.customer-card",
      "area": "checkout",
      "title": "Customer pays by card and gets one confirmed order",
      "impact_question": "This diff can directly or indirectly change how a signed-in customer adds a product to the cart, pays by card and receives an order confirmation.",
      "paths": ["web/src/checkout/**", "services/orders/**", "services/payments/**"],
      "smoke": true,
      "quarantined": false
    }
  ]
}
```

| Field | Type | Required | Constraint / meaning |
| --- | --- | --- | --- |
| `version` | integer | yes | Must be `1`. |
| `journeys` | array | yes | May be empty. Order is not significant. |
| `journeys[].id` | string | yes | `^[a-z0-9]+(\.[a-z0-9-]+)+$`: at least two dot-separated segments, lowercase, digits and `-` (no `_`). Unique in the file. Convention: first segment = area key when the area key has no `_`. |
| `journeys[].area` | string | yes | Must be a key of `[areas.<key>]` in `target.toml` (checked by `guard doctor`, not by the schema). |
| `journeys[].title` | string | yes | Non-empty, human-readable. |
| `journeys[].impact_question` | string | yes | Non-empty. Written as a statement ("This diff can ..."), because the engine returns the probability that it is true. Unused in Phase 1. |
| `journeys[].paths` | string[] | yes | Repo-relative globs of the **app** repo (globset syntax). May be empty: the journey is then selected only by `@smoke`, `--all` or `--journey`. Paths inside git submodules are written from the superproject root (`frontend/src/**`); see cli.md "Changed files". |
| `journeys[].smoke` | bool | yes | `true` = the journey's spec also carries `@smoke` and runs in every `--affected` run. |
| `journeys[].quarantined` | bool | yes | `true` = excluded from `--affected`, `--all` and the gate; still runnable with `--journey <id>`. |

An optional top-level `"$schema"` string is allowed so editors can autocomplete. No other keys are allowed.

"Active journey" means `quarantined == false`.

## Tag convention

Tags are Playwright test tags (`test.describe(title, { tag: [...] }, ...)` or `test(title, { tag: [...] }, ...)`). Every tag starts with `@`.

| Tag | Cardinality | Meaning |
| --- | --- | --- |
| `@journey:<id>` | exactly one per journey test | `<id>` is the catalog id, verbatim. |
| `@area:<area>` | exactly one per journey test | `<area>` equals the catalog `area` of that journey. |
| `@smoke` | iff catalog `smoke == true` | Runs in every `--affected` selection. |
| `@ticket:<KEY>` | one or more on QA-agent specs (`tests/features/**`) | `<KEY>` is the tracker key, for example `@ticket:SHOP-123`. |

Rules enforced by `scripts/catalog-lint.ts` over `npx playwright test --list --reporter=json`:

1. Every active catalog journey has at least one test tagged `@journey:<id>`.
2. Every `@journey:<id>` tag references an id present in the catalog.
3. A test has at most one `@journey:` tag, and its `@area:` tag equals the catalog area.
4. `@smoke` appears on a journey test iff its catalog entry has `smoke: true`.
5. Every spec under `tests/features/**` has at least one `@ticket:<KEY>` tag.

## File layout convention

One journey per spec file, so balancing files across shards is as fine as balancing tests:

```text
e2e-playwright/tests/journeys/<area>/<id-without-area-prefix>.spec.ts
# checkout.customer-card -> tests/journeys/checkout/customer-card.spec.ts
```

## Tag matching

Selection is passed to Playwright as `E2E_TAGS` (see [`runner-env.md`](runner-env.md)). Each selected tag must match a **whole** tag, so `@ticket:SHOP-12` never matches `@ticket:SHOP-123` and `@journey:checkout.card` never matches `@journey:checkout.card-3ds`.
