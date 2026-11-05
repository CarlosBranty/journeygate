//! Building `events.jsonl` and `result.json` from a runtime's native output.

use crate::clock::rfc3339_from_millis;
use guard_core::ports::agent::{
    RuntimeEvent, RuntimeEventKind, TASK_SCHEMA_VERSION, TaskResult, TaskStatus, Usage,
};

/// `message.text` cap (agent-task.md).
pub(crate) const MESSAGE_MAX_BYTES: usize = 2000;
/// `result.summary` cap, in characters (agent-task.md).
pub(crate) const SUMMARY_MAX_CHARS: usize = 4000;
/// `RuntimeError::Crashed.stderr_tail` cap.
pub(crate) const STDERR_TAIL_BYTES: usize = 2000;

/// A runtime's output after parsing. `result` is `None` when the stream
/// ended without its terminal record (the run loop turns that into
/// `Protocol` or `Crashed`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Parsed {
    pub events: Vec<RuntimeEvent>,
    pub result: Option<TaskResult>,
}

#[derive(Default)]
pub(crate) struct EventLog {
    events: Vec<RuntimeEvent>,
}

impl EventLog {
    pub(crate) fn push_at(&mut self, millis: u64, event: RuntimeEventKind) {
        self.push_ts(rfc3339_from_millis(millis), event);
    }

    pub(crate) fn push_ts(&mut self, ts: String, event: RuntimeEventKind) {
        let event = match event {
            RuntimeEventKind::Message { text } => RuntimeEventKind::Message {
                text: truncate_bytes(&text, MESSAGE_MAX_BYTES),
            },
            other => other,
        };
        let seq = self.events.len() as u64 + 1;
        self.events.push(RuntimeEvent { seq, ts, event });
    }

    pub(crate) fn usage_sum(&self) -> Usage {
        Usage::sum(self.events.iter().filter_map(RuntimeEvent::usage))
    }

    pub(crate) fn cost_sum(&self) -> f64 {
        self.events
            .iter()
            .map(|e| match &e.event {
                RuntimeEventKind::ModelCall { cost_usd, .. } => *cost_usd,
                _ => 0.0,
            })
            .sum()
    }

    pub(crate) fn into_events(self) -> Vec<RuntimeEvent> {
        self.events
    }
}

pub(crate) fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// The last `max` bytes of `bytes` as lossy UTF-8.
pub(crate) fn tail(bytes: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s = s.trim_end();
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

pub(crate) fn summary(text: &str) -> String {
    text.trim().chars().take(SUMMARY_MAX_CHARS).collect()
}

pub(crate) fn result(
    status: TaskStatus,
    summary_text: &str,
    usage: Usage,
    cost_usd: f64,
    steps: u32,
    error: Option<String>,
) -> TaskResult {
    TaskResult {
        version: TASK_SCHEMA_VERSION,
        status,
        summary: summary(summary_text),
        usage,
        cost_usd,
        steps,
        memory_proposals: vec![],
        error: if status == TaskStatus::Completed {
            None
        } else {
            error.or_else(|| Some(format!("{status:?}")))
        },
    }
}

/// `events.jsonl` bytes: one compact object per line, `\n`-terminated.
pub(crate) fn to_jsonl(events: &[RuntimeEvent]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in events {
        out.extend(serde_json::to_vec(e).expect("event serializes"));
        out.push(b'\n');
    }
    out
}

/// Parses and checks a native `events.jsonl` (seq from 1, strictly increasing).
pub(crate) fn from_jsonl(bytes: &[u8]) -> Result<Vec<RuntimeEvent>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("events.jsonl is not UTF-8: {e}"))?;
    let mut events: Vec<RuntimeEvent> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let e: RuntimeEvent = serde_json::from_str(line)
            .map_err(|err| format!("events.jsonl line {}: {err}", i + 1))?;
        let expected = events.last().map_or(1, |p| p.seq + 1);
        if e.seq != expected {
            return Err(format!(
                "events.jsonl line {}: seq {} where {expected} was expected",
                i + 1,
                e.seq
            ));
        }
        events.push(e);
    }
    Ok(events)
}

pub(crate) fn u64_at(v: &serde_json::Value, key: &str) -> u64 {
    v.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0)
}

pub(crate) fn str_at<'a>(v: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(serde_json::Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_respects_char_boundaries() {
        let s = "é".repeat(1500); // 3000 bytes
        let t = truncate_bytes(&s, 2000);
        assert_eq!(t.len(), 2000);
        let t = truncate_bytes(&s, 1999);
        assert_eq!(t.len(), 1998);
        assert_eq!(tail("ab\n".as_bytes(), 10), "ab");
        assert_eq!(tail("€€".as_bytes(), 4), "€");
    }

    #[test]
    fn jsonl_round_trip_and_seq_check() {
        let mut log = EventLog::default();
        log.push_at(0, RuntimeEventKind::Message { text: "a".into() });
        log.push_at(
            0,
            RuntimeEventKind::Error {
                message: "b".into(),
            },
        );
        let bytes = to_jsonl(&log.into_events());
        assert_eq!(from_jsonl(&bytes).unwrap().len(), 2);
        let bad = String::from_utf8(bytes)
            .unwrap()
            .replace("\"seq\":2", "\"seq\":5");
        assert!(from_jsonl(bad.as_bytes()).unwrap_err().contains("seq 5"));
    }
}
