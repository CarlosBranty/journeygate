use super::*;
use serde_json::json;

fn args(value: Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        other => panic!("not an object: {other}"),
    }
}

fn err(name: &str, value: Value) -> String {
    parse_call(name, args(value)).unwrap_err().0
}

#[test]
fn affected_defaults_base_to_none() {
    assert_eq!(
        parse_call("guard_affected", None).unwrap(),
        ToolCall::Affected(AffectedRequest { base: None })
    );
    assert_eq!(
        parse_call("guard_affected", args(json!({"base": "develop"}))).unwrap(),
        ToolCall::Affected(AffectedRequest {
            base: Some("develop".into())
        })
    );
}

#[test]
fn unknown_properties_are_rejected() {
    let message = err("guard_affected", json!({"base": "main", "force": true}));
    assert!(message.contains("force"), "{message}");
    // There is no snapshot update, headed or ui mode on guard_test.
    for forbidden in ["update_snapshots", "headed", "ui", "debug", "skip"] {
        let message = err("guard_test", json!({forbidden: true}));
        assert!(message.contains(forbidden), "{message}");
    }
}

#[test]
fn wrong_types_are_rejected() {
    err("guard_affected", json!({"base": 3}));
    err("guard_test", json!({"docker": "yes"}));
    err("guard_test", json!({"selection": "everything"}));
    err("guard_catalog", json!({"include_quarantined": "no"}));
}

#[test]
fn test_defaults_to_affected_without_base() {
    assert_eq!(
        parse_call("guard_test", None).unwrap(),
        ToolCall::Test(TestRequest {
            selection: TestSelection::Affected,
            base: None,
            docker: false,
        })
    );
    assert_eq!(
        parse_call(
            "guard_test",
            args(json!({"selection": "all", "docker": true, "base": "dev"}))
        )
        .unwrap(),
        ToolCall::Test(TestRequest {
            selection: TestSelection::All,
            base: Some("dev".into()),
            docker: true
        })
    );
}

#[test]
fn test_journeys_selection_requires_journey_ids() {
    assert_eq!(
        parse_call(
            "guard_test",
            args(json!({"selection": "journeys", "journeys": ["auth.login"]}))
        )
        .unwrap(),
        ToolCall::Test(TestRequest {
            selection: TestSelection::Journeys(vec!["auth.login".into()]),
            base: None,
            docker: false,
        })
    );
    let message = err("guard_test", json!({"selection": "journeys"}));
    assert!(message.contains("journeys"), "{message}");
    err(
        "guard_test",
        json!({"selection": "journeys", "journeys": []}),
    );
    err(
        "guard_test",
        json!({"selection": "journeys", "journeys": [1]}),
    );
}

#[test]
fn failure_requires_test_id_and_a_positive_pr() {
    assert_eq!(
        parse_call(
            "guard_failure",
            args(json!({"test_id": "chromium::a.spec.ts::pays"}))
        )
        .unwrap(),
        ToolCall::Failure(FailureRequest {
            test_id: "chromium::a.spec.ts::pays".into(),
            pr: None
        })
    );
    assert_eq!(
        parse_call("guard_failure", args(json!({"test_id": "t", "pr": 7}))).unwrap(),
        ToolCall::Failure(FailureRequest {
            test_id: "t".into(),
            pr: Some(7)
        })
    );
    let message = err("guard_failure", json!({}));
    assert!(message.contains("test_id"), "{message}");
    err("guard_failure", json!({"test_id": "t", "pr": 0}));
    err("guard_failure", json!({"test_id": "t", "pr": -3}));
    err("guard_failure", json!({"test_id": "t", "pr": 1.5}));
}

#[test]
fn repro_requires_test_id_and_pr() {
    assert_eq!(
        parse_call("guard_repro", args(json!({"test_id": "t", "pr": 12}))).unwrap(),
        ToolCall::Repro(ReproRequest {
            test_id: "t".into(),
            pr: 12
        })
    );
    err("guard_repro", json!({"test_id": "t"}));
    err("guard_repro", json!({"pr": 12}));
    err("guard_repro", json!({"test_id": "t", "pr": 0}));
}

#[test]
fn catalog_filters_are_optional_and_include_quarantined_by_default() {
    assert_eq!(
        parse_call("guard_catalog", None).unwrap(),
        ToolCall::Catalog(CatalogQuery::default())
    );
    assert_eq!(
        parse_call(
            "guard_catalog",
            args(json!({"area": "auth", "tag": "@smoke", "text": "log", "include_quarantined": false}))
        )
        .unwrap(),
        ToolCall::Catalog(CatalogQuery {
            area: Some("auth".into()),
            tag: Some("@smoke".into()),
            text: Some("log".into()),
            include_quarantined: false,
        })
    );
}

#[test]
fn unknown_tools_are_rejected() {
    for name in [
        "guard_update_snapshots",
        "guard_skip",
        "guard_edit_journey",
        "",
    ] {
        let message = parse_call(name, None).unwrap_err().0;
        assert!(message.contains("unknown tool"), "{message}");
    }
}
