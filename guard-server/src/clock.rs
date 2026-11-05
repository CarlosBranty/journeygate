//! Wall clock behind a trait, so decision timestamps are testable.

use guard_core::timefmt::rfc3339_utc;
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// Now, as RFC 3339 UTC (`YYYY-MM-DDTHH:MM:SSZ`).
    fn now_rfc3339(&self) -> String;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_rfc3339(&self) -> String {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        rfc3339_utc(secs)
    }
}

/// Always the same instant (tests, replays).
pub struct FixedClock(pub String);

impl Clock for FixedClock {
    fn now_rfc3339(&self) -> String {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_is_rfc3339_utc() {
        let now = SystemClock.now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'));
        assert!(now.as_str() > "2025-01-01T00:00:00Z");
    }

    #[test]
    fn fixed_clock_is_fixed() {
        assert_eq!(FixedClock("x".into()).now_rfc3339(), "x");
    }
}
