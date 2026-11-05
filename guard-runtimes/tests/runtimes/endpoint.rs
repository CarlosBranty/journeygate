use crate::common::coder_task;
use guard_core::ports::RuntimeError;
use guard_runtimes::endpoint::{GatewayEndpoint, ensure_only_gateway};
use guard_runtimes::{Invocation, SandboxFile};

#[test]
fn pin_accepts_a_plain_gateway_root() {
    let ep = GatewayEndpoint::pin(&coder_task().gateway).unwrap();
    assert_eq!(ep.root(), "http://llm-gateway:4000");
    assert_eq!(ep.v1(), "http://llm-gateway:4000/v1");
    let mut g = coder_task().gateway;
    g.base_url = "https://gw.internal/".into();
    assert_eq!(
        GatewayEndpoint::pin(&g).unwrap().root(),
        "https://gw.internal"
    );
}

#[test]
fn pin_rejects_anything_but_a_plain_http_root() {
    for bad in [
        "ftp://llm-gateway:4000",
        "http://user:pw@llm-gateway:4000",
        "http://llm-gateway:4000/v1",
        "http://llm-gateway:4000?x=1",
        "http://llm-gateway:4000#f",
        "not a url",
    ] {
        let mut g = coder_task().gateway;
        g.base_url = bad.into();
        let err = GatewayEndpoint::pin(&g).unwrap_err();
        assert!(
            matches!(err, RuntimeError::InvalidTask(_)),
            "{bad}: {err:?}"
        );
    }
}

#[test]
fn pin_rejects_direct_mode() {
    let mut g = coder_task().gateway;
    g.via_gateway = false;
    assert!(matches!(
        GatewayEndpoint::pin(&g).unwrap_err(),
        RuntimeError::InvalidTask(_)
    ));
}

fn inv_with(env_value: &str, file: &str) -> Invocation {
    Invocation {
        argv: vec!["x".into()],
        env: [("A".to_string(), env_value.to_string())]
            .into_iter()
            .collect(),
        workdir: "/workspace".into(),
        stdin: None,
        files: vec![SandboxFile {
            path: "/task/f".into(),
            contents: file.into(),
        }],
    }
}

#[test]
fn foreign_urls_anywhere_in_the_invocation_are_rejected() {
    let ep = GatewayEndpoint::pin(&coder_task().gateway).unwrap();
    ensure_only_gateway(&inv_with("http://llm-gateway:4000/v1", "{}"), Some(&ep)).unwrap();
    let err = ensure_only_gateway(
        &inv_with("x", "base = 'https://api.openai.com/v1'"),
        Some(&ep),
    )
    .unwrap_err();
    assert!(
        matches!(err, RuntimeError::InvalidTask(ref m) if m.contains("api.openai.com")),
        "{err:?}"
    );
    // A prefix trick is still foreign.
    let err = ensure_only_gateway(&inv_with("http://llm-gateway:4000.evil.com", ""), Some(&ep));
    assert!(err.is_err());
    // Without a gateway (Copilot-hosted exception) no URL may appear at all.
    assert!(ensure_only_gateway(&inv_with("http://llm-gateway:4000", ""), None).is_err());
    ensure_only_gateway(&inv_with("plain", ""), None).unwrap();
}
