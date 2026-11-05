use super::*;
use crate::config::parse_target;

const TARGET: &str = r#"
version = 1
[app]
name = "shop"
base_url = "http://localhost:3000"
[testability]
roles = ["customer", "admin"]
[network]
blocked_domains = ["*.google-analytics.com", "*.intercom.io"]
[areas.checkout]
description = "Checkout"
paths = ["web/**"]
"#;

fn value<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn builds_every_runner_variable() {
    let config = parse_target(TARGET).unwrap();
    let env = runner_env(&RunnerEnvInput {
        config: &config,
        e2e_tags: "@journey:a.b,@smoke",
        run_id: "r1-2",
        token: Some("dev-token"),
        docker: false,
    });
    let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "E2E_TAGS",
            "TARGET_BASE_URL",
            "RUN_ID",
            "GUARD_TEST_TOKEN",
            "GUARD_LOGIN_PATH",
            "GUARD_SEED_PATH",
            "GUARD_VIDEO",
            "GUARD_ROLES",
            "GUARD_BLOCKED_DOMAINS"
        ]
    );
    assert_eq!(value(&env, "E2E_TAGS"), Some("@journey:a.b,@smoke"));
    assert_eq!(
        value(&env, "TARGET_BASE_URL"),
        Some("http://localhost:3000")
    );
    assert_eq!(value(&env, "RUN_ID"), Some("r1-2"));
    assert_eq!(value(&env, "GUARD_TEST_TOKEN"), Some("dev-token"));
    assert_eq!(value(&env, "GUARD_LOGIN_PATH"), Some("/__test__/login"));
    assert_eq!(value(&env, "GUARD_SEED_PATH"), Some("/__test__/seed"));
    assert_eq!(value(&env, "GUARD_VIDEO"), Some("on-first-retry"));
    assert_eq!(value(&env, "GUARD_ROLES"), Some("customer,admin"));
    assert_eq!(
        value(&env, "GUARD_BLOCKED_DOMAINS"),
        Some("*.google-analytics.com,*.intercom.io")
    );
}

#[test]
fn token_is_omitted_when_unset_and_empty_lists_are_empty_strings() {
    let text = r#"
version = 1
[app]
name = "x"
base_url = "http://127.0.0.1:8080"
[evidence]
video = "off"
[areas.a]
description = "A"
paths = ["**"]
"#;
    let config = parse_target(text).unwrap();
    let env = runner_env(&RunnerEnvInput {
        config: &config,
        e2e_tags: "@smoke",
        run_id: "x",
        token: None,
        docker: true,
    });
    assert_eq!(value(&env, "GUARD_TEST_TOKEN"), None);
    assert_eq!(value(&env, "GUARD_ROLES"), Some(""));
    assert_eq!(value(&env, "GUARD_BLOCKED_DOMAINS"), Some(""));
    assert_eq!(value(&env, "GUARD_VIDEO"), Some("off"));
    assert_eq!(
        value(&env, "TARGET_BASE_URL"),
        Some("http://host.docker.internal:8080")
    );
}

#[test]
fn docker_rewrites_only_loopback_hosts() {
    assert_eq!(
        docker_base_url("http://localhost:3000"),
        "http://host.docker.internal:3000"
    );
    assert_eq!(
        docker_base_url("http://localhost"),
        "http://host.docker.internal"
    );
    assert_eq!(
        docker_base_url("https://127.0.0.1/app"),
        "https://host.docker.internal/app"
    );
    assert_eq!(
        docker_base_url("http://localhost.example.com"),
        "http://localhost.example.com"
    );
    assert_eq!(
        docker_base_url("http://shop.test:3000"),
        "http://shop.test:3000"
    );
}

#[test]
fn run_ids_are_generated_and_validated() {
    assert_eq!(generate_run_id(1_793_452_800, 4242), "r1793452800-4242");
    assert!(is_valid_run_id("12345-1"));
    assert!(is_valid_run_id("r1793452800-4242"));
    assert!(!is_valid_run_id(""));
    assert!(!is_valid_run_id("has space"));
    assert!(!is_valid_run_id(&"x".repeat(65)));
}

#[test]
fn resolve_run_id_forwards_an_inherited_value_unchanged() {
    assert_eq!(resolve_run_id(Some("12345-1".into()), 1, 2), "12345-1");
    assert_eq!(resolve_run_id(None, 1, 2), "r1-2");
    assert_eq!(resolve_run_id(Some(String::new()), 1, 2), "r1-2");
}

#[test]
fn exports_custom_login_and_seed_paths() {
    let text = TARGET.replace(
        "[testability]
",
        "[testability]
login_path = \"/qa/login\"
seed_path = \"/qa/seed\"
",
    );
    let config = parse_target(&text).unwrap();
    let env = runner_env(&RunnerEnvInput {
        config: &config,
        e2e_tags: "",
        run_id: "r1-2",
        token: None,
        docker: false,
    });
    assert_eq!(value(&env, "GUARD_LOGIN_PATH"), Some("/qa/login"));
    assert_eq!(value(&env, "GUARD_SEED_PATH"), Some("/qa/seed"));
}
