//! Helpers shared by the Postgres and in-memory stores, so both normalize
//! values the same way.

use guard_core::ports::Mode;
use guard_core::ports::{
    Area, HumanLabel, Kind, LabelSource, Lane, LaneKind, StoreError, TriageDecision,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Parses an RFC 3339 timestamp; anything else is `StoreError::Invalid`.
pub(crate) fn parse_ts(field: &str, value: &str) -> Result<OffsetDateTime, StoreError> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|e| StoreError::Invalid(format!("{field} {value:?} is not RFC 3339: {e}")))
}

/// RFC 3339 in UTC (`...Z`), fractional seconds only when non-zero.
pub(crate) fn format_ts(ts: OffsetDateTime) -> String {
    ts.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_else(|_| ts.unix_timestamp().to_string())
}

/// Postgres keeps microseconds: truncate so both stores compare the same.
pub(crate) fn to_micros(ts: OffsetDateTime) -> OffsetDateTime {
    let micros = ts.microsecond();
    ts.replace_microsecond(micros).unwrap_or(ts)
}

pub(crate) fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Shadow => "shadow",
        Mode::Active => "active",
    }
}

pub(crate) fn lane_parts(lane: &Lane) -> (&'static str, Vec<String>) {
    match lane {
        Lane::Agent => ("agent", vec![]),
        Lane::Human(reasons) => (
            "human",
            reasons
                .iter()
                .map(|r| {
                    serde_json::to_value(r)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                })
                .collect(),
        ),
    }
}

pub(crate) fn lane_kind_name(lane: LaneKind) -> &'static str {
    match lane {
        LaneKind::Agent => "agent",
        LaneKind::Human => "human",
    }
}

pub(crate) fn lane_kind_from(name: &str) -> Result<LaneKind, StoreError> {
    match name {
        "agent" => Ok(LaneKind::Agent),
        "human" => Ok(LaneKind::Human),
        other => Err(StoreError::Invalid(format!("unknown lane {other:?}"))),
    }
}

pub(crate) fn source_name(source: LabelSource) -> &'static str {
    match source {
        LabelSource::JiraWebhook => "jira_webhook",
        LabelSource::Manual => "manual",
    }
}

pub(crate) fn source_from(name: &str) -> Result<LabelSource, StoreError> {
    match name {
        "jira_webhook" => Ok(LabelSource::JiraWebhook),
        "manual" => Ok(LabelSource::Manual),
        other => Err(StoreError::Invalid(format!(
            "unknown label source {other:?}"
        ))),
    }
}

pub(crate) fn kind_from(name: &str) -> Result<Kind, StoreError> {
    Kind::from_name(name).ok_or_else(|| StoreError::Invalid(format!("unknown kind {name:?}")))
}

/// The table checks of `triage_decisions`, enforced before writing.
pub(crate) fn check_decision(d: &TriageDecision) -> Result<OffsetDateTime, StoreError> {
    if d.ticket_key.is_empty() {
        return Err(StoreError::Invalid("decision ticket_key is empty".into()));
    }
    if d.answers.is_none() && d.engine_error.is_none() {
        return Err(StoreError::Invalid(
            "a decision without answers must carry engine_error".into(),
        ));
    }
    parse_ts("decided_at", &d.decided_at).map(to_micros)
}

/// Normalizes a label the way it reads back: UTC timestamp, sorted labels.
pub(crate) fn normalize_label(
    label: &HumanLabel,
) -> Result<(HumanLabel, OffsetDateTime), StoreError> {
    if label.ticket_key.is_empty() {
        return Err(StoreError::Invalid("label ticket_key is empty".into()));
    }
    let at = to_micros(parse_ts("labeled_at", &label.labeled_at)?);
    let mut normalized = label.clone();
    normalized.labeled_at = format_ts(at);
    normalized.labels.sort();
    Ok((normalized, at))
}

pub(crate) fn area_of(name: Option<String>) -> Option<Area> {
    name.map(Area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::ports::HumanReason;

    #[test]
    fn timestamps_normalize_to_utc() {
        let ts = parse_ts("x", "2026-11-01T12:00:00+02:00").unwrap();
        assert_eq!(format_ts(ts), "2026-11-01T10:00:00Z");
        let frac = parse_ts("x", "2026-11-01T10:00:00.1234567Z").unwrap();
        assert_eq!(format_ts(to_micros(frac)), "2026-11-01T10:00:00.123456Z");
        assert!(parse_ts("x", "2026-11-01").is_err());
    }

    #[test]
    fn lane_reasons_are_snake_case() {
        let (lane, reasons) = lane_parts(&Lane::Human(vec![HumanReason::HardRulePath]));
        assert_eq!(lane, "human");
        assert_eq!(reasons, ["hard_rule_path"]);
        assert_eq!(lane_parts(&Lane::Agent), ("agent", vec![]));
    }

    #[test]
    fn names_round_trip() {
        for l in [LaneKind::Agent, LaneKind::Human] {
            assert_eq!(lane_kind_from(lane_kind_name(l)).unwrap(), l);
        }
        for s in [LabelSource::JiraWebhook, LabelSource::Manual] {
            assert_eq!(source_from(source_name(s)).unwrap(), s);
        }
        assert!(kind_from("feature").is_err());
    }
}
