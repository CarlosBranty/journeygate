use super::*;

const FULL: &str = r#"
version = 1

[app]
name = "shop"
languages = ["typescript", "go"]
start = "docker compose up -d --wait"
stop = "docker compose down -v"
base_url = "http://localhost:3000"
ready_path = "/healthz"
preview_url = "https://pr-{pr}.preview.example.com"

[testability]
login_path = "/__test__/login"
seed_path = "/__test__/seed"
token_env = "GUARD_TEST_TOKEN"
roles = ["customer", "admin"]
test_id_attribute = "data-testid"

[areas.checkout]
description = "Cart, payment and order confirmation"
paths = ["web/src/checkout/**", "services/orders/**", "services/payments/**"]
sensitive = true

[areas.inventory]
description = "Stock levels, reservations and restock"
paths = ["services/inventory/**"]

[areas.ui_general]
description = "Shared UI components and copy"
paths = ["web/src/components/**"]

[hard_rules]
human_only_paths = ["**/migrations/**", "infra/**"]
max_fan_out_for_agent = 10

[network]
blocked_domains = ["*.google-analytics.com", "*.intercom.io"]
recorded_hosts = ["api.payments-sandbox.example.com"]

[evidence]
video = "on-first-retry"
retention_days = 14
storage_url_env = "GUARD_STORAGE_URL"

[tracker]
kind = "jira"
project = "SHOP"
ticket_pattern = 'SHOP-\d+'
require_ticket = "warn"

[code_host]
kind = "github"

[server]
url = "https://guard.example.com"
"#;

const MINIMAL: &str = r#"
version = 1

[app]
name = "demo-shop"
base_url = "http://localhost:3000"

[areas.checkout]
description = "Cart, payment and order confirmation"
paths = ["examples/demo-shop/**"]
"#;

fn minimal_with(extra: &str) -> String {
    format!("{MINIMAL}\n{extra}")
}

fn invalid(text: &str) -> Vec<ValidationError> {
    match parse_target(text) {
        Err(ConfigError::Invalid(errors)) => errors,
        other => panic!("expected validation errors, got {other:?}"),
    }
}

fn has_error(errors: &[ValidationError], field: &str) -> bool {
    errors.iter().any(|e| e.field == field)
}

#[test]
fn parses_the_full_example() {
    let config = parse_target(FULL).expect("full example is valid");
    assert_eq!(config.app.name, "shop");
    assert_eq!(config.app.languages, vec!["typescript", "go"]);
    assert_eq!(
        config.app.start.as_deref(),
        Some("docker compose up -d --wait")
    );
    assert_eq!(config.testability.roles, vec!["customer", "admin"]);
    assert_eq!(config.areas.len(), 3);
    assert!(config.areas["checkout"].sensitive);
    assert!(!config.areas["inventory"].sensitive);
    assert_eq!(config.network.blocked_domains.len(), 2);
    assert_eq!(config.evidence.video, Video::OnFirstRetry);
    let tracker = config.tracker.as_ref().expect("tracker present");
    assert_eq!(tracker.kind, TrackerKind::Jira);
    assert_eq!(tracker.ticket_pattern.as_deref(), Some(r"SHOP-\d+"));
    assert_eq!(tracker.require_ticket, RequireTicket::Warn);
    assert_eq!(
        config.code_host.as_ref().map(|c| c.kind),
        Some(CodeHostKind::Github)
    );
    assert_eq!(
        config.server.as_ref().map(|s| s.url.as_str()),
        Some("https://guard.example.com")
    );
}

#[test]
fn minimal_file_takes_every_default() {
    let config = parse_target(MINIMAL).expect("minimal example is valid");
    assert_eq!(config.app.ready_path, "/healthz");
    assert!(config.app.languages.is_empty());
    assert_eq!(config.app.start, None);
    assert_eq!(config.testability, Testability::default());
    assert_eq!(config.testability.login_path, "/__test__/login");
    assert_eq!(config.testability.seed_path, "/__test__/seed");
    assert_eq!(config.testability.token_env, "GUARD_TEST_TOKEN");
    assert_eq!(config.testability.test_id_attribute, "data-testid");
    assert_eq!(config.hard_rules.max_fan_out_for_agent, 10);
    assert!(config.network.blocked_domains.is_empty());
    assert_eq!(config.evidence.video, Video::OnFirstRetry);
    assert_eq!(config.evidence.retention_days, 14);
    assert_eq!(config.evidence.storage_url_env, "GUARD_STORAGE_URL");
    assert!(config.tracker.is_none());
    assert!(config.code_host.is_none());
    assert!(config.server.is_none());
}

#[test]
fn ready_url_joins_base_url_and_ready_path() {
    let config = parse_target(MINIMAL).unwrap();
    assert_eq!(config.ready_url(), "http://localhost:3000/healthz");
}

#[test]
fn unknown_keys_are_a_parse_error() {
    let text = MINIMAL.replace("base_url", "base_ulr");
    match parse_target(&text) {
        Err(ConfigError::Parse(message)) => assert!(message.contains("base_ulr"), "{message}"),
        other => panic!("expected parse error, got {other:?}"),
    }
}

#[test]
fn invalid_toml_is_a_parse_error() {
    assert!(matches!(
        parse_target("version = "),
        Err(ConfigError::Parse(_))
    ));
}

#[test]
fn only_version_one_is_supported() {
    let errors = invalid(&MINIMAL.replace("version = 1", "version = 2"));
    assert_eq!(errors[0].field, "version");
    assert_eq!(errors[0].kind, ValidationErrorKind::UnsupportedVersion(2));
}

#[test]
fn app_name_must_match_the_pattern() {
    let errors = invalid(&MINIMAL.replace("demo-shop", "Demo Shop"));
    assert!(matches!(
        &errors[0].kind,
        ValidationErrorKind::PatternMismatch { value, .. } if value == "Demo Shop"
    ));
    assert_eq!(errors[0].field, "app.name");
}

#[test]
fn base_url_must_be_http_or_https() {
    let errors = invalid(&MINIMAL.replace("http://localhost:3000", "localhost:3000"));
    assert!(has_error(&errors, "app.base_url"));
}

#[test]
fn ready_path_must_start_with_a_slash() {
    let text = MINIMAL.replace(
        "base_url = \"http://localhost:3000\"",
        "base_url = \"http://localhost:3000\"\nready_path = \"healthz\"",
    );
    assert!(has_error(&invalid(&text), "app.ready_path"));
}

#[test]
fn at_least_one_area_is_required() {
    let text = r#"
version = 1
areas = {}

[app]
name = "x"
base_url = "http://localhost:3000"
"#;
    let errors = invalid(text);
    assert_eq!(errors[0].field, "areas");
    assert_eq!(errors[0].kind, ValidationErrorKind::AreaCount(0));
}

#[test]
fn area_keys_must_be_snake_case() {
    let errors = invalid(&MINIMAL.replace("[areas.checkout]", "[areas.Checkout]"));
    assert!(has_error(&errors, "areas.Checkout"));
}

#[test]
fn area_paths_must_be_unique_and_non_empty() {
    let errors = invalid(&MINIMAL.replace(
        r#"paths = ["examples/demo-shop/**"]"#,
        r#"paths = ["a/**", "a/**"]"#,
    ));
    assert_eq!(errors[0].field, "areas.checkout.paths");
    assert_eq!(
        errors[0].kind,
        ValidationErrorKind::Duplicate("a/**".into())
    );

    let errors = invalid(&MINIMAL.replace(r#"paths = ["examples/demo-shop/**"]"#, "paths = []"));
    assert_eq!(errors[0].field, "areas.checkout.paths");
    assert_eq!(errors[0].kind, ValidationErrorKind::Empty);
}

#[test]
fn area_globs_must_compile() {
    let errors = invalid(&MINIMAL.replace("examples/demo-shop/**", "web/[a"));
    assert!(matches!(
        &errors[0].kind,
        ValidationErrorKind::InvalidGlob { glob, .. } if glob == "web/[a"
    ));
}

#[test]
fn env_fields_hold_names_not_secrets() {
    let errors = invalid(&minimal_with("[testability]\ntoken_env = \"s3cr3t-value\""));
    assert!(has_error(&errors, "testability.token_env"));
}

#[test]
fn roles_and_domains_are_checked() {
    let text = minimal_with(
        "[testability]\nroles = [\"Admin\", \"x\", \"x\"]\n[network]\nblocked_domains = [\"https://x.com\"]",
    );
    let errors = invalid(&text);
    assert!(has_error(&errors, "testability.roles"));
    assert!(has_error(&errors, "network.blocked_domains"));
    assert!(errors.len() >= 3, "{errors:?}");
}

#[test]
fn numeric_minimums_are_enforced() {
    let errors = invalid(&minimal_with("[evidence]\nretention_days = 0"));
    assert_eq!(
        errors[0].kind,
        ValidationErrorKind::BelowMinimum {
            value: 0,
            minimum: 1
        }
    );
    assert_eq!(errors[0].field, "evidence.retention_days");
}

#[test]
fn enums_reject_unknown_values() {
    assert!(matches!(
        parse_target(&minimal_with("[evidence]\nvideo = \"always\"")),
        Err(ConfigError::Parse(_))
    ));
    assert!(matches!(
        parse_target(&minimal_with("[code_host]\nkind = \"bitbucket\"")),
        Err(ConfigError::Parse(_))
    ));
}

#[test]
fn ticket_regex_compiles_or_reports_a_typed_error() {
    let config = parse_target(FULL).unwrap();
    let re = config.ticket_regex().unwrap().expect("pattern present");
    assert!(re.is_match("SHOP-1-x"));

    let config = parse_target(MINIMAL).unwrap();
    assert!(config.ticket_regex().unwrap().is_none());

    let config = parse_target(&minimal_with(
        "[tracker]\nkind = \"jira\"\nticket_pattern = '(['",
    ))
    .unwrap();
    let error = config.ticket_regex().unwrap_err();
    assert_eq!(error.field, "tracker.ticket_pattern");
    assert!(matches!(error.kind, ValidationErrorKind::InvalidRegex(_)));
}

#[test]
fn validation_errors_render_field_and_message() {
    let error = ValidationError {
        field: "app.name".into(),
        kind: ValidationErrorKind::PatternMismatch {
            value: "Bad".into(),
            pattern: "^[a-z]$",
        },
    };
    assert_eq!(
        error.to_string(),
        "app.name: \"Bad\" does not match ^[a-z]$"
    );
}

#[test]
fn lint_reports_dead_globs() {
    let config = parse_target(FULL).unwrap();
    let tracked = vec![
        "web/src/checkout/cart.ts".to_string(),
        "services/orders/main.go".to_string(),
        "web/src/components/button.tsx".to_string(),
    ];
    let warnings = lint(&config, &tracked);
    let dead: Vec<String> = warnings.iter().map(|w| w.to_string()).collect();
    assert!(
        dead.contains(&"dead glob in areas.checkout.paths: services/payments/**".to_string()),
        "{dead:?}"
    );
    assert!(
        dead.contains(&"dead glob in areas.inventory.paths: services/inventory/**".to_string()),
        "{dead:?}"
    );
    assert!(!dead.iter().any(|w| w.contains("web/src/checkout/**")));
}

#[test]
fn lint_reports_files_matched_by_two_areas_with_the_sensitive_winner() {
    let text = FULL.replace(
        r#"paths = ["web/src/components/**"]"#,
        r#"paths = ["web/src/**"]"#,
    );
    let config = parse_target(&text).unwrap();
    let tracked = vec![
        "web/src/checkout/cart.ts".to_string(),
        "web/src/checkout/pay.ts".to_string(),
    ];
    let overlap = lint(&config, &tracked)
        .into_iter()
        .find(|w| matches!(w, ConfigWarning::OverlappingAreas { .. }))
        .expect("overlap warning");
    assert_eq!(
        overlap,
        ConfigWarning::OverlappingAreas {
            first: "checkout".into(),
            second: "ui_general".into(),
            files: 2,
            example: "web/src/checkout/cart.ts".into(),
            winner: Some("checkout".into()),
        }
    );
    assert_eq!(
        overlap.to_string(),
        "2 files matched by areas checkout and ui_general (e.g. web/src/checkout/cart.ts); sensitive area checkout wins"
    );
}

#[test]
fn lint_reports_todo_descriptions() {
    let text = MINIMAL.replace("Cart, payment and order confirmation", "TODO: describe web");
    let config = parse_target(&text).unwrap();
    let warnings = lint(&config, &["examples/demo-shop/a.js".to_string()]);
    assert_eq!(
        warnings,
        vec![ConfigWarning::TodoDescription {
            area: "checkout".into()
        }]
    );
    assert_eq!(
        warnings[0].to_string(),
        "areas.checkout.description still starts with TODO"
    );
}

#[test]
fn services_are_absent_by_default_and_parse_in_file_order() {
    assert!(parse_target(MINIMAL).unwrap().services.is_empty());
    let config = parse_target(&minimal_with(
        r#"
[[services]]
name = "backend"
cwd = "apps/backend"
start = "bun run dev"
stop = "bun run clean"
ready_url = "http://localhost:3001/health"

[[services]]
name = "frontend"
start = "npm run dev"
"#,
    ))
    .expect("services are valid");
    assert_eq!(
        config.services,
        vec![
            Service {
                name: "backend".into(),
                cwd: "apps/backend".into(),
                start: "bun run dev".into(),
                stop: Some("bun run clean".into()),
                ready_url: Some("http://localhost:3001/health".into()),
            },
            Service {
                name: "frontend".into(),
                cwd: ".".into(),
                start: "npm run dev".into(),
                stop: None,
                ready_url: None,
            },
        ]
    );
}

#[test]
fn services_reject_bad_names_cwds_commands_and_urls() {
    let errors = invalid(&minimal_with(
        r#"
[[services]]
name = "Backend"
cwd = "../outside"
start = ""
stop = ""
ready_url = "ftp://x"

[[services]]
name = "web"
cwd = "/abs"
start = "a"

[[services]]
name = "web"
cwd = 'apps\web'
start = "b"

[[services]]
name = "ok"
cwd = "apps/.hidden"
start = "c"
"#,
    ));
    for field in [
        "services[0].name",
        "services[0].cwd",
        "services[0].start",
        "services[0].stop",
        "services[0].ready_url",
        "services[1].cwd",
        "services[2].cwd",
        "services[3].cwd",
        "services.name",
    ] {
        assert!(has_error(&errors, field), "{field}: {errors:?}");
    }
    assert!(errors.contains(&ValidationError {
        field: "services.name".into(),
        kind: ValidationErrorKind::Duplicate("web".into()),
    }));
    let missing_start = parse_target(&minimal_with("[[services]]\nname = \"x\"\n"));
    assert!(matches!(missing_start, Err(ConfigError::Parse(_))));
    let unknown_key = parse_target(&minimal_with(
        "[[services]]\nname = \"x\"\nstart = \"y\"\nport = 1\n",
    ));
    assert!(matches!(unknown_key, Err(ConfigError::Parse(_))));
}

#[test]
fn service_cwd_accepts_the_root_and_nested_directories() {
    for cwd in [".", "backend", "apps/web-1", "a/b_c/d.e", "@scope/pkg"] {
        let text = minimal_with(&format!(
            "[[services]]\nname = \"s\"\ncwd = \"{cwd}\"\nstart = \"x\"\n"
        ));
        assert!(parse_target(&text).is_ok(), "{cwd}");
    }
}

#[test]
fn lint_treats_a_subtree_entry_as_live_but_never_as_an_overlap() {
    let text = FULL.replace(
        r#"paths = ["web/src/components/**"]"#,
        r#"paths = ["web/src/**"]"#,
    );
    let config = parse_target(&text).unwrap();
    let tracked = vec!["web/".to_string(), "services/".to_string()];
    let warnings = lint(&config, &tracked);
    let rendered: Vec<String> = warnings.iter().map(|w| w.to_string()).collect();
    assert!(
        !rendered.iter().any(|w| w.starts_with("dead glob")),
        "{rendered:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| matches!(w, ConfigWarning::OverlappingAreas { .. })),
        "{rendered:?}"
    );
}
