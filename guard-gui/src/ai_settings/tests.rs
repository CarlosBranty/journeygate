use super::*;
use crate::commands::GuardCommand;

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }
}

fn value<'a>(env: &'a [(String, String)], name: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[test]
fn every_preset_fills_its_base_url_and_model() {
    assert_eq!(
        Provider::Anthropic.preset(),
        Some(("https://api.anthropic.com", "claude-haiku-5-5"))
    );
    assert_eq!(
        Provider::DeepSeek.preset(),
        Some(("https://api.deepseek.com/anthropic", "deepseek-flash"))
    );
    assert_eq!(
        Provider::Ollama.preset(),
        Some(("http://localhost:11434", "qwen3:8b"))
    );
    assert_eq!(Provider::Custom.preset(), None);
}

#[test]
fn the_default_is_anthropic_in_spanish() {
    let settings = AiSettings::default();
    assert_eq!(settings.provider, Provider::Anthropic);
    assert_eq!(settings.base_url, "https://api.anthropic.com");
    assert_eq!(settings.model, "claude-haiku-5-5");
    assert_eq!(settings.lang, SummaryLang::Es);
    assert_eq!(settings.lang.code(), "es");
    assert_eq!(SummaryLang::En.code(), "en");
}

#[test]
fn choosing_a_preset_fills_base_and_model_and_custom_keeps_them() {
    let mut settings = AiSettings::default();
    settings.choose(Provider::DeepSeek);
    assert_eq!(settings.provider, Provider::DeepSeek);
    assert_eq!(settings.base_url, "https://api.deepseek.com/anthropic");
    assert_eq!(settings.model, "deepseek-flash");
    // Both stay editable; Custom keeps whatever was typed.
    settings.model = "deepseek-pro".into();
    settings.choose(Provider::Custom);
    assert_eq!(settings.provider, Provider::Custom);
    assert_eq!(settings.base_url, "https://api.deepseek.com/anthropic");
    assert_eq!(settings.model, "deepseek-pro");
}

#[test]
fn only_ollama_needs_no_key() {
    assert!(!Provider::Ollama.needs_key());
    for p in [Provider::Anthropic, Provider::DeepSeek, Provider::Custom] {
        assert!(p.needs_key(), "{p:?}");
    }
}

#[test]
fn child_env_carries_base_model_lang_and_the_saved_key() {
    let settings = AiSettings {
        lang: SummaryLang::En,
        ..Default::default()
    };
    let env = child_env(&settings, Some("sk-saved"));
    assert_eq!(
        value(&env, "GUARD_EXPLAIN_BASE_URL"),
        Some("https://api.anthropic.com")
    );
    assert_eq!(value(&env, "GUARD_EXPLAIN_MODEL"), Some("claude-haiku-5-5"));
    assert_eq!(value(&env, "GUARD_EXPLAIN_LANG"), Some("en"));
    assert_eq!(value(&env, "GUARD_EXPLAIN_API_KEY"), Some("sk-saved"));
}

#[test]
fn without_a_saved_key_anthropic_inherits_the_process_environment() {
    let settings = AiSettings::default();
    let env = child_env(&settings, None);
    assert_eq!(value(&env, "GUARD_EXPLAIN_API_KEY"), None);
    assert_eq!(value(&env, "ANTHROPIC_API_KEY"), None);
    let blank = child_env(&settings, Some("   "));
    assert_eq!(value(&blank, "GUARD_EXPLAIN_API_KEY"), None);
    assert_eq!(value(&blank, "ANTHROPIC_API_KEY"), None);
}

#[test]
fn another_host_never_inherits_an_environment_key() {
    // DeepSeek with no saved key: both inherited variables are blanked (the
    // CLI treats blank as unset), so an Anthropic key never reaches it.
    let mut settings = AiSettings::default();
    settings.choose(Provider::DeepSeek);
    let env = child_env(&settings, None);
    assert_eq!(value(&env, "GUARD_EXPLAIN_API_KEY"), Some(""));
    assert_eq!(value(&env, "ANTHROPIC_API_KEY"), Some(""));
    // The same for a custom host, and for a preset whose URL was edited.
    let mut custom = AiSettings::default();
    custom.choose(Provider::Custom);
    custom.base_url = "https://llm.example.com/v1".into();
    assert_eq!(
        value(&child_env(&custom, None), "ANTHROPIC_API_KEY"),
        Some("")
    );
    let edited = AiSettings {
        base_url: "https://proxy.example.com".into(),
        ..Default::default()
    };
    let env = child_env(&edited, None);
    assert_eq!(value(&env, "GUARD_EXPLAIN_API_KEY"), Some(""));
    assert_eq!(value(&env, "ANTHROPIC_API_KEY"), Some(""));
    // A key saved for that host still goes through.
    let env = child_env(&settings, Some("sk-deepseek"));
    assert_eq!(value(&env, "GUARD_EXPLAIN_API_KEY"), Some("sk-deepseek"));
    assert_eq!(value(&env, "ANTHROPIC_API_KEY"), Some(""));
}

#[test]
fn the_host_of_a_base_url_names_where_the_key_goes() {
    assert_eq!(
        endpoint_host("https://api.anthropic.com"),
        "api.anthropic.com"
    );
    assert_eq!(
        endpoint_host("https://api.deepseek.com/anthropic"),
        "api.deepseek.com"
    );
    assert_eq!(endpoint_host("http://localhost:11434"), "localhost:11434");
    assert_eq!(
        endpoint_host(" HTTPS://API.Example.COM/v1?x#y "),
        "api.example.com"
    );
    assert_eq!(endpoint_host("https://user:pw@host.test/x"), "host.test");
    assert_eq!(endpoint_host("host.test/path"), "host.test");
    // Blank means guard's default endpoint.
    assert_eq!(endpoint_host("  "), "api.anthropic.com");
    assert!(AiSettings::default().is_anthropic_host());
    let mut deepseek = AiSettings::default();
    deepseek.choose(Provider::DeepSeek);
    assert!(!deepseek.is_anthropic_host());
}

#[test]
fn ollama_sends_a_placeholder_key_unless_one_is_saved() {
    let mut settings = AiSettings::default();
    settings.choose(Provider::Ollama);
    assert_eq!(
        value(&child_env(&settings, None), "GUARD_EXPLAIN_API_KEY"),
        Some("ollama")
    );
    assert_eq!(
        value(&child_env(&settings, Some("mine")), "GUARD_EXPLAIN_API_KEY"),
        Some("mine")
    );
}

#[test]
fn blank_base_or_model_are_left_to_guard_defaults() {
    let settings = AiSettings {
        provider: Provider::Custom,
        base_url: "  ".into(),
        model: String::new(),
        lang: SummaryLang::Es,
        auto_summarize: true,
    };
    let env = child_env(&settings, None);
    assert_eq!(value(&env, "GUARD_EXPLAIN_BASE_URL"), None);
    assert_eq!(value(&env, "GUARD_EXPLAIN_MODEL"), None);
    assert_eq!(value(&env, "GUARD_EXPLAIN_LANG"), Some("es"));
}

#[test]
fn the_key_goes_to_the_environment_never_to_argv() {
    let secret = "sk-ant-very-secret";
    let env = child_env(&AiSettings::default(), Some(secret));
    assert!(env.iter().any(|(_, v)| v == secret));
    for journey in [None, Some("checkout.split-payment".to_string())] {
        let argv = GuardCommand::Explain {
            journey: journey.clone(),
        }
        .args();
        assert!(argv.iter().all(|a| !a.contains(secret)), "{argv:?}");
        let display = GuardCommand::Explain { journey }.display();
        assert!(!display.contains(secret), "{display}");
    }
}

#[test]
fn key_status_prefers_the_saved_key_then_the_environment() {
    let anthropic = AiSettings::default();
    let both = env(&[("GUARD_EXPLAIN_API_KEY", "a"), ("ANTHROPIC_API_KEY", "b")]);
    assert_eq!(key_status(true, &anthropic, &both), KeyStatus::Saved);
    assert_eq!(
        key_status(false, &anthropic, &both),
        KeyStatus::FromEnv("GUARD_EXPLAIN_API_KEY")
    );
    let fallback = env(&[("ANTHROPIC_API_KEY", "b"), ("GUARD_EXPLAIN_API_KEY", " ")]);
    assert_eq!(
        key_status(false, &anthropic, &fallback),
        KeyStatus::FromEnv("ANTHROPIC_API_KEY")
    );
    assert_eq!(key_status(false, &anthropic, env(&[])), KeyStatus::Missing);
    let mut ollama = AiSettings::default();
    ollama.choose(Provider::Ollama);
    assert_eq!(key_status(false, &ollama, env(&[])), KeyStatus::NotRequired);
}

#[test]
fn key_status_ignores_the_environment_for_another_host() {
    let both = env(&[("GUARD_EXPLAIN_API_KEY", "a"), ("ANTHROPIC_API_KEY", "b")]);
    let mut deepseek = AiSettings::default();
    deepseek.choose(Provider::DeepSeek);
    assert_eq!(key_status(false, &deepseek, &both), KeyStatus::Missing);
    assert_eq!(key_status(true, &deepseek, &both), KeyStatus::Saved);
    let mut ollama = AiSettings::default();
    ollama.choose(Provider::Ollama);
    assert_eq!(key_status(false, &ollama, &both), KeyStatus::NotRequired);
}

#[test]
fn redact_hides_a_secret_in_any_text() {
    assert_eq!(
        redact("HTTP 401: bad key sk-1 for sk-1", Some("sk-1")),
        "HTTP 401: bad key [redacted] for [redacted]"
    );
    assert_eq!(redact("nothing here", Some("sk-1")), "nothing here");
    assert_eq!(redact("text", None), "text");
    // A blank secret must not turn every gap into [redacted].
    assert_eq!(redact("a b", Some("")), "a b");
}

#[test]
fn settings_round_trip_through_serde_without_a_key_field() {
    let mut settings = AiSettings::default();
    settings.choose(Provider::Ollama);
    settings.lang = SummaryLang::En;
    let json = serde_json::to_string(&settings).unwrap();
    assert!(!json.to_ascii_lowercase().contains("key"), "{json}");
    let back: AiSettings = serde_json::from_str(&json).unwrap();
    assert_eq!(back, settings);
}

#[test]
fn summarizing_automatically_is_on_by_default_and_survives_old_settings() {
    assert!(AiSettings::default().auto_summarize);
    let old = r#"{"provider":"Anthropic","base_url":"https://api.anthropic.com","model":"m","lang":"Es"}"#;
    let back: AiSettings = serde_json::from_str(old).unwrap();
    assert!(back.auto_summarize);
    let off = AiSettings {
        auto_summarize: false,
        ..AiSettings::default()
    };
    let back: AiSettings = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
    assert!(!back.auto_summarize);
}

#[test]
fn a_finished_run_with_failures_is_summarized_when_a_key_is_there() {
    use AutoSummary::*;
    let on = AiSettings::default();
    assert_eq!(auto_summary(&on, KeyStatus::Saved, false, 2, false), Start);
    assert_eq!(
        auto_summary(&on, KeyStatus::FromEnv(KEY_ENV), false, 1, false),
        Start
    );
    assert_eq!(
        auto_summary(&on, KeyStatus::NotRequired, false, 1, false),
        Start
    );
    // No key for the host: a hint instead of a doomed call.
    assert_eq!(
        auto_summary(&on, KeyStatus::Missing, false, 1, false),
        NeedsKey
    );
    // Nothing failed, the run was cancelled, or the user turned it off.
    assert_eq!(auto_summary(&on, KeyStatus::Saved, false, 0, false), Skip);
    assert_eq!(auto_summary(&on, KeyStatus::Saved, true, 3, false), Skip);
    let off = AiSettings {
        auto_summarize: false,
        ..AiSettings::default()
    };
    assert_eq!(auto_summary(&off, KeyStatus::Saved, false, 3, false), Skip);
}

#[test]
fn a_due_summary_waits_for_an_explanation_in_flight() {
    use AutoSummary::*;
    let on = AiSettings::default();
    assert_eq!(auto_summary(&on, KeyStatus::Saved, false, 2, true), Queue);
    // Nothing to queue when it would not run anyway.
    assert_eq!(auto_summary(&on, KeyStatus::Saved, false, 0, true), Skip);
    assert_eq!(
        auto_summary(&on, KeyStatus::Missing, false, 1, true),
        NeedsKey
    );
}
