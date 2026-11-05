//! The Model picker of the AI settings: the models the provider offers,
//! fetched through `guard explain --list-models --json`, with a static
//! fallback per provider when the fetch fails or has not answered yet.
//!
//! [`ModelCatalog`] is the pure state machine behind the picker: it decides
//! when to fetch (debounced, once per [`FetchKey`]), keeps results apart per
//! key so a late answer for old settings never shows for new ones, and
//! builds the [`Choices`] the combo lists.

#[cfg(test)]
mod tests;

use crate::ai_settings::{AiSettings, Provider};
use serde::Deserialize;
use std::time::{Duration, Instant};

/// How long the provider, base URL and key must hold still before a fetch,
/// so typing a base URL does not start one fetch per keystroke.
pub const DEBOUNCE: Duration = Duration::from_millis(600);

/// One model the provider offers, as `guard explain --list-models --json`
/// prints it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// The CLI's `[{id, name?}]` stdout.
pub fn parse_list(stdout: &str) -> Result<Vec<ModelInfo>, String> {
    serde_json::from_str(stdout.trim()).map_err(|e| format!("bad model list: {e}"))
}

/// Model ids offered when the provider's list cannot be fetched. Each
/// preset's own model is among them.
pub fn fallback_models(provider: Provider) -> &'static [&'static str] {
    match provider {
        Provider::Anthropic => &[
            "claude-haiku-5-5",
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-fable-5-1",
        ],
        // api-docs.deepseek.com: the two current ids.
        Provider::DeepSeek => &["deepseek-flash", "deepseek-v4-pro"],
        Provider::Ollama => &["qwen3:8b"],
        Provider::Custom => &[],
    }
}

/// What a fetched list belongs to: the endpoint and whether a key is saved
/// for it. The model and language are not part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchKey {
    provider: Provider,
    base_url: String,
    key_saved: bool,
}

impl FetchKey {
    pub fn of(settings: &AiSettings, key_saved: bool) -> Self {
        Self {
            provider: settings.provider,
            base_url: settings.base_url.trim().to_string(),
            key_saved,
        }
    }
}

/// One entry of the Model combo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

/// What the Model combo lists.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Choices {
    pub options: Vec<ModelOption>,
    /// Why the static fallback is shown, for a muted line under the combo.
    pub note: Option<String>,
    /// Whether the options came from the provider.
    pub fetched: bool,
}

/// The fetched model lists and when to fetch the next one.
#[derive(Debug, Default)]
pub struct ModelCatalog {
    result: Option<(FetchKey, Result<Vec<ModelInfo>, String>)>,
    in_flight: Option<FetchKey>,
    /// The last key seen and since when, for the debounce.
    seen: Option<(FetchKey, Instant)>,
    /// Refresh was pressed: fetch at once, even for a fetched key.
    force: bool,
}

impl ModelCatalog {
    /// Records the settings shown now; true when a fetch for `key` should
    /// start: nothing in flight, and either refresh was pressed or `key` has
    /// no result yet and held still for [`DEBOUNCE`].
    pub fn observe(&mut self, key: &FetchKey, now: Instant) -> bool {
        if self.seen.as_ref().is_none_or(|(k, _)| k != key) {
            self.seen = Some((key.clone(), now));
        }
        if self.in_flight.is_some() {
            return false;
        }
        if self.force {
            return true;
        }
        if self.has_result(key) {
            return false;
        }
        self.seen
            .as_ref()
            .is_some_and(|(_, since)| now.saturating_duration_since(*since) >= DEBOUNCE)
    }

    /// How long until [`Self::observe`] would start a fetch for `key`, for a
    /// delayed repaint; `None` when nothing is waiting.
    pub fn wait(&self, key: &FetchKey, now: Instant) -> Option<Duration> {
        if self.in_flight.is_some() || self.force || self.has_result(key) {
            return None;
        }
        let (seen, since) = self.seen.as_ref()?;
        if seen != key {
            return None;
        }
        let left = DEBOUNCE.saturating_sub(now.saturating_duration_since(*since));
        (!left.is_zero()).then_some(left)
    }

    pub fn started(&mut self, key: &FetchKey) {
        self.in_flight = Some(key.clone());
        self.force = false;
    }

    /// Stores a fetch's result under the key it was started for.
    pub fn finished(&mut self, key: &FetchKey, result: Result<Vec<ModelInfo>, String>) {
        if self.in_flight.as_ref() == Some(key) {
            self.in_flight = None;
        }
        self.result = Some((key.clone(), result));
    }

    /// The ↻ button: fetch again as soon as nothing is in flight.
    pub fn refresh(&mut self) {
        self.force = true;
    }

    pub fn is_loading(&self, key: &FetchKey) -> bool {
        self.in_flight.as_ref() == Some(key)
    }

    fn has_result(&self, key: &FetchKey) -> bool {
        self.result.as_ref().is_some_and(|(k, _)| k == key)
    }

    /// The fetched list for `key`, else the provider's static fallback (with
    /// a note when a fetch failed or came back empty). `current` is always
    /// listed, first when the provider does not offer it.
    pub fn choices(&self, key: &FetchKey, provider: Provider, current: &str) -> Choices {
        let fallback = || {
            fallback_models(provider)
                .iter()
                .map(|id| ModelOption {
                    id: (*id).to_string(),
                    label: (*id).to_string(),
                })
                .collect::<Vec<_>>()
        };
        let result = self
            .result
            .as_ref()
            .filter(|(k, _)| k == key)
            .map(|(_, r)| r);
        let mut choices = match result {
            Some(Ok(models)) if !models.is_empty() => Choices {
                options: models.iter().map(option).collect(),
                note: None,
                fetched: true,
            },
            Some(Ok(_)) => Choices {
                options: fallback(),
                note: Some("The provider reported no models; showing suggestions.".into()),
                fetched: false,
            },
            Some(Err(e)) => Choices {
                options: fallback(),
                note: Some(format!("Could not fetch the model list: {}", one_line(e))),
                fetched: false,
            },
            None => Choices {
                options: fallback(),
                note: None,
                fetched: false,
            },
        };
        let current = current.trim();
        if !current.is_empty() && !choices.options.iter().any(|o| o.id == current) {
            choices.options.insert(
                0,
                ModelOption {
                    id: current.to_string(),
                    label: current.to_string(),
                },
            );
        }
        choices
    }
}

/// `id — display name`, or the id alone when there is no other name.
fn option(model: &ModelInfo) -> ModelOption {
    let label = match model.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() && name != model.id => {
            format!("{} \u{2014} {name}", model.id)
        }
        _ => model.id.clone(),
    };
    ModelOption {
        id: model.id.clone(),
        label,
    }
}

/// An error's lines joined, for a single wrapped label.
fn one_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}
