//! The AI Summary settings: provider preset, API base URL, model and summary
//! language, and how they reach the `guard explain` child.
//!
//! Everything here is non-secret and may be persisted. The API key is never a
//! field: it lives in the OS credential store ([`crate::credentials`]) and is
//! only handed to [`child_env`] at spawn time, so it reaches the child through
//! its environment and never its argv (visible to other processes) or the log.
//!
//! A key is bound to the host it was saved for ([`endpoint_host`]): switching
//! provider or editing the base URL selects that host's key, and the
//! inherited `GUARD_EXPLAIN_API_KEY` / `ANTHROPIC_API_KEY` only ever go to
//! [`ANTHROPIC_HOST`], so an Anthropic key never reaches another endpoint.

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

/// guard's own key variable; the CLI falls back to [`FALLBACK_KEY_ENV`].
pub const KEY_ENV: &str = "GUARD_EXPLAIN_API_KEY";
pub const FALLBACK_KEY_ENV: &str = "ANTHROPIC_API_KEY";
pub const BASE_URL_ENV: &str = "GUARD_EXPLAIN_BASE_URL";
pub const MODEL_ENV: &str = "GUARD_EXPLAIN_MODEL";
pub const LANG_ENV: &str = "GUARD_EXPLAIN_LANG";
/// The host of guard's default endpoint, the only one environment keys go to.
pub const ANTHROPIC_HOST: &str = "api.anthropic.com";
/// What a local Ollama server accepts when it needs no key.
pub const OLLAMA_KEY: &str = "ollama";

/// An Anthropic-compatible Messages API endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Anthropic,
    DeepSeek,
    Ollama,
    Custom,
}

impl Provider {
    pub const ALL: [Self; 4] = [Self::Anthropic, Self::DeepSeek, Self::Ollama, Self::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::DeepSeek => "DeepSeek",
            Self::Ollama => "Ollama (local)",
            Self::Custom => "Custom",
        }
    }

    /// `(base URL, model)` a preset fills in; `None` for Custom.
    pub fn preset(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Anthropic => Some(("https://api.anthropic.com", "claude-haiku-5-5")),
            Self::DeepSeek => Some(("https://api.deepseek.com/anthropic", "deepseek-flash")),
            Self::Ollama => Some(("http://localhost:11434", "qwen3:8b")),
            Self::Custom => None,
        }
    }

    /// A local Ollama server takes any key; every other provider needs one.
    pub fn needs_key(self) -> bool {
        self != Self::Ollama
    }
}

/// The language the summary is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SummaryLang {
    Es,
    En,
}

impl SummaryLang {
    pub const ALL: [Self; 2] = [Self::Es, Self::En];

    /// The `--lang` / `GUARD_EXPLAIN_LANG` code.
    pub fn code(self) -> &'static str {
        match self {
            Self::Es => "es",
            Self::En => "en",
        }
    }

    /// Each language is named in itself.
    pub fn label(self) -> &'static str {
        match self {
            Self::Es => "Español",
            Self::En => "English",
        }
    }
}

/// The persisted, non-secret settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSettings {
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub lang: SummaryLang,
    /// Start "Explain failures" when a test run ends with failures.
    #[serde(default = "enabled")]
    pub auto_summarize: bool,
}

fn enabled() -> bool {
    true
}

/// What to do about the failures summary when a test run ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSummary {
    Start,
    /// It would run, but another explanation is in flight: start it when
    /// that one finishes.
    Queue,
    /// It would run, but no key is there for the configured host.
    NeedsKey,
    Skip,
}

/// Summarize a finished run automatically only when it was not cancelled,
/// something failed or was flaky, the setting is on and a key is there;
/// queued while `busy` (an explanation is still running).
pub fn auto_summary(
    settings: &AiSettings,
    key: KeyStatus,
    cancelled: bool,
    failures: usize,
    busy: bool,
) -> AutoSummary {
    if !settings.auto_summarize || cancelled || failures == 0 {
        AutoSummary::Skip
    } else if key == KeyStatus::Missing {
        AutoSummary::NeedsKey
    } else if busy {
        AutoSummary::Queue
    } else {
        AutoSummary::Start
    }
}

impl Default for AiSettings {
    fn default() -> Self {
        let mut settings = Self {
            provider: Provider::Custom,
            base_url: String::new(),
            model: String::new(),
            lang: SummaryLang::Es,
            auto_summarize: true,
        };
        settings.choose(Provider::Anthropic);
        settings
    }
}

impl AiSettings {
    /// Selects `provider`; a preset fills the base URL and model (both stay
    /// editable), Custom keeps what is there.
    pub fn choose(&mut self, provider: Provider) {
        self.provider = provider;
        if let Some((base, model)) = provider.preset() {
            self.base_url = base.to_string();
            self.model = model.to_string();
        }
    }

    /// The host the key is sent to; see [`endpoint_host`].
    pub fn host(&self) -> String {
        endpoint_host(&self.base_url)
    }

    /// Whether the request goes to Anthropic's own API.
    pub fn is_anthropic_host(&self) -> bool {
        self.host() == ANTHROPIC_HOST
    }
}

/// The `host[:port]` of a base URL, lowercased, without scheme, credentials,
/// path, query or fragment. Blank is guard's default, [`ANTHROPIC_HOST`].
pub fn endpoint_host(base_url: &str) -> String {
    let url = base_url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if host.is_empty() {
        ANTHROPIC_HOST.to_string()
    } else {
        host.to_ascii_lowercase()
    }
}

/// The variables the `guard explain` child gets on top of the inherited
/// environment. Blank base URL or model are left out (guard's defaults
/// apply). The key is the one saved for this host, or the Ollama
/// placeholder. Without either, only [`ANTHROPIC_HOST`] inherits
/// `GUARD_EXPLAIN_API_KEY` / `ANTHROPIC_API_KEY` from this process: for any
/// other host both are blanked (the CLI treats blank as unset), so a key
/// meant for Anthropic is never sent elsewhere.
pub fn child_env(settings: &AiSettings, saved_key: Option<&str>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut push = |name: &str, value: &str| env.push((name.to_string(), value.to_string()));
    let base = settings.base_url.trim();
    if !base.is_empty() {
        push(BASE_URL_ENV, base);
    }
    let model = settings.model.trim();
    if !model.is_empty() {
        push(MODEL_ENV, model);
    }
    push(LANG_ENV, settings.lang.code());
    let foreign = !settings.is_anthropic_host();
    match saved_key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(key) => push(KEY_ENV, key),
        None if !settings.provider.needs_key() => push(KEY_ENV, OLLAMA_KEY),
        None if foreign => push(KEY_ENV, ""),
        None => {}
    }
    if foreign {
        push(FALLBACK_KEY_ENV, "");
    }
    env
}

/// Where the key for the next explain comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// Saved in the OS credential store by this GUI.
    Saved,
    /// Inherited from this variable of the process environment.
    FromEnv(&'static str),
    /// The provider takes no key (Ollama).
    NotRequired,
    Missing,
}

/// The key source, by name only: `lookup` values are never returned. The
/// environment counts only for [`ANTHROPIC_HOST`], as in [`child_env`].
pub fn key_status(
    saved: bool,
    settings: &AiSettings,
    lookup: impl Fn(&str) -> Option<String>,
) -> KeyStatus {
    if saved {
        return KeyStatus::Saved;
    }
    if settings.is_anthropic_host() {
        for name in [KEY_ENV, FALLBACK_KEY_ENV] {
            if lookup(name).is_some_and(|v| !v.trim().is_empty()) {
                return KeyStatus::FromEnv(name);
            }
        }
    }
    if settings.provider.needs_key() {
        KeyStatus::Missing
    } else {
        KeyStatus::NotRequired
    }
}

/// `text` with every occurrence of `secret` replaced, for anything shown
/// after a child that held the key has run.
pub fn redact(text: &str, secret: Option<&str>) -> String {
    match secret.map(str::trim).filter(|s| !s.is_empty()) {
        Some(secret) => text.replace(secret, "[redacted]"),
        None => text.to_string(),
    }
}
