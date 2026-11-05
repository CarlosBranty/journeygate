use super::*;

#[test]
fn wide_strings_are_nul_terminated_utf16() {
    assert_eq!(wide("ab"), vec![b'a' as u16, b'b' as u16, 0]);
    assert_eq!(wide(""), vec![0]);
}

#[test]
fn each_host_has_its_own_target_name() {
    assert_eq!(
        target("api.anthropic.com"),
        "guard-gui/explain-api-key/api.anthropic.com"
    );
    assert_eq!(
        target("localhost:11434"),
        "guard-gui/explain-api-key/localhost:11434"
    );
    assert_ne!(target("api.anthropic.com"), target("api.deepseek.com"));
}

#[test]
fn jira_tokens_have_their_own_targets_apart_from_the_explain_keys() {
    assert_eq!(
        jira_target("acme.atlassian.net"),
        "guard-gui/jira-api-token/acme.atlassian.net"
    );
    assert_ne!(
        jira_target("acme.atlassian.net"),
        target("acme.atlassian.net")
    );
}

/// Writes, reads and deletes a throwaway entry (never the real target) in
/// the Windows Credential Manager. The value is a dummy, not a key.
#[cfg(windows)]
#[test]
fn a_secret_round_trips_through_the_credential_manager() {
    let target = format!("guard-gui/test-{}", std::process::id());
    let _ = delete(&target);
    assert_eq!(read(&target).unwrap(), None);
    write(&target, "dummy-value-not-a-key").unwrap();
    assert_eq!(
        read(&target).unwrap().as_deref(),
        Some("dummy-value-not-a-key")
    );
    delete(&target).unwrap();
    assert_eq!(read(&target).unwrap(), None);
    // Deleting what is not there is not an error.
    delete(&target).unwrap();
}

#[cfg(not(windows))]
#[test]
fn other_platforms_report_no_store() {
    let t = target("api.anthropic.com");
    assert!(read(&t).is_err());
    assert!(write(&t, "x").is_err());
}
