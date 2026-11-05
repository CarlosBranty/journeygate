use super::*;
use crate::ai_settings::{AiSettings, Provider, SummaryLang};
use std::time::Instant;

fn key(provider: Provider) -> FetchKey {
    let mut settings = AiSettings::default();
    settings.choose(provider);
    FetchKey::of(&settings, false)
}

fn ids(choices: &Choices) -> Vec<&str> {
    choices.options.iter().map(|o| o.id.as_str()).collect()
}

fn fetched(list: &[(&str, Option<&str>)]) -> Result<Vec<ModelInfo>, String> {
    Ok(list
        .iter()
        .map(|(id, name)| ModelInfo {
            id: (*id).into(),
            name: name.map(str::to_string),
        })
        .collect())
}

#[test]
fn the_cli_json_list_parses_ids_and_optional_names() {
    let models = parse_list(
        r#"[{"id":"deepseek-flash","name":"DeepSeek-V4.1-Flash"},{"id":"deepseek-v4-pro"}]"#,
    )
    .unwrap();
    assert_eq!(models[0].id, "deepseek-flash");
    assert_eq!(models[0].name.as_deref(), Some("DeepSeek-V4.1-Flash"));
    assert_eq!(models[1].name, None);
    assert_eq!(parse_list("  []\n").unwrap(), []);
    assert!(parse_list("").is_err());
    assert!(parse_list(r#"{"id":"x"}"#).is_err());
}

#[test]
fn every_provider_has_a_static_fallback_holding_its_preset_model() {
    for provider in [Provider::Anthropic, Provider::DeepSeek, Provider::Ollama] {
        let (_, preset) = provider.preset().unwrap();
        assert!(fallback_models(provider).contains(&preset), "{provider:?}");
    }
    assert_eq!(
        fallback_models(Provider::DeepSeek),
        ["deepseek-flash", "deepseek-v4-pro"]
    );
    assert!(fallback_models(Provider::Custom).is_empty());
}

#[test]
fn the_fetch_key_changes_with_provider_base_url_and_saved_key() {
    let anthropic = key(Provider::Anthropic);
    assert_ne!(anthropic, key(Provider::DeepSeek));
    let edited = AiSettings {
        base_url: "https://proxy.example.com".into(),
        ..Default::default()
    };
    assert_ne!(anthropic, FetchKey::of(&edited, false));
    assert_ne!(anthropic, FetchKey::of(&AiSettings::default(), true));
    // Model and language are not part of it: picking a model refetches nothing.
    let other_model = AiSettings {
        model: "claude-opus-5-5".into(),
        lang: SummaryLang::En,
        ..Default::default()
    };
    assert_eq!(anthropic, FetchKey::of(&other_model, false));
    // Surrounding blanks in the URL are not a change.
    let padded = AiSettings {
        base_url: " https://api.anthropic.com ".into(),
        ..Default::default()
    };
    assert_eq!(anthropic, FetchKey::of(&padded, false));
}

#[test]
fn a_fetch_starts_once_the_settings_held_still_for_the_debounce() {
    let mut catalog = ModelCatalog::default();
    let t0 = Instant::now();
    let k = key(Provider::Anthropic);
    assert!(!catalog.observe(&k, t0));
    assert!(!catalog.observe(&k, t0 + DEBOUNCE / 2));
    assert!(catalog.observe(&k, t0 + DEBOUNCE));
    catalog.started(&k);
    assert!(catalog.is_loading(&k));
    // While one is in flight nothing else starts.
    assert!(!catalog.observe(&k, t0 + DEBOUNCE * 3));
    catalog.finished(&k, fetched(&[("m", None)]));
    assert!(!catalog.is_loading(&k));
    // Fetched for this key: no refetch until something changes.
    assert!(!catalog.observe(&k, t0 + DEBOUNCE * 10));
}

#[test]
fn a_change_restarts_the_debounce_and_fetches_for_the_new_key() {
    let mut catalog = ModelCatalog::default();
    let t0 = Instant::now();
    let a = key(Provider::Anthropic);
    let d = key(Provider::DeepSeek);
    catalog.observe(&a, t0);
    assert!(catalog.observe(&a, t0 + DEBOUNCE));
    catalog.started(&a);
    catalog.finished(&a, fetched(&[("m", None)]));
    assert!(!catalog.observe(&d, t0 + DEBOUNCE * 2));
    assert_eq!(catalog.wait(&d, t0 + DEBOUNCE * 2), Some(DEBOUNCE));
    assert!(catalog.observe(&d, t0 + DEBOUNCE * 3));
    assert_eq!(catalog.wait(&d, t0 + DEBOUNCE * 3), None);
}

#[test]
fn refresh_refetches_at_once() {
    let mut catalog = ModelCatalog::default();
    let t0 = Instant::now();
    let k = key(Provider::Anthropic);
    catalog.observe(&k, t0);
    catalog.observe(&k, t0 + DEBOUNCE);
    catalog.started(&k);
    catalog.finished(&k, Err("boom".into()));
    catalog.refresh();
    assert!(catalog.observe(&k, t0 + DEBOUNCE));
    catalog.started(&k);
    // The forced fetch is spent once it starts.
    catalog.finished(&k, fetched(&[("m", None)]));
    assert!(!catalog.observe(&k, t0 + DEBOUNCE * 5));
}

#[test]
fn a_late_result_for_an_old_key_never_shows_for_the_new_one() {
    let mut catalog = ModelCatalog::default();
    let a = key(Provider::Anthropic);
    let d = key(Provider::DeepSeek);
    catalog.started(&a);
    catalog.finished(&a, fetched(&[("claude-x", None)]));
    let choices = catalog.choices(&d, Provider::DeepSeek, "deepseek-flash");
    assert_eq!(ids(&choices), ["deepseek-flash", "deepseek-v4-pro"]);
}

#[test]
fn fetched_models_are_listed_with_their_display_names() {
    let mut catalog = ModelCatalog::default();
    let k = key(Provider::Anthropic);
    catalog.started(&k);
    catalog.finished(
        &k,
        fetched(&[
            ("claude-opus-5-5", Some("Claude Opus 5.5")),
            ("claude-haiku-5-5", None),
        ]),
    );
    let choices = catalog.choices(&k, Provider::Anthropic, "claude-haiku-5-5");
    assert_eq!(ids(&choices), ["claude-opus-5-5", "claude-haiku-5-5"]);
    assert_eq!(
        choices.options[0].label,
        "claude-opus-5-5 \u{2014} Claude Opus 5.5"
    );
    assert_eq!(choices.options[1].label, "claude-haiku-5-5");
    assert!(choices.fetched);
    assert_eq!(choices.note, None);
}

#[test]
fn a_failed_fetch_falls_back_to_the_static_list_and_says_why() {
    let mut catalog = ModelCatalog::default();
    let k = key(Provider::DeepSeek);
    catalog.started(&k);
    catalog.finished(&k, Err("guard exited 3: error: HTTP 401: bad key".into()));
    let choices = catalog.choices(&k, Provider::DeepSeek, "deepseek-flash");
    assert_eq!(ids(&choices), ["deepseek-flash", "deepseek-v4-pro"]);
    assert!(!choices.fetched);
    let note = choices.note.unwrap();
    assert!(note.contains("HTTP 401: bad key"), "{note}");
}

#[test]
fn an_empty_fetch_falls_back_with_a_note() {
    let mut catalog = ModelCatalog::default();
    let k = key(Provider::Ollama);
    catalog.started(&k);
    catalog.finished(&k, Ok(Vec::new()));
    let choices = catalog.choices(&k, Provider::Ollama, "qwen3:8b");
    assert_eq!(ids(&choices), fallback_models(Provider::Ollama));
    assert!(choices.note.unwrap().contains("no models"));
}

#[test]
fn before_any_fetch_the_fallback_shows_without_a_note() {
    let catalog = ModelCatalog::default();
    let k = key(Provider::Anthropic);
    let choices = catalog.choices(&k, Provider::Anthropic, "claude-haiku-5-5");
    assert_eq!(ids(&choices), fallback_models(Provider::Anthropic));
    assert_eq!(choices.note, None);
    assert!(!choices.fetched);
}

#[test]
fn the_current_model_stays_listed_even_when_the_provider_lacks_it() {
    let mut catalog = ModelCatalog::default();
    let k = key(Provider::Anthropic);
    catalog.started(&k);
    catalog.finished(&k, fetched(&[("claude-opus-5-5", None)]));
    let choices = catalog.choices(&k, Provider::Anthropic, " my-private-model ");
    assert_eq!(ids(&choices), ["my-private-model", "claude-opus-5-5"]);
    // A blank model adds nothing; Custom with nothing fetched may be empty.
    let none = ModelCatalog::default().choices(&key(Provider::Custom), Provider::Custom, "");
    assert!(none.options.is_empty());
}

#[test]
fn list_models_goes_through_the_cli_with_json() {
    use crate::commands::GuardCommand;
    assert_eq!(
        GuardCommand::ListModels.args(),
        ["explain", "--list-models", "--json"]
    );
}
