---
name: guard-qa-red-first
description: Use when acting as the QA agent of lane A, writing the red-first spec tests/features/<KEY>.spec.ts for a ticket from its acceptance criteria before any code changes.
---

# QA agent: the red-first spec

Your output is one file: `<tests_dir>/features/<KEY>.spec.ts` (`allowed_paths.write` in `/task/task.json`). Everything else in `/workspace` is read-only, and the orchestrator rejects a diff that touches any other path (`diff_rejected`).

## What happens to your spec

1. The orchestrator extracts your diff and checks the path.
2. Deterministic test-quality checks run on the spec's syntax tree (see `guard-e2e-authoring`). Any failure is `quality_rejected`.
3. **Red-first:** only your spec runs on a clean checkout of `base_commit`. It must **fail on an assertion**. A spec that passes proves nothing new (`red_first_passed`); a syntax error, a compile error or an app-start timeout is not a valid red either.
4. The spec is **sealed**: its SHA-256 is recorded and the bytes are stored outside every sandbox. From then on nobody can change it, including you. The Coder agent writes the app code against that exact file, without seeing the journeys.

So the spec is the contract for the fix. Write it carefully once.

## How to write it

1. Read `summary` and `acceptance_criteria` in `/task/task.json`. Each criterion becomes at least one assertion; if a criterion cannot be observed through the UI, say so in `result.json` instead of inventing one.
2. Explore the preview environment with Playwright MCP to find the screens, roles and labels involved. Reuse Page Objects from `src/pages` and fixtures from `src/fixtures`; you may only write the spec file, so express missing helpers inline in the spec with role-based locators.
3. Tag every test `@ticket:<KEY>`.
4. Create data through the `data` fixture (seed endpoint). Never assert on records you did not create.
5. Assert the behaviour the ticket asks for, which the current code does **not** have. A good red-first spec fails today for the reason the ticket describes ("expected 'Out of stock', received ''"), not because of a typo.
6. Do not mock the app's own API with `page.route`, do not swallow assertion errors, no `waitForTimeout`, no `.only`/`.skip`.

## Finish

Write `/task/result.json` with `status: "completed"` and a summary listing the criteria you covered. Use `status: "refused"` with an `error` when the criteria are too vague to test; that sends the ticket to a human instead of producing a weak spec. Durable findings (a flaky area, a missing test id) go in `memory_proposals`; you cannot write memory directly.
