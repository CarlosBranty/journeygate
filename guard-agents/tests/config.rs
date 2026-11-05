//! `agents.toml` parsing and validation (docs/contracts/agents-toml.md).

use guard_agents::config::{
    AgentsConfig, ConfigError, LoadOptions, RuntimeKind, Selection, provider_of,
};
use guard_core::ports::Role;
use std::time::Duration;

const FIXTURE: &str = include_str!("fixtures/agents.toml");
const JSON_EXAMPLE: &str = include_str!("../../schemas/examples/agents.example.json");

fn strict() -> LoadOptions {
    LoadOptions {
        allow_fake_runtime: false,
    }
}

fn parse(text: &str) -> Result<AgentsConfig, ConfigError> {
    AgentsConfig::parse(text, &strict())
}

/// The fixture with `from` replaced by `to` (exactly one occurrence).
fn edit(from: &str, to: &str) -> String {
    assert_eq!(FIXTURE.matches(from).count(), 1, "fixture edit {from:?}");
    FIXTURE.replace(from, to)
}

fn problems(text: &str) -> Vec<String> {
    match parse(text) {
        Err(ConfigError::Invalid(p)) => p,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn assert_problem(text: &str, needle: &str) {
    let p = problems(text);
    assert!(
        p.iter().any(|m| m.contains(needle)),
        "no problem containing {needle:?} in {p:#?}"
    );
}

#[test]
fn contract_example_parses_with_defaults() {
    let c = parse(FIXTURE).expect("fixture is valid");
    assert_eq!(c.version, 1);
    assert_eq!(c.gateway.url, "http://llm-gateway:4000");
    assert_eq!(c.gateway.key_env, "GUARD_GATEWAY_KEY");
    assert_eq!(c.roles.qa, "qa");
    let qa = &c.agents["qa"];
    assert_eq!(qa.role, Role::Qa);
    assert_eq!(qa.runtime, RuntimeKind::ClaudeCode);
    assert_eq!(qa.attempt_policy().max_attempts(), 1);
    let copilot = &c.agents["coder-copilot"];
    assert_eq!(copilot.max_attempts, 3, "default max_attempts");
    assert_eq!(copilot.timeout_secs, 1800, "default timeout");
    assert!(copilot.via_gateway);
    assert_eq!(c.scheduler.lease(), Duration::from_secs(300));
    let bon = c.scheduler.best_of_n.as_ref().unwrap();
    assert_eq!(bon.select, Selection::FirstGreenSmallestDiff);
    assert_eq!(bon.label.as_deref(), Some("agent-best-of-n"));
}

#[test]
fn minimal_document_gets_every_default() {
    let text = r#"
version = 1
[gateway]
url = "https://gw.internal"
master_key_env = "LITELLM_MASTER_KEY"
[roles]
qa = "qa"
coder = "coder"
[agents.qa]
role = "qa"
runtime = "codex"
model = "openai/gpt-5"
budget_usd = 1
[agents.coder]
role = "coder"
runtime = "codex"
model = "openai/gpt-5"
budget_usd = 1.5
image = "ghcr.io/acme/sb@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
[scheduler]
host_slots = 1
daily_usd_cap = 10
"#;
    let text = text.replace(
        "[agents.qa]",
        "[agents.qa]\nimage = \"ghcr.io/acme/sb@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\"",
    );
    let c = parse(&text).expect("minimal is valid");
    assert_eq!(c.gateway.key_env, "GUARD_GATEWAY_KEY");
    assert_eq!(c.gateway.key_duration_secs, 7200);
    assert_eq!(c.scheduler.lease_secs, 300);
    assert!(c.scheduler.provider_slots.is_empty());
    assert!(c.scheduler.best_of_n.is_none());
    assert_eq!(c.agents["coder"].max_attempts, 3);
}

#[test]
fn json_example_is_the_same_document() {
    // The schema example and the TOML fixture describe the same config, so the
    // Rust types, the TOML form and the JSON form stay bound together.
    let from_json: AgentsConfig = serde_json::from_str(JSON_EXAMPLE).expect("json example");
    from_json
        .validate(&strict())
        .expect("json example validates");
    assert_eq!(from_json, parse(FIXTURE).unwrap());
}

#[test]
fn unknown_keys_are_errors() {
    for (from, to) in [
        ("budget_usd = 2.0", "budget_usd = 2.0\nbudget = 3"),
        ("host_slots = 4", "host_slots = 4\nslots = 2"),
        ("version = 1", "version = 1\nextra = true"),
        (
            "select = \"first_green_smallest_diff\"",
            "select = \"best\"",
        ),
    ] {
        assert!(
            matches!(parse(&edit(from, to)), Err(ConfigError::Toml(_))),
            "{to}"
        );
    }
}

#[test]
fn every_rule_beyond_the_schema_is_enforced() {
    let cases: &[(&str, &str, &str)] = &[
        ("version = 1", "version = 2", "version"),
        (
            "url = \"http://llm-gateway:4000\"",
            "url = \"ftp://gw\"",
            "gateway.url",
        ),
        (
            "master_key_env = \"LITELLM_MASTER_KEY\"",
            "master_key_env = \"litellm\"",
            "master_key_env",
        ),
        (
            "key_duration_secs = 7200",
            "key_duration_secs = 10",
            "key_duration_secs",
        ),
        ("rpm_limit = 60", "rpm_limit = 0", "rpm_limit"),
        ("qa = \"qa\"", "qa = \"ghost\"", "roles.qa"),
        ("coder = \"coder\"", "coder = \"qa\"", "roles.coder"),
        ("budget_usd = 2.0", "budget_usd = 0.0", "budget_usd"),
        ("max_attempts = 1", "max_attempts = 4", "max_attempts"),
        ("timeout_secs = 1800", "timeout_secs = 5", "timeout_secs"),
        (
            "model = \"claude-opus-5-5\"",
            "model = \"openai/gpt-5\"",
            "claude-code",
        ),
        (
            "via_gateway = true",
            "via_gateway = false",
            "copilot_token_env",
        ),
        ("host_slots = 4", "host_slots = 0", "host_slots"),
        (
            "daily_usd_cap = 50.0",
            "daily_usd_cap = 0.0",
            "daily_usd_cap",
        ),
        ("lease_secs = 300", "lease_secs = 5", "lease_secs"),
        ("openai = 3 }", "openai = 0 }", "provider_slots"),
        (
            "agents = [\"coder\", \"coder-copilot\"]",
            "agents = [\"coder\", \"qa\"]",
            "share a role",
        ),
        (
            "agents = [\"coder\", \"coder-copilot\"]",
            "agents = [\"coder\"]",
            "at least 2",
        ),
        (
            "agents = [\"coder\", \"coder-copilot\"]",
            "agents = [\"coder\", \"coder\"]",
            "unique",
        ),
        (
            "agents = [\"coder\", \"coder-copilot\"]",
            "agents = [\"coder\", \"nobody\"]",
            "nobody",
        ),
        ("host_slots = 4", "host_slots = 1", "best_of_n"),
        (
            "image = \"ghcr.io/acme/guard-sandbox@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"",
            "image = \"ghcr.io/acme/guard-sandbox:latest\"",
            "image",
        ),
        ("[agents.qa]", "[agents.Bad_Name]", "Bad_Name"),
    ];
    for (from, to, needle) in cases {
        assert_problem(&edit(from, to), needle);
    }
}

#[test]
fn via_gateway_false_needs_copilot_cli() {
    let text = edit(
        "runtime = \"mini-swe-agent\"",
        "runtime = \"mini-swe-agent\"\nvia_gateway = false\ncopilot_token_env = \"COPILOT_GITHUB_TOKEN\"",
    );
    assert_problem(&text, "copilot-cli");
    let ok = edit(
        "via_gateway = true",
        "via_gateway = false\ncopilot_token_env = \"COPILOT_GITHUB_TOKEN\"",
    );
    let c = parse(&ok).expect("copilot exception is valid");
    assert!(!c.agents["coder-copilot"].via_gateway);
}

#[test]
fn every_agent_needs_an_image() {
    let text = FIXTURE
        .lines()
        .filter(|l| !l.starts_with("image = "))
        .collect::<Vec<_>>()
        .join("\n");
    assert_problem(&text, "no sandbox image");
}

#[test]
fn fake_runtime_needs_the_explicit_opt_in() {
    let text = edit("runtime = \"claude-code\"", "runtime = \"fake\"");
    assert_problem(&text, "GUARD_ALLOW_FAKE_RUNTIME");
    let c = AgentsConfig::parse(
        &text,
        &LoadOptions {
            allow_fake_runtime: true,
        },
    )
    .expect("fake allowed with the opt-in");
    assert_eq!(c.agents["qa"].runtime, RuntimeKind::Fake);
}

#[test]
fn all_problems_are_reported_at_once() {
    let text = edit("host_slots = 4", "host_slots = 0").replace("version = 1", "version = 9");
    assert!(problems(&text).len() >= 2);
}

#[test]
fn load_from_file_and_from_env_options() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agents.toml");
    std::fs::write(&path, FIXTURE).unwrap();
    assert!(AgentsConfig::load(&path, &strict()).is_ok());
    assert!(matches!(
        AgentsConfig::load(&dir.path().join("missing.toml"), &strict()),
        Err(ConfigError::Io(_))
    ));
    assert!(LoadOptions::from_env_value(Some("1")).allow_fake_runtime);
    assert!(!LoadOptions::from_env_value(Some("true")).allow_fake_runtime);
    assert!(!LoadOptions::from_env_value(None).allow_fake_runtime);
}

#[test]
fn provider_prefixes() {
    assert_eq!(provider_of("anthropic/claude-sonnet-4-5"), "anthropic");
    assert_eq!(provider_of("openai/gpt-5"), "openai");
    assert_eq!(provider_of("claude-opus-5-5"), "anthropic");
    assert_eq!(provider_of("gpt-5"), "gpt-5");
}

#[test]
fn helpers_resolve_roles_images_gateway_and_keys() {
    let c = parse(FIXTURE).unwrap();
    let (name, agent) = c.role_agent(Role::Coder);
    assert_eq!(name, "coder");
    assert_eq!(agent.runtime, RuntimeKind::MiniSweAgent);
    assert!(c.image_for(agent).ends_with(&"a".repeat(64)));
    assert_eq!(
        c.gateway_endpoint().unwrap(),
        ("llm-gateway".to_string(), 4000)
    );
    let req = c.key_request("coder", "run-1:coder:1", Default::default());
    assert_eq!(req.models, vec!["openai/gpt-5".to_string()]);
    assert_eq!(req.max_budget_usd, 5.0);
    assert_eq!(req.rpm_limit, Some(60));
    assert_eq!(req.tpm_limit, Some(400000));
    assert_eq!(req.max_parallel_requests, Some(2));
    assert_eq!(req.duration, Duration::from_secs(7200));
    req.validate().unwrap();
    let runtimes = c.runtimes_in_use();
    assert!(runtimes.contains(&RuntimeKind::ClaudeCode));
    assert!(runtimes.contains(&RuntimeKind::CopilotCli));
    assert_eq!(RuntimeKind::CopilotCli.as_str(), "copilot-cli");
}

#[test]
fn https_gateway_defaults_to_port_443() {
    let c = parse(&edit(
        "url = \"http://llm-gateway:4000\"",
        "url = \"https://gw.example.com/v1\"",
    ))
    .unwrap();
    assert_eq!(
        c.gateway_endpoint().unwrap(),
        ("gw.example.com".to_string(), 443)
    );
}
