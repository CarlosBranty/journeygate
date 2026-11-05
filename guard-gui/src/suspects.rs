//! `guard suspects --json`, parsed: per failing or flaky journey, the commits
//! most likely related to it, uncommitted changes on its paths and who last
//! changed the line it failed on. Unknown fields are ignored so a newer
//! guard keeps working.

use serde::Deserialize;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Suspect {
    #[serde(default)]
    pub repo: Option<String>,
    pub sha: String,
    pub short: String,
    pub author: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub timestamp: u64,
    pub subject: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub recent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Blame {
    pub file: String,
    pub line: u32,
    #[serde(default)]
    pub sha: Option<String>,
    #[serde(default)]
    pub short: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub timestamp: Option<u64>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub uncommitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JourneySuspects {
    #[serde(default)]
    pub journey: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub commits: Vec<Suspect>,
    #[serde(default)]
    pub uncommitted: Vec<String>,
    #[serde(default)]
    pub blame: Vec<Blame>,
}

impl JourneySuspects {
    /// Whether there is anything to show.
    pub fn has_any(&self) -> bool {
        !self.commits.is_empty() || !self.uncommitted.is_empty() || !self.blame.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct SuspectsDoc {
    #[serde(default)]
    pub journeys: Vec<JourneySuspects>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl SuspectsDoc {
    pub fn for_journey(&self, id: &str) -> Option<&JourneySuspects> {
        self.journeys
            .iter()
            .find(|j| j.journey.as_deref() == Some(id))
    }
}

/// The cached value when it was read for report `stamp`; `None` while the
/// cache holds another report's (or nothing).
pub fn current<'a, S: PartialEq, T>(cached: &'a Option<(S, T)>, stamp: &S) -> Option<&'a T> {
    cached.as_ref().filter(|(s, _)| s == stamp).map(|(_, v)| v)
}

pub fn parse(text: &str) -> Result<SuspectsDoc, String> {
    serde_json::from_str(text.trim()).map_err(|e| format!("bad suspects output: {e}"))
}

/// "5 min ago", "3 days ago": how long before `now` (unix seconds) a commit
/// was authored. A date in the future reads as "just now".
pub fn relative_age(now: u64, timestamp: u64) -> String {
    let secs = now.saturating_sub(timestamp);
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match secs {
        0..1_209_600 => crate::results::format_age(std::time::Duration::from_secs(secs)),
        1_209_600..5_184_000 => plural(secs / 604_800, "week"),
        5_184_000..31_536_000 => plural(secs / 2_592_000, "month"),
        _ => plural(secs / 31_536_000, "year"),
    }
}

/// `text` cut to at most `max` characters, an ellipsis marking the cut.
pub fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}
