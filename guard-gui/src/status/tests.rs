use super::*;

const STATUS: &str = r#"{
  "base_url": "http://localhost:3000",
  "ready": false,
  "entries": [
    {"kind": "app", "name": "shop", "cwd": ".", "start": "docker compose up -d --wait",
     "ready_url": "http://localhost:3000/healthz", "pid": 4242, "alive": true, "ready": false,
     "log": ".guard/app.log"},
    {"kind": "service", "name": "backend", "cwd": "backend", "start": "bun run dev",
     "ready_url": null, "pid": null, "alive": null, "ready": null,
     "log": ".guard/services/backend.log"}
  ]
}"#;

const AFFECTED: &str = r#"{
  "base": "main",
  "base_source": "local_branch",
  "merge_base": "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
  "branch": "SHOP-123-fix-cart",
  "ticket": "SHOP-123",
  "changed_files": ["web/src/checkout/cart.ts"],
  "tags": ["@journey:checkout.customer-card", "@smoke", "@ticket:SHOP-123"],
  "journeys": [
    {"id": "checkout.customer-card", "tag": "@journey:checkout.customer-card",
     "reasons": [{"kind": "path", "file": "web/src/checkout/cart.ts", "glob": "web/src/checkout/**"}, {"kind": "smoke"}]}
  ]
}"#;

#[test]
fn parses_guard_status_json() {
    let status = parse_status(STATUS).unwrap();
    assert_eq!(status.base_url, "http://localhost:3000");
    assert!(!status.ready);
    assert_eq!(status.entries.len(), 2);
    let app = &status.entries[0];
    assert_eq!(app.kind, "app");
    assert_eq!(app.pid, Some(4242));
    assert_eq!(app.alive, Some(true));
    assert_eq!(app.ready, Some(false));
    let svc = &status.entries[1];
    assert_eq!(svc.ready_url, None);
    assert_eq!(svc.pid, None);
}

#[test]
fn tri_state_flags_read_as_words() {
    assert_eq!(yes_no(Some(true)), "yes");
    assert_eq!(yes_no(Some(false)), "no");
    assert_eq!(yes_no(None), "-");
}

#[test]
fn parses_guard_affected_json() {
    let affected = parse_affected(AFFECTED).unwrap();
    assert_eq!(affected.base, "main");
    assert_eq!(affected.changed_files, 1);
    assert_eq!(affected.ids, vec!["checkout.customer-card"]);
    assert_eq!(affected.tags.len(), 3);
}

#[test]
fn bad_json_is_an_error() {
    assert!(parse_status("nope").is_err());
    assert!(parse_affected("{}").is_err());
}
