use crate::common::coder_task;
use guard_core::ports::fakes::FakeSandboxProvider;
use guard_runtimes::{Admission, RUNTIME_NAMES, admission, runtime_for, secret_env_aliases};
use std::sync::Arc;

#[test]
fn every_agents_toml_runtime_resolves() {
    let provider = Arc::new(FakeSandboxProvider::new());
    assert_eq!(
        RUNTIME_NAMES,
        [
            "claude-code",
            "codex",
            "copilot-cli",
            "mini-swe-agent",
            "openhands",
            "fake"
        ]
    );
    for name in RUNTIME_NAMES {
        let rt = runtime_for(name, provider.clone()).unwrap();
        assert_eq!(rt.name(), name);
    }
    assert!(runtime_for("aider", provider).is_none());
}

#[test]
fn admission_marks_what_is_unconfirmed() {
    assert_eq!(admission("claude-code"), Some(Admission::Admitted));
    assert_eq!(admission("codex"), Some(Admission::Admitted));
    assert_eq!(admission("mini-swe-agent"), Some(Admission::Admitted));
    assert_eq!(admission("copilot-cli"), Some(Admission::Provisional));
    assert_eq!(admission("openhands"), Some(Admission::Provisional));
    assert_eq!(admission("fake"), Some(Admission::TestOnly));
    assert_eq!(admission("nope"), None);
}

#[test]
fn only_runtimes_that_cannot_read_key_env_need_aliases() {
    let t = coder_task();
    for name in ["claude-code", "codex", "copilot-cli", "openhands", "fake"] {
        assert!(secret_env_aliases(name, &t).is_empty(), "{name}");
    }
}
