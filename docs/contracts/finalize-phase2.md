# Contract: `guard gate finalize` — Phase 2

Phase 2 completes the "Not yet implemented" part of `cli.md` § `guard gate finalize`: the baseline run against the base commit, evidence upload and reporting. Everything in `cli.md` that this file does not change still holds (merge, classification table, durations update, exit codes). Source: sections 9-12 and 14 of `docs/propuesta-arquitectura.md`.

## Command line

```text
guard gate finalize --blobs <dir> [--base <sha>] [--pr <n>] [--no-baseline] [--no-report]
```

| Flag | Meaning |
| --- | --- |
| `--base <sha>` | Base commit for the baseline run. Overrides discovery (below). |
| `--pr <n>` | PR number / MR IID. Overrides discovery. |
| `--no-baseline` | Skip the baseline run: every 3/3 failure keeps `baseline = null` and blocks (Phase 1 behavior). |
| `--no-report` | Skip Jira, PR comment and status publishing. Evidence is still uploaded when storage is configured. |

## Steps

1. **Merge and classify**: Phase 1 steps 1-3, unchanged (`classify` in `guard_core::verdict`).
2. **Context**
   - `run_id`: `RUN_ID`, else generated (runner-env.md).
   - `head_sha`: `git rev-parse HEAD`.
   - PR number: `--pr`, else `GUARD_PR_NUMBER`, else GitHub `pull_request.number` from the JSON at `GITHUB_EVENT_PATH`, else GitLab `CI_MERGE_REQUEST_IID`, else none.
   - PR context: with `[server]`: `GET /api/pr-context?number=<n>` (server-api.md); without: `CodeHost::pr_context` when the code host token is present; else none.
   - Base sha (resolved to a full sha with `git rev-parse`; a ref that is not a commit of the clone counts as none, with a warning): `--base`, else `GUARD_BASE_SHA`, else `git merge-base HEAD $GUARD_BASE` (the base `guard test --affected` selected from, cli.md "Base resolution" step 2; a `GUARD_BASE` that is not a commit gets the warning below, and a merge base equal to HEAD counts as none and moves on), else `pr.base_sha`, else GitHub `pull_request.base.sha` from `GITHUB_EVENT_PATH`, else `CI_MERGE_REQUEST_DIFF_BASE_SHA`, else `git merge-base HEAD <d>` where `<d>` is the first candidate of cli.md "Base resolution" steps 4-6 (needs `fetch-depth: 0` / `GIT_DEPTH: 0`; a merge base equal to HEAD, a run on the default branch itself, counts as none), else none.
   - Changed files: `pr.changed_files`, else the committed part (step 1) of cli.md "Changed files" from `git merge-base <base_sha> HEAD`, with submodule expansion, else empty. An entry of `pr.changed_files` that is a gitlink at HEAD or at `<base_sha>` is replaced by the local expansion under `<path>/`, or by the subtree entry `<path>/` when there is no base sha.
   - Ticket key: `pr.ticket_key`, else `[tracker].ticket_pattern` on the branch (`GITHUB_HEAD_REF` / `CI_MERGE_REQUEST_SOURCE_BRANCH_NAME` / `git rev-parse --abbrev-ref HEAD`).
   - `ticket_is_flow_change`: server `ticket.flow_change`; direct mode `IssueTracker::get_issue(key).labels` contains `flow-change`; unknown = `false`.
3. **Journey changes**: for every changed file under `e2e-playwright/tests/journeys/**`, read it at `base_sha` (`git show <base>:<path>`, absent = new file) and at `HEAD`, and call `guard_impact::journey_change(path, base_src, head_src) -> Option<JourneyChange>` (`None` when the spec has no `@journey:` tag). Then `guard_core::journey_changes::review(&changes, ticket_is_flow_change)`.
4. **Baseline** (below) for the tests classified `failed`.
5. **Blocking**: recompute every test with `guard_core::baseline::is_blocking`, where `smoke` = the test has `@smoke` and `affected_by_path` = its journey is selected by `routing::select_affected(catalog, changed_files, None)` with a `Path` reason. Verdict `red` iff any test blocks.
6. **Fingerprints** for every `failed` and `flaky` test: `fingerprint(test_id(project, file, title), error)` (reporting.md).
7. **Evidence** (below) for every `failed` test and every blocking `flaky` test.
8. **Write** `.guard/verdict.json` (v1, see "Files") and `.guard/gate-report.json` (`GateReport` v2); update durations as in Phase 1.
9. **Report** (below) unless `--no-report`.
10. **Print** the summary and exit.

## Baseline run

Only tests classified `failed` (3/3, `unexpected`) are re-run, at most `MAX_BASELINE_TESTS` = 20 (in `tests[]` order); the rest get `unavailable`. App **and** tests run at the base commit: a test that does not exist at base is a regression, and a journey changed by the PR is judged by its base version (fail closed).

1. No base sha → every failed test keeps `baseline = null` (blocks). Print `baseline: no base commit, failures stay blocking`.
2. Worktree: `git worktree remove --force --force .guard/baseline` (ignore errors), then `git worktree add --detach .guard/baseline <base_sha>`. When `.gitmodules` exists at `<base_sha>`, run `git -C <worktree> submodule update --init --recursive` after `worktree add`; on failure, `warning:` and the baseline is unavailable (same path as a failed `worktree add`).
3. Install in the worktree: `pnpm-lock.yaml` at its root → `pnpm install --frozen-lockfile` at its root (`GUARD_PNPM` overrides the program; on Windows `pnpm.exe`, then `pnpm.cmd`); else `package-lock.json` in its `e2e-playwright/` → `npm ci` there. Browsers come from the CI image, never downloaded here.
4. Readiness guard: if `GET base_url + ready_path` already returns `200`, some other app is listening: every failed test → `unavailable` (never test the wrong app).
5. `guard up` with the current binary, `current_dir` = the worktree (its own `target.toml`).
6. Run once per Playwright project involved: `npx playwright test <spec files of the failed tests> --project=<project> --retries=2 --reporter=json` in `<worktree>/e2e-playwright`, with the runner env of runner-env.md (`CI=1`, `GUARD_VIDEO=off`, **no** `E2E_TAGS`: the whole spec files run) and `PLAYWRIGHT_JSON_OUTPUT_NAME=<abs>/.guard/baseline-<project>.json`. Whole files are run and results are matched by `(file, title, project)`, so guard never builds a `--grep` regex.
7. Map each failed test (`guard_core::baseline::BaseRun`): absent or `skipped` at base → `Missing`; `expected` → `Passed`; `unexpected` → `Failed`; `flaky` → `Flaky`. A Playwright exit not in `{0, 1}`, a failed install, a failed `guard up`, or more than 15 minutes for the whole baseline → `Infrastructure` for every test not yet mapped. Then `baseline::classify`.
8. Always: `guard down` (worktree cwd), `git worktree remove --force --force .guard/baseline`.

Outcomes: `regression` (passes or missing at base: this change broke it, blocks), `pre_existing` (fails 3/3 at base: already broken on main, does not block, Bug P1), `flaky` (unstable at base: blocks only if smoke or affected by path), `unavailable` (blocks, fail closed).

## Evidence

Collected from the merged JSON report: `results[].attachments[]` of the first retry (`retry == 1`) of each test, plus the screenshot of the last failing attempt. Names: `screenshot` (`image/png`), `trace` (`application/zip`), `video` (`video/webm`). Also: `location` = `file:line` of the error (`results[].error.location`), the `seed` annotation, the attempts, the baseline outcome, `head_sha`.

### Storage

- Configured by the variable named in `[evidence].storage_url_env` (default `GUARD_STORAGE_URL`). Unset = no upload; links are omitted and Jira attachments come from the local files (direct mode only). Print `evidence: storage not configured, nothing uploaded`.
- Format: `s3+https://<access_key>:<secret_key>@<host>[:<port>]/<bucket>[?region=<region>]` (`s3+http` for a local MinIO; region default `us-east-1`; credentials percent-encoded). Always path-style addressing. Adapter: `guard_store` S3 `EvidenceStore` (SigV4).
- Keys: `<repo or [app].name>/<run_id>/<fingerprint>/<attachment file name>`; the merged HTML report goes under `<repo or app name>/<run_id>/report/` (whole `playwright-report/` directory, at most 500 files / 200 MiB, else skipped with a warning).
- Links: presigned GET URLs valid for `min([evidence].retention_days, 7)` days (SigV4 maximum). Traces also get `viewer_url = https://trace.playwright.dev/?trace=<percent-encoded presigned URL>` (the bucket needs CORS for `https://trace.playwright.dev`; `deploy/docker-compose.yml` configures its S3 server, SeaweedFS, that way). `report_url` = presigned `report/index.html` (its embedded summary opens; per-test attachments inside the HTML report do not, because each object needs its own signature — the evidence links in Jira and the PR are the way in).
- Retention is the bucket lifecycle rule (`retention_days`), not guard.
- Upload failures are warnings; the bundle keeps `key: null`, `url: null`.

## Reporting

Order, each step independent (a failure becomes `warning: reporting: <detail>` and never changes the exit code):

1. **Jira**:
   - `[server]` set → `POST <server.url>/api/gate-result` with `Authorization: Bearer $GUARD_SERVER_TOKEN` and the `GateReport`; the response `outcome` is used for the PR comment. Missing token = warning.
   - No `[server]`, `[tracker].kind = "jira"` and `JIRA_BASE_URL`/`JIRA_EMAIL`/`JIRA_API_TOKEN` set → `guard_tracker::report::report_gate` directly (screenshots from local files).
   - Otherwise skip: `jira: not configured`.
2. **PR comment**: when a PR number and code host credentials exist (`GITHUB_TOKEN` + `GITHUB_REPOSITORY`, or `GITLAB_TOKEN` + `CI_PROJECT_ID`) → `CodeHost::comment_pr(render_pr_comment(report, outcome))`.
3. **Status**: with code host credentials → `CodeHost::publish_status(gate_status(report, None))`; on an infrastructure exit (`3`) → `gate_status(report, Some(reason))` before exiting, when the head sha is known.

Only when a PR number is known: `[tracker].require_ticket = "block"` and no ticket key (and the PR author is not `dependabot[bot]`, `renovate[bot]` or `renovate-bot`) adds the flag `PR has no ticket (require_ticket = block)` to `change_flags` and makes the verdict `red`; `warn` only adds the flag.

## Files

- `.guard/gate-report.json`: the `GateReport` (version `2`), exactly what is sent to the server.
- `.guard/verdict.json`: still version `1` (cli.md), with two changes: `tests[].blocking` follows the Phase 2 rule, `baseline` is `{"base_sha": "<sha or null>", "outcomes": {"<test_id>": "regression" | "pre_existing" | "flaky" | "unavailable"}}` (or `null` with `--no-baseline`), and `not_implemented` is `[]`.

## stdout

```text
verdict: red  (passed 40, flaky 2, failed 2, skipped 0; regressions 1, pre-existing 1)
REGRESSION   tests/journeys/billing/invoice-pdf.spec.ts:7  billing.invoice-pdf  guard-fp-1a2b3c4d  SHOP-210 (created)
PRE-EXISTING tests/journeys/catalog/stock.spec.ts:12  catalog.stock-badge  guard-fp-9e8d7c6b  SHOP-198 (commented)
FLAKY (blocking: smoke)  tests/journeys/checkout/customer-card.spec.ts:9  checkout.customer-card
journeys: checkout.customer-card: assertions 5 -> 3
evidence: 7 files uploaded
jira: 1 created, 1 commented, summary on SHOP-123
pr: comment updated, status guard=failure
```

Lines appear only when relevant; warnings go to stderr.

## Exit codes

| Code | When |
| --- | --- |
| `0` | Verdict green. |
| `1` | Verdict red. Baseline infrastructure problems are **not** exit 3: they make tests `unavailable` (blocking), so the verdict is red. |
| `2` | Usage or config error (`target.toml`, catalog, `thresholds.toml`, bad flags). |
| `3` | Infrastructure error of the main run (no blobs, merge failed, zero tests), as in Phase 1. |

Reporting and evidence failures never change the exit code.
