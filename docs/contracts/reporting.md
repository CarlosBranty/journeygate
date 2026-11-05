# Contract: reporting (Jira, PR comment, check status)

What `guard gate finalize` (directly, or through `POST /api/gate-result`) writes to Jira and to the PR/MR. Source: sections 11, 12 and 14 of `docs/propuesta-arquitectura.md`.

| Piece | Owner | Rust |
| --- | --- | --- |
| Fingerprint | `guard-core` | `guard_core::fingerprint::{test_id, normalize_error, fingerprint, from_label}` |
| Jira reporting use case + ADF builders | `guard-tracker` | `guard_tracker::report::{report_gate, bug_summary, bug_description_adf, occurrence_comment_adf, gate_summary_adf}` |
| PR comment + check status | `guard-codehost` | `guard_codehost::report::{render_pr_comment, gate_status}` |

`report_gate` signature (used by `guard-server` and by `guard-cli` in direct mode):

```rust
pub async fn report_gate(
    tracker: &dyn IssueTracker,
    screenshots: &dyn ScreenshotSource,   // local test-results/ (CLI) or EvidenceStore::get_object (server)
    report: &GateReport,
    options: &ReportOptions,              // project key, pre-existing priority, limits
) -> ReportOutcome;                       // never fails: problems go to ReportOutcome.warnings
```

`ScreenshotSource` is a one-method async trait defined in `guard-tracker` (`async fn read(&self, file: &EvidenceFile) -> Option<Vec<u8>>`).

## Fingerprint

```text
test_id     = "<project>::<file>::<title>"
normalized  = normalize_error(first line of the failing attempt's message)
fingerprint = lowercase hex of sha1(test_id + "\n" + normalized), first 8 characters
label       = "guard-fp-" + fingerprint                 e.g. guard-fp-1a2b3c4d
```

Example test id: `chromium::tests/journeys/checkout/customer-card.spec.ts::Checkout: customer pays by card > creates exactly one confirmed order` (`title` joined with ` > ` exactly as in `.guard/verdict.json`).

`normalize_error` rules, in order:

1. Strip ANSI escapes (`\x1b\[[0-9;]*[A-Za-z]`).
2. Keep the first non-empty line.
3. UUIDs (`[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}`) -> `<id>`.
4. Runs of 8 or more hex characters (`\b[0-9a-fA-F]{8,}\b`) that contain at least one digit -> `<id>`.
5. Remaining digit runs with an optional decimal part (`\d+(\.\d+)?`) -> `<n>`.
6. Whitespace runs -> one space; trim.

Example: `\x1b[31mTimeout 5000ms exceeded waiting for order 9f8e7d6c5b4a\x1b[0m` -> `Timeout <n>ms exceeded waiting for order <id>`.

`finalize` computes the fingerprint and carries it in `GateTest.fingerprint` and `EvidenceBundle.fingerprint`; the server never recomputes it.

## Which tests are reported to Jira

| Test (`GateTest`) | Jira action | Labels |
| --- | --- | --- |
| `failed`, baseline `regression` | Bug (dedup by fingerprint), linked to the change ticket | `guard`, `guard-regression`, `guard-fp-<8hex>` |
| `failed`, baseline `pre_existing` | Bug (dedup), priority `JIRA_PRE_EXISTING_PRIORITY` (default `Highest`), not linked to the change ticket | `guard`, `guard-pre-existing`, `guard-fp-<8hex>` |
| `failed`, baseline `unavailable` or none | No Bug; listed as "unconfirmed" in the gate summary | — |
| `flaky`, or `failed` with baseline `flaky` | No Bug; listed in the gate summary | — |
| `passed`, `skipped` | — | — |

At most **10** Bugs are created or commented per gate run; the rest are listed in the summary as `not reported: limit reached`, so a broken environment cannot flood Jira.

## Bug dedup

For each reportable test, in `tests[]` order:

1. `IssueTracker::find_by_label(fingerprint.label())`. Adapter request: `POST /rest/api/3/search/jql` with `{"jql": "project = \"<PROJECT>\" AND labels = \"guard-fp-<8hex>\" ORDER BY created DESC", "fields": ["summary", "status", "labels"], "maxResults": 10}`.
2. If the newest result whose `status_category` is not `done` exists: `comment_adf(key, occurrence_comment_adf(...))`, `ReportedBug.action = commented`.
3. Otherwise: `create_bug(NewBug { project: [tracker].project, summary: bug_summary(test), description_adf: bug_description_adf(...), labels, priority, linked_issue })`, `action = created`. If a `done` Bug was found, the description adds `Previously reported as <KEY> (closed).`
4. Attach up to 3 screenshots (`EvidenceKind::Screenshot`, PNG, each at most 10 MiB) to that Bug with `attach_file`. Traces and videos are never attached; they are linked.

### Jira calls (Cloud REST v3) behind the port

| Port method | Request |
| --- | --- |
| `find_by_label` | `POST /rest/api/3/search/jql` (above) |
| `create_bug` | `POST /rest/api/3/issue` `{"fields": {"project": {"key": P}, "issuetype": {"name": "Bug"}, "summary": S, "description": ADF, "labels": [...], "priority": {"name": "Highest"}}, "update": {"issuelinks": [{"add": {"type": {"name": "Relates"}, "outwardIssue": {"key": "SHOP-123"}}}]}}` (`priority` and `update` only when set) |
| `comment_adf` | `POST /rest/api/3/issue/{key}/comment` `{"body": ADF}` |
| `attach_file` | `POST /rest/api/3/issue/{key}/attachments`, header `X-Atlassian-Token: no-check`, multipart field `file` (file name + `image/png`) |
| `add_labels` | `PUT /rest/api/3/issue/{key}` `{"update": {"labels": [{"add": "x"}]}}` |
| `suggest_labels` | `PUT /rest/api/3/issue/{key}` `{"fields": {"<JIRA_SUGGESTED_FIELD>": [...]}}`; without that variable, `comment_adf` with heading `jev-suggested` and the labels as a bullet list |
| `get_issue` | `GET /rest/api/3/issue/{key}?fields=summary,description,labels,issuetype,status` |

Auth: basic `JIRA_EMAIL:JIRA_API_TOKEN` against `JIRA_BASE_URL` (`https://acme.atlassian.net`). Issue URL: `<JIRA_BASE_URL>/browse/<key>`. Status mapping: 401/403 -> `Unauthorized`, 404 -> `NotFound`, 429 -> `RateLimited`, other non-2xx -> `Rejected`. `statusCategory.key`: `new` -> `to_do`, `indeterminate` -> `in_progress`, `done` -> `done`.

## ADF layouts

All bodies are `{"type": "doc", "version": 1, "content": [...]}` using only `heading`, `paragraph`, `bulletList`/`listItem`, `codeBlock` and `text` with marks `strong`, `code` and `link`. Builders are pure functions tested against golden JSON.

### `bug_summary(test)`

`[guard] <journey id, else title>: <normalized error>`, cut to 255 characters (Jira limit) with a trailing `…` when cut.

### `bug_description_adf(test, bundle, report)`

1. heading 3: `Regression in <journey or title>`, or `Already broken on <base_branch>` (pre-existing)
2. paragraph: **Test** `<test_id>` (code mark)
3. paragraph: **Location** `<bundle.location>` (code mark), else `<file>:<line>`
4. codeBlock: the error (first line, not normalized)
5. bulletList, one item per attempt: `attempt <retry+1>: <status> (<duration_ms> ms)`, then `base commit <base_sha[0..12]>: <baseline outcome>`
6. paragraph **Links**: `Trace` (viewer_url), `Video`, `Report` (report_url), `Pipeline` (ci_url), `PR` (pr.url): only those present, as link marks separated by ` · `
7. paragraph: `Seed <seed> · commit <head_sha[0..12]> · run <run_id>` (seed omitted when absent)
8. paragraph, regressions only: `If the flow change is intentional, update this journey in the same PR.`

### `occurrence_comment_adf(test, bundle, report)`

1. paragraph: **New occurrence** in run `<run_id>` on commit `<head_sha[0..12]>` (PR link when present)
2. codeBlock: the error
3. paragraph: the same Links line

### `gate_summary_adf(report, outcome)`: comment on the change ticket

Posted once per gate run on `report.ticket_key` when it exists (`ReportOutcome.ticket_comment`):

1. heading 3: `guard: green` / `guard: red`
2. paragraph: `passed P, flaky F, failed X (regressions R, pre-existing E), skipped S`
3. heading 4 `Regressions` + bulletList `<journey or title> — <Bug key link> — <location>` (only when any)
4. heading 4 `Already broken on main` + bulletList (only when any)
5. heading 4 `Unconfirmed or flaky` + bulletList (only when any)
6. heading 4 `Modified journeys` + bulletList: `report.change_flags` first, then `<journey>: <flag>` per `journey_changes` entry (only when any)
7. paragraph: Links (Report, Pipeline, PR)

No ticket key: no Jira comment; the summary lives only in the PR.

## PR / MR comment

`render_pr_comment(report, outcome)` returns Markdown, upserted with `CodeHost::comment_pr` (one comment per PR, edited on every run). The first line is the marker `<!-- guard:gate-summary -->`:

```markdown
<!-- guard:gate-summary -->
### guard: red

passed 40 · flaky 2 · failed 1 (regressions 1, pre-existing 0) · skipped 0

| | Test | Location | Jira |
| --- | --- | --- | --- |
| ❌ regression | `checkout.customer-card` | `tests/journeys/checkout/customer-card.spec.ts:14` | [SHOP-210](https://acme.atlassian.net/browse/SHOP-210) |
| ⚠️ flaky (blocking: smoke) | `catalog.stock-badge` | `tests/journeys/catalog/stock.spec.ts:9` | — |

**Modified journeys** (QA review via CODEOWNERS)
- journeys modified, but the ticket is not labeled flow-change
- checkout.customer-card: assertions 5 -> 3

If the flow change is intentional, update the failing journey in this PR.

[Report](<report_url>) · [Pipeline](<ci_url>) · run `12345-1`
```

- Rows: every blocking test, then pre-existing, then non-blocking flaky; at most 30 rows, then `… and N more`.
- Row labels: `❌ regression`, `❌ unconfirmed` (failed, baseline unavailable or none), `🟡 pre-existing`, `⚠️ flaky (blocking: smoke)`, `⚠️ flaky (blocking: affected)`, `⚠️ flaky`.
- The Test column shows the journey id, else the title. The Jira column links `outcome.bugs` by fingerprint, else `—`.
- Empty sections are omitted; the flow-change sentence appears only with at least one regression; links only when present.

## Check run / commit status

`gate_status(report, infrastructure_error: Option<&str>) -> GateStatus` with `name = "guard"` and `head_sha = report.head_sha`:

| Case | `CheckState` | GitHub check run (`POST /repos/{owner}/{repo}/check-runs`) | GitLab commit status (`POST /projects/:id/statuses/:sha`) |
| --- | --- | --- | --- |
| green | `success` | `status: completed`, `conclusion: success` | `state: success` |
| red | `failure` | `completed`, `failure` | `failed` |
| infrastructure error (finalize exit 3) | `error` | `completed`, `failure`, title `guard: infrastructure error` | `failed` |
| started (optional, by `gate plan`) | `running` | `in_progress` | `running` |
| queued | `pending` | `queued` | `pending` |

- The name is always `guard`: branch protection (GitHub) and "Pipelines must succeed" plus the status name (GitLab) depend on it.
- `title` (GitHub `output.title`, GitLab `description`, at most 140 characters): `guard: red — 1 regression, 1 blocking flaky` or `guard: green — 43 passed, 2 flaky`.
- `summary` (GitHub `output.summary` only): the PR comment Markdown without the marker.
- `details_url`: `report.ci_url`, else `report.report_url` (GitHub `details_url`, GitLab `target_url`).
- GitHub needs `checks: write` and `pull-requests: write` (`GITHUB_TOKEN` in Actions); comments use `GET/POST /repos/{owner}/{repo}/issues/{n}/comments` and `PATCH /repos/{owner}/{repo}/issues/comments/{id}`. GitLab uses `GITLAB_TOKEN` (`api` scope); MR notes use `GET/POST /projects/:id/merge_requests/:iid/notes` and `PUT .../notes/:note_id`.
