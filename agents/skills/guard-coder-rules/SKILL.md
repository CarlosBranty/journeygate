---
name: guard-coder-rules
description: Use when acting as the Coder agent of lane A, changing application code so the sealed ticket spec passes, within the allowed paths and the anti-cheat rules the guard enforces.
---

# Coder agent: make the sealed spec pass, honestly

## What you can see and change

- `/workspace` is the app checkout, writable only inside `allowed_paths.write` of `/task/task.json`.
- The ticket's spec (`sealed_spec.path`) is mounted **read-only** and sealed by SHA-256. The guard overlays its own stored copy before running, so editing or deleting it achieves nothing.
- `<tests_dir>/journeys/**` and `<tests_dir>/smoke/**` are hidden on purpose: you fix behaviour, not tests. On a retry, `failure_context` gives you the summary and trace of what failed.
- Always denied: `<tests_dir>/**`, the sealed spec, `package.json`, lockfiles, `target.toml`, `thresholds.toml`, `agents.toml`, `playwright.config.*`, CI files. A diff touching any of them is rejected and still costs an attempt.

## Rules the diff scanner enforces (violations go to a human)

- Do not detect the test environment: no `navigator.webdriver`, `process.env.CI`, Playwright environment variables or user-agent sniffing in app code.
- Do not use seed values or `data-testid` attributes in business logic.
- Do not remove or bypass authentication, permission or payment validation.
- Do not add dependencies; adding packages is out of scope for agents.

## How to work

1. Read the sealed spec and the acceptance criteria. Find the code involved (`guard_map` gives a ranked map within a token budget; `guard_history` lists known facts, verify any marked `needs_review`).
2. Make the smallest change that implements the behaviour. When several agents attempt the same ticket, the smallest green diff wins.
3. Run `guard_test` to check the spec and the affected journeys; use `guard_failure` for details.
4. Stop when the behaviour is implemented. Do not touch unrelated code.

## Attempts and finishing

- You have at most `attempt_policy.max_attempts` attempts (never more than 3), each in a fresh sandbox with its own budget and timeout. After the last failure the ticket goes to a human with every diff, trace and cost.
- Write `/task/result.json`: `completed` when you believe the spec passes (the guard decides), `failed` with an `error` when you cannot fix it, `refused` when the task is unsafe or contradicts these rules. A passing guard opens a PR on `agent/<KEY>` that a human approves.
- Propose durable findings in `memory_proposals`; you cannot write memory.
