# Contract: Jev questions (triage and impact)

How `guard-jev` implements `guard_core::ports::DecisionEngine` on TypeSafe Jev. Source: sections 2 and 3 of `docs/propuesta-arquitectura.md`. Everything in this file is a contract: question names, texts and option names are part of the calibration dataset, so changing any of them invalidates calibration and needs a new golden-set run.

## Endpoint and wire format

| Item | Value |
| --- | --- |
| Endpoint | `POST https://api.typesafe.ai/v1/systemone` (override with `JEV_URL`, used by tests against wiremock) |
| Auth | `Authorization: Bearer <TYPESAFE_API_KEY>` |
| Model | `jev-1.13.0`, pinned (`guard_jev::MODEL`). Never an alias such as `jev-latest`. |
| Body | `{"model": "jev-1.13.0", "state": <object>, "questions": {<name>: <question>}}`. `questions` keeps insertion order (`IndexMap`). |
| Timeout | 800 ms per attempt. |
| Retry | Exactly one retry, after 200 ms, on `429`, `529` or a timeout. Then `EngineError::Retryable(status)` / `EngineError::Timeout`. |
| No retry | `400`, `422` and any other non-200: `EngineError::Rejected { status, body }`. |
| Model drift | `200` whose `model` differs from `jev-1.13.0`: `EngineError::ModelDrift`. Never accepted. |

Question JSON shapes (`type` tag, lowercase):

```json
{"type": "noul",   "instructions": "...", "criteria": {"true": "...", "false": "..."}}
{"type": "choice", "instructions": "...", "criteria": {"<option>": "<description or null>"}}
{"type": "score",  "instructions": "...", "criteria": ["level 0", "level 1", "level 2", "level 3", "level 4"]}
```

Answer JSON shapes, keyed by question name in `answers`:

```json
{"type": "noul",   "noul": 0.12}
{"type": "choice", "choice": "checkout", "confidence": 0.91, "probabilities": {"checkout": 0.91, "catalog": 0.06, "account": 0.03}}
{"type": "score",  "score": 0.4, "confidence": 0.88, "legend": {"0": "...", "1": "..."}, "probabilities": {"0": 0.7, "1": 0.2, "2": 0.1, "3": 0.0, "4": 0.0}}
```

- `noul` has **only** the probability that the statement is true. There is no confidence for Noul.
- `score` is the probability-weighted mean of **0-based** levels: `0.0..=4.0` for five levels.
- Any other answer type (e.g. `box`) is `EngineError::UnexpectedAnswer`.

The response also carries `usage {input_tokens, output_tokens}`; the adapter logs it and ignores it otherwise.

## Triage request

One request with all seven questions, in this order. The `state` is `guard_core::ports::TriageState` serialized as-is:

```json
{
  "summary": "Stock badge shows 'In stock' for sold-out products",
  "description": "On the product page the badge ignores reservations ...",
  "acceptance_criteria": ["A product with 0 available units shows 'Sold out'"],
  "mentioned_modules": ["examples/demo-shop/src/products/stock.mjs"],
  "blast_radius": "isolated"
}
```

`blast_radius` is the only reach information Jev sees (`isolated` 0-3, `moderate` 4-15, `wide` 16+ fan-out). The raw numbers stay in Rust.

### 1. `area` (Choice)

- instructions: `Which product area of the application does this ticket primarily change? Choose the single area whose description best matches where the requested change lives.`
- criteria: one option per `[areas.<key>]` of `target.toml`; option name = the area key, description = `[areas.<key>].description` verbatim. Canonical order = keys sorted ascending (the order of `TargetConfig.areas`). At most 255 options (`MAX_AREAS`).
- maps to `TriageAnswers.area = Classified { value: Area(choice), confidence }`. A `choice` that is not one of the options is `UnexpectedAnswer`.

### 2. `kind` (Choice)

- instructions: `What kind of change does this ticket ask for?`
- criteria (canonical order = `Kind::ALL`):

| Option | Description |
| --- | --- |
| `bugfix` | `Fixes behavior that is wrong today, without adding new capabilities.` |
| `minor_feature` | `Adds a small, self-contained capability or option to an existing flow.` |
| `refactor` | `Restructures code without changing user-visible behavior.` |
| `architecture` | `Changes system structure: new services, data models, cross-cutting infrastructure, or how components communicate.` |

- maps to `TriageAnswers.kind` via `Kind::from_name`.

### 3. `risk` (Score, 5 levels)

- instructions: `How risky is it to implement this ticket incorrectly, for users and for the business?`
- criteria, low to high (never permuted: the order is the scale):
  0. `Trivial: copy, styling or configuration with no behavioral impact.`
  1. `Low: a localized behavior change in one module, easy to verify and revert.`
  2. `Medium: a behavior change across several modules, or one that touches persisted data.`
  3. `High: changes a critical flow (payments, authentication, orders) or data shared by many features.`
  4. `Critical: could cause data loss, a security exposure or a full outage if wrong.`
- maps to `TriageAnswers.risk = Classified { value: score, confidence }`. `score` must be within `0.0..=4.0`.

### 4. `clear_e2e_criteria` (Noul)

- instructions: `The ticket states acceptance criteria concrete enough that an end-to-end browser test can verify each of them through observable outcomes.`
- criteria.true: `Every criterion names an observable result in the UI or the API that a test can assert.`
- criteria.false: `Criteria are missing, vague, or only describe implementation details.`

### 5. `touches_money_or_security` (Noul)

- instructions: `Implementing this ticket changes how money is charged, refunded or displayed, or changes authentication, authorization, secrets or the handling of personal data.`
- criteria.true: `The change affects payments, prices, refunds, login, permissions, tokens, secrets or personal data.`
- criteria.false: `The change does not touch money, access control, secrets or personal data.`

### 6. `agent_suitable` (Noul)

- instructions: `A competent autonomous coding agent could implement this ticket correctly using only the ticket text and the repository, without asking a human any clarifying question.`
- criteria.true: `The goal, scope and expected behavior are unambiguous and self-contained.`
- criteria.false: `The ticket needs product decisions, design input, access to external systems or clarification.`

### 7. `changes_existing_flow` (Noul)

- instructions: `This ticket intentionally changes how an existing user-facing process works (its steps, rules or outcome), rather than fixing it or adding a separate new process.`
- criteria.true: `An existing process will behave differently on purpose after this change.`
- criteria.false: `Existing processes keep working as they do today; the ticket fixes a defect or adds something new alongside them.`

### Conversion rules (`TryFrom` response -> `TriageAnswers`)

1. `model` must equal `jev-1.13.0` (`ModelDrift`).
2. Each of the seven names must be present (`MissingAnswer(name)`) with the expected type (`UnexpectedAnswer`).
3. Every number must be finite; Noul and confidences within `0.0..=1.0`; Score within `0.0..=4.0` (`UnexpectedAnswer`).
4. `TriageAnswers.model` = response `model`; `TriageAnswers.raw` = the response `answers` object verbatim (keeps Choice probabilities and the Score legend for calibration).

## Choice permutation (calibration)

Some configurations favor the first options (position bias). In Shadow Mode the server sets `TriageRequest.permutation_seed = Some(random u64)` and records it in the decision log; in Active Mode it is `None` (canonical order).

With a seed, the `area` and `kind` options are shuffled independently:

- PRNG: SplitMix64. `state = seed`; `next(): state = state + 0x9E3779B97F4A7C15 (wrapping); z = state; z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9; z = (z ^ (z >> 27)) * 0x94D049BB133111EB; return z ^ (z >> 31)` (all wrapping `u64`).
- `area` uses `seed`; `kind` uses `seed ^ 0xD1B54A32D192ED03`.
- Fisher-Yates: `for i in (1..n).rev() { j = next() % (i + 1); swap(i, j) }` over the canonical order.
- `risk` levels are never permuted.

The same seed always yields the same order, so a logged decision can be replayed exactly.

## Impact request

`DecisionEngine::impact(diff, journeys)`: one Noul per active catalog journey, answering "does this diff affect the journey".

- `state` = `guard_core::ports::DiffSummary` serialized as-is (`files`, `symbols`, `hunks`, `truncated`). Never the raw `git diff`.
- Question name: `impact_<n>`, `n` = 0-based index of the journey **within that request**. The adapter maps names back to journey ids; ids never travel as question names (they contain `.` and `-`).
- instructions: the journey's `impact_question` verbatim (catalog contract: written as an affirmation).
- criteria.true: `The changed code runs, or its output is used, while a user goes through this process.`
- criteria.false: `The changed code is unrelated to this process or only affects other processes.`
- Batching: at most **25** questions per request; more journeys = several requests with the same state, sent sequentially. Any batch failing fails the whole call (fail closed: callers run every journey).
- Result: exactly one entry per input journey id, value = the Noul. Selection threshold: `thresholds.impact_threshold` (`> 0.25`), applied by the caller, never by the adapter.

### `DiffSummary` size limits (`guard_impact::summarize_diff`)

Jev allows at most 32k tokens per question and 64k per request. The summary keeps the state well under that:

| Limit | Value |
| --- | --- |
| files | 200 (sorted by path) |
| symbols | 300 (`path#symbol`) |
| hunks | 40, each at most 12 body lines, each line cut to 200 characters |
| serialized JSON | 48,000 bytes; when over, drop hunks from the end, then symbols, then files |

`truncated = true` whenever anything was cut.

## Fail-closed behavior (callers)

| Caller | On any `EngineError` |
| --- | --- |
| Triage (server) | `gatekeeper::decide_without_engine` = human lane with `EngineUnavailable`; the decision is still recorded with `engine_error`. |
| `/api/impact` (server) | `503 {"error": "engine_unavailable"}`. |
| `guard test --affected` | Runs every active journey instead of guessing. |
