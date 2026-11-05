# Agent conventions (guard)

These rules apply to every assistant and every autonomous agent working in this repository: Claude Code, Codex, Copilot, mini-swe-agent, OpenHands. This file is the single source; `guard agents sync` renders it for each runtime. Edit `agents/AGENTS.md`, never a rendered copy.

## How the guard works

- The guard decides whether a change is safe by running the affected journeys, the ticket's spec and the `@smoke` suite on a clean checkout. Its verdict cannot be overridden from a PR, a prompt or a tool.
- Autonomous agents run in sandboxes. Permissions come from mounts and network rules, not from these instructions: a read-only path stays read-only whatever you try. Do not look for ways around a restriction; report it instead.
- The only model endpoint is the gateway. Never print, log or write credentials, and never ask for them.

## Writing end-to-end tests

- Use the Page Objects in `src/pages` and the fixtures in `src/fixtures` (`import { test, expect } from '<relative>/src/fixtures'`). Add a Page Object method rather than repeating locators in a test.
- Locators: accessible roles, labels and text first (`getByRole`, `getByLabel`, `getByText`), then `getByTestId`. Never CSS classes, XPath or DOM structure.
- Data: create it per test through the seed fixture (`POST /__test__/seed`, namespaced per run, test and retry). Never reuse shared records or depend on test order. Log in with `storageState` per role, not through the UI, unless the login UI is what the test proves.
- Tags: every journey test has exactly one `@journey:<id>` from the catalog; specs written for a ticket carry `@ticket:<KEY>` on every test. `@smoke` is reserved for the vital-signs suite.
- Never use `waitForTimeout`, `test.only`, `test.skip`/`test.fixme`, trivial assertions (`expect(true).toBe(true)`), `try/catch` around assertions, or `page.route` mocks of the app's own API. The guard rejects them before any model looks at the test.

## Changing code

- Keep diffs small and on topic. Do not reformat unrelated code.
- Never touch lockfiles, `package.json`, `target.toml`, `thresholds.toml`, `agents.toml`, `playwright.config.*` or CI files unless the ticket is explicitly about them and a human asked for it. Adding dependencies is out of scope for agents.
- Never make application behaviour depend on being under test (`navigator.webdriver`, `process.env.CI`, Playwright variables, user-agent sniffing), and never use seed values or `data-testid` in business logic.
- Never weaken authentication, permission or payment validation to make a test pass.

## Tools (`guard mcp`)

- `guard_affected`: journeys affected by the current diff and why.
- `guard_test`: run the affected journeys (or a selection) and get a structured result.
- `guard_failure`: one failure in detail (error, steps, screenshot, trace link).
- `guard_history`: read-only facts the orchestrator recorded about this repo (flaky tests, past fixes). Items marked `needs_review` may be stale: verify them.
- `guard_map`: a ranked map of the code relevant to a question, within a token budget.

None of these tools can update snapshots, edit journeys or the catalog, quarantine tests or change the guard verdict. That is by design.

## Memory

You cannot write the orchestrator's memory. If you learn something durable and evidence-backed (a root cause, a flaky pattern), propose it in `result.json` `memory_proposals`; a human approves or rejects it.
