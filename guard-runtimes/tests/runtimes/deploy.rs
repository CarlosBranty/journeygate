//! `deploy/litellm/config.yaml` structure (docs/contracts/tokens.md). A
//! line-based check (no YAML parser in the workspace): every model block is
//! split on `- model_name:` and inspected.

use std::path::PathBuf;

fn config() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../deploy/litellm/config.yaml");
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
        .replace("\r\n", "\n")
}

fn strip_comments(s: &str) -> String {
    s.lines()
        .map(|l| match l.find(" #") {
            Some(i) => &l[..i],
            None if l.trim_start().starts_with('#') => "",
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn model_blocks(cfg: &str) -> Vec<(String, String)> {
    let list = cfg
        .split("\nlitellm_settings:")
        .next()
        .unwrap()
        .split("model_list:")
        .nth(1)
        .expect("model_list");
    list.split("- model_name:")
        .skip(1)
        .map(|b| {
            let name = b.lines().next().unwrap().trim().to_string();
            (name, b.to_string())
        })
        .collect()
}

#[test]
fn injection_points_only_on_native_anthropic_deployments() {
    let cfg = strip_comments(&config());
    let blocks = model_blocks(&cfg);
    assert!(blocks.len() >= 3, "{blocks:?}");
    let mut anthropic = 0;
    for (name, body) in &blocks {
        let upstream = body
            .lines()
            .find_map(|l| l.trim().strip_prefix("model: "))
            .unwrap_or_else(|| panic!("{name}: litellm_params.model"));
        let has_points = body.contains("cache_control_injection_points:");
        if upstream.starts_with("anthropic/") {
            anthropic += 1;
            assert!(
                has_points,
                "{name}: anthropic/ deployments inject cache_control"
            );
            assert!(body.contains("location: message") && body.contains("role: system"));
        } else {
            assert!(
                !has_points,
                "{name}: injection points are for anthropic/ only"
            );
        }
    }
    assert!(anthropic >= 1);
}

#[test]
fn provider_keys_are_env_references_only() {
    let cfg = strip_comments(&config());
    for l in cfg.lines() {
        let t = l.trim();
        for key in ["api_key:", "master_key:", "password:", "database_url:"] {
            if let Some(v) = t.strip_prefix(key) {
                assert!(v.trim().starts_with("os.environ/"), "{t}");
            }
        }
    }
}

#[test]
fn exact_cache_is_default_off_and_otel_is_on() {
    let cfg = strip_comments(&config());
    assert!(cfg.contains("mode: default_off"));
    assert!(cfg.contains("callbacks: [\"otel\"]"));
    assert!(
        !cfg.contains("semantic"),
        "semantic cache is rejected (architecture 19.4)"
    );
}

#[test]
fn spike_note_says_unverified() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../deploy/litellm/context-editing-spike.md");
    let note = std::fs::read_to_string(p).unwrap();
    assert!(note.contains("Status: unverified"));
}
