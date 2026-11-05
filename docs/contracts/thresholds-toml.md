# Contract: `thresholds.toml`

The calibrated thresholds of the Gatekeeper (section 2) and the impact threshold (section 3), plus the triage mode. They come out of Shadow Mode calibration (section 6) and are owned by the platform team, **not** by the app team: that is why they are not in `target.toml`.

- Machine-readable schema: [`schemas/thresholds.schema.json`](../../schemas/thresholds.schema.json) (JSON Schema draft 2020-12, applied to the TOML document after parsing it into JSON).
- Valid example: [`schemas/examples/thresholds.example.json`](../../schemas/examples/thresholds.example.json).
- Rust: `guard_core::thresholds::{Thresholds, Mode, parse_thresholds, validate}`.

## Location

First match wins:

1. `guard serve --thresholds <path>` / `guard test --thresholds <path>`.
2. `GUARD_THRESHOLDS` environment variable (a path).
3. `<repo root>/thresholds.toml`.
4. None found: every default below (`mode = "shadow"`). A missing file is not an error.

A file that exists but fails to parse or validate is a config error (exit `2` for the CLI; `guard serve` refuses to start).

## Keys

All keys are top-level and optional. Unknown keys are rejected.

| Key | Type | Default | Range | Rule it feeds |
| --- | --- | --- | --- | --- |
| `version` | integer | `1` | must be `1` | — |
| `mode` | `"shadow"` \| `"active"` | `"shadow"` | — | Shadow: every decision is recorded, but the applied lane is always `Human([ShadowMode])` and suggestions go to `jev-suggested`. |
| `agent_suitable_min` | float | `0.80` | `0.0..=1.0` | agent lane needs `agent_suitable > agent_suitable_min` |
| `risk_score_max` | float | `1.0` | `0.0..=4.0` | agent lane needs `risk.score < risk_score_max` (0-based Score, 5 levels; `1.0` = the spec's `< 2.0` on 1-5) |
| `money_or_security_max` | float | `0.15` | `0.0..=1.0` | agent lane needs `touches_money_or_security < money_or_security_max` |
| `clear_criteria_min` | float | `0.70` | `0.0..=1.0` | agent lane needs `clear_e2e_criteria > clear_criteria_min` |
| `confidence_min` | float | `0.75` | `0.0..=1.0` | agent lane needs area, kind and risk `confidence > confidence_min` (Choice and Score only; Noul has no confidence) |
| `existing_flow_max` | float | `0.20` | `0.0..=1.0` | agent lane needs `changes_existing_flow < existing_flow_max`; otherwise human + `flow-change` label |
| `impact_threshold` | float | `0.25` | `0.0..=1.0` | a journey is selected by impact when its Noul `> impact_threshold` (`guard test --affected`, `/api/impact`) |

Every value must be finite (TOML `nan`/`inf` are rejected by `validate`). Integers are accepted where a float is expected (`risk_score_max = 1`).

Rules that come from `target.toml`, not from this file: `[hard_rules].human_only_paths`, `[hard_rules].max_fan_out_for_agent` and `[areas.<key>].sensitive` (see `guard_core::gatekeeper::GateRules`).

## Example

```toml
version = 1
mode = "shadow"

agent_suitable_min = 0.80
risk_score_max = 1.0
money_or_security_max = 0.15
clear_criteria_min = 0.70
confidence_min = 0.75
existing_flow_max = 0.20
impact_threshold = 0.25
```

## Errors

`ThresholdsError::Parse(message)` for TOML syntax errors, unknown keys and wrong types; `ThresholdsError::Invalid { field, message }` for range violations, with `field` the key name and `message` one of:

- `must be 1` (`version`)
- `must be between 0.0 and 1.0`
- `must be between 0.0 and 4.0` (`risk_score_max`)
- `must be finite`

## Change policy

Changing a threshold is a calibration decision: it is reviewed against the reliability diagram per question and the agent-lane precision target (at least 95 %, section 6). The decision log stores the thresholds used for every decision (`triage_decisions.thresholds`), so a change never rewrites history.
