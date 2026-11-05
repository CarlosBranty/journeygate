use super::*;

const TARGET: &str = "# my repo\nversion = 1\n\n[app]\nname = \"shop\"\nbase_url = \"http://localhost:3000\"\n\n[areas.shop]\ndescription = \"Shop\" # keep\npaths = [\"src/**\"]\n";

fn roles(v: &[&str]) -> Vec<String> {
    v.iter().map(|r| r.to_string()).collect()
}

fn edit(login: &str, token: &str, r: &[&str]) -> TestabilityEdit {
    TestabilityEdit {
        login_path: login.into(),
        token_env: token.into(),
        roles: roles(r),
    }
}

#[test]
fn roles_follow_the_contract_pattern() {
    for ok in ["admin", "customer", "a", "back-office", "role_2"] {
        assert!(check_role(ok).is_ok(), "{ok}");
    }
    for bad in ["", "Admin", "2fa", "-x", "a b", "ñandu", "admin!"] {
        assert!(check_role(bad).is_err(), "{bad}");
    }
}

#[test]
fn adding_a_role_trims_it_and_rejects_duplicates() {
    assert_eq!(
        add_role(&roles(&["admin"]), "  customer ").unwrap(),
        roles(&["admin", "customer"])
    );
    let dup = add_role(&roles(&["admin"]), "admin").unwrap_err();
    assert!(dup.contains("admin") && dup.contains("ya"), "{dup}");
    assert!(add_role(&[], "Admin").is_err());
}

#[test]
fn removing_a_role_keeps_the_others_in_order() {
    assert_eq!(
        remove_role(&roles(&["a", "b", "c"]), "b"),
        roles(&["a", "c"])
    );
    assert_eq!(remove_role(&roles(&["a"]), "z"), roles(&["a"]));
}

#[test]
fn login_path_and_token_env_follow_the_contract() {
    assert!(check_login_path("/__test__/login").is_ok());
    assert!(check_login_path("/").is_ok());
    for bad in ["", "__test__/login", "/a b", "https://x/login"] {
        assert!(check_login_path(bad).is_err(), "{bad}");
    }
    assert!(check_token_env("GUARD_TEST_TOKEN").is_ok());
    assert!(check_token_env("_X1").is_ok());
    for bad in ["", "guard_token", "1TOKEN", "MY-TOKEN", "s3cr3t-value"] {
        assert!(check_token_env(bad).is_err(), "{bad}");
    }
}

#[test]
fn credential_variables_are_named_after_the_role() {
    assert_eq!(
        credential_env_names("admin"),
        (
            "E2E_ADMIN_EMAIL".to_string(),
            "E2E_ADMIN_PASSWORD".to_string()
        )
    );
    assert_eq!(
        credential_env_names("back-office").0,
        "E2E_BACK_OFFICE_EMAIL"
    );
    assert_eq!(credential_env_names("role_2").1, "E2E_ROLE_2_PASSWORD");
}

#[test]
fn credential_status_reports_presence_only() {
    let set = |name: &str| name == "E2E_ADMIN_EMAIL";
    assert_eq!(
        credential_status("admin", set),
        CredentialStatus {
            email: true,
            password: false
        }
    );
    assert_eq!(
        credential_status("customer", set),
        CredentialStatus {
            email: false,
            password: false
        }
    );
}

#[test]
fn the_form_starts_from_the_contract_defaults() {
    let form = TestabilityEdit::from_target(TARGET).unwrap();
    assert_eq!(form, edit("/__test__/login", "GUARD_TEST_TOKEN", &[]));
    let set = format!("{TARGET}\n[testability]\nroles = [\"admin\"]\ntoken_env = \"APP_TOKEN\"\n");
    let form = TestabilityEdit::from_target(&set).unwrap();
    assert_eq!(form, edit("/__test__/login", "APP_TOKEN", &["admin"]));
    assert!(TestabilityEdit::from_target("not toml [").is_none());
}

#[test]
fn saving_roles_creates_the_table_and_keeps_the_rest() {
    let out = set_testability(
        TARGET,
        &edit(
            "/__test__/login",
            "GUARD_TEST_TOKEN",
            &["admin", "customer"],
        ),
    )
    .unwrap();
    assert_eq!(
        out,
        format!("{TARGET}\n[testability]\nroles = [\"admin\", \"customer\"]\n")
    );
    let config = guard_core::config::parse_target(&out).unwrap();
    assert_eq!(config.testability.roles, roles(&["admin", "customer"]));
}

#[test]
fn saving_changes_only_the_keys_that_differ() {
    let text = format!(
        "{TARGET}\n[testability]\n# sessions\nroles = [\"admin\"]\nseed_path = \"/seed\"\n"
    );
    let out = set_testability(&text, &edit("/login", "APP_TOKEN", &[])).unwrap();
    assert_eq!(
        out,
        format!(
            "{TARGET}\n[testability]\n# sessions\nroles = []\nseed_path = \"/seed\"\nlogin_path = \"/login\"\ntoken_env = \"APP_TOKEN\"\n"
        )
    );
    // Nothing changed: the file is returned as it was.
    let same = edit("/__test__/login", "GUARD_TEST_TOKEN", &["admin"]);
    assert_eq!(set_testability(&text, &same).unwrap(), text);
}

#[test]
fn saving_rejects_values_outside_the_contract() {
    assert!(set_testability(TARGET, &edit("login", "GUARD_TEST_TOKEN", &[])).is_err());
    assert!(set_testability(TARGET, &edit("/login", "token", &[])).is_err());
    assert!(set_testability(TARGET, &edit("/login", "T", &["Admin"])).is_err());
    assert!(set_testability(TARGET, &edit("/login", "T", &["a", "a"])).is_err());
    assert!(set_testability("[testability.x]\ny = 1\n", &edit("/login", "T", &[])).is_err());
}
