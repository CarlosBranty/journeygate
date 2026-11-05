//! Wall clock for event timestamps and durations, injectable for tests.

use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_millis(&self) -> u64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Always the same instant: deterministic `ts` and zero durations.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(u64);

impl FixedClock {
    pub fn new(millis: u64) -> Self {
        Self(millis)
    }
}

impl Clock for FixedClock {
    fn now_millis(&self) -> u64 {
        self.0
    }
}

/// RFC 3339 UTC, `YYYY-MM-DDTHH:MM:SSZ`, with `.mmm` only when the
/// milliseconds are non-zero.
pub fn rfc3339_from_millis(millis: u64) -> String {
    let secs = millis / 1000;
    let ms = millis % 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    let base = format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    );
    if ms == 0 {
        format!("{base}Z")
    } else {
        format!("{base}.{ms:03}Z")
    }
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Milliseconds of an RFC 3339 UTC timestamp (`Z` or `+00:00` offset, optional
/// fraction). `None` for anything else.
pub fn millis_from_rfc3339(ts: &str) -> Option<u64> {
    let ts = ts.trim();
    let (body, _) = if let Some(b) = ts.strip_suffix('Z') {
        (b, ())
    } else if let Some(b) = ts.strip_suffix("+00:00") {
        (b, ())
    } else {
        return None;
    };
    let (date, time) = body.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let m: i64 = dp.next()?.parse().ok()?;
    let d: i64 = dp.next()?.parse().ok()?;
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (time, None),
    };
    let mut tp = hms.split(':');
    let hh: u64 = tp.next()?.parse().ok()?;
    let mm: u64 = tp.next()?.parse().ok()?;
    let ss: u64 = tp.next()?.parse().ok()?;
    let ms = match frac {
        Some(f) => {
            let digits: String = f.chars().take(3).collect();
            let n: u64 = digits.parse().ok()?;
            n * 10u64.pow(3 - digits.len() as u32)
        }
        None => 0,
    };
    let days = days_from_civil(y, m, d);
    if days < 0 {
        return None;
    }
    Some(((days as u64) * 86_400 + hh * 3600 + mm * 60 + ss) * 1000 + ms)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_round_trip() {
        assert_eq!(rfc3339_from_millis(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            rfc3339_from_millis(1_793_527_200_000),
            "2026-11-01T10:00:00Z"
        );
        assert_eq!(
            rfc3339_from_millis(951_782_400_250),
            "2000-02-29T00:00:00.250Z"
        );
        for ms in [0, 951_782_400_250, 1_793_527_200_000, 4_102_444_799_999] {
            assert_eq!(millis_from_rfc3339(&rfc3339_from_millis(ms)), Some(ms));
        }
        assert_eq!(
            millis_from_rfc3339("2026-11-01T10:00:00.5+00:00"),
            Some(1_793_527_200_500)
        );
        assert_eq!(millis_from_rfc3339("2026-11-01T12:00:00+02:00"), None);
    }
}
