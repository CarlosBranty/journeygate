//! `guard explain` end to end (cli.md). Never calls the real Anthropic API:
//! the only network path points at a local stub via GUARD_EXPLAIN_BASE_URL.

use assert_cmd::Command;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use tempfile::TempDir;

/// A credentials file that never exists, so the developer's own never leaks in.
const NO_CREDENTIALS: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/no-credentials.toml");

fn guard(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("guard").unwrap();
    cmd.current_dir(dir)
        .env("NO_COLOR", "1")
        .env("GUARD_CREDENTIALS_FILE", NO_CREDENTIALS);
    for var in [
        "GUARD_EXPLAIN_API_KEY",
        "ANTHROPIC_API_KEY",
        "GUARD_EXPLAIN_MODEL",
        "GUARD_EXPLAIN_BASE_URL",
        "GUARD_EXPLAIN_LANG",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n")
}

fn write_report(root: &Path, status: &str, pw_status: &str, error: &str) {
    let report = serde_json::json!({
        "suites": [{
            "title": "checkout/split-payment.spec.ts",
            "specs": [{
                "title": "Gift card and credit card",
                "file": "tests/journeys/checkout/split-payment.spec.ts",
                "line": 42,
                "tags": ["@journey:checkout.split-payment"],
                "tests": [{
                    "projectName": "chromium",
                    "status": pw_status,
                    "results": [{
                        "duration": 14730.0,
                        "retry": 0,
                        "status": status,
                        "error": {"message": error}
                    }]
                }]
            }]
        }],
        "errors": [],
        "stats": {"duration": 14730.0}
    });
    let path = root.join("e2e-playwright/results/report.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
}

/// A local HTTP stub that answers `POST /v1/messages` with a canned Messages
/// API body; returns its base URL.
fn stub_messages_api(text: &'static str) -> String {
    stub_recording(text).0
}

/// Like [`stub_messages_api`], and also hands back each raw request (headers
/// and body) so a test can check what guard sent.
fn stub_recording(text: &'static str) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = tx.send(read_request(&mut stream));
            let body = serde_json::json!({
                "content": [{"type": "text", "text": text}],
                "model": "claude-haiku-5-5",
                "stop_reason": "end_turn"
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

/// The blank line between HTTP headers and body.
const HEADER_END: &str = "\r\n\r\n";

/// Reads one request: the headers, then `Content-Length` bytes of body.
fn read_request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let Ok(n) = stream.read(&mut buf) else { break };
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
        let text = String::from_utf8_lossy(&bytes);
        if let Some(end) = text.find(HEADER_END) {
            let length = text[..end]
                .lines()
                .find_map(|l| {
                    let (name, value) = l.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The `messages[0].content` (the user prompt) of a recorded request.
fn sent_prompt(request: &str) -> String {
    let body = request.split_once(HEADER_END).map(|(_, b)| b).unwrap_or("");
    let value: serde_json::Value = serde_json::from_str(body).unwrap();
    value["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn explain_without_a_report_is_an_environment_error() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path()).arg("explain").output().unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("report.json"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn explain_with_a_missing_key_names_the_variable_and_makes_no_call() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    // No ANTHROPIC_API_KEY, and a base URL that would refuse any connection:
    // the friendly error must come before any request.
    let output = guard(dir.path())
        .arg("explain")
        .env("GUARD_EXPLAIN_BASE_URL", "http://127.0.0.1:9")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("GUARD_EXPLAIN_API_KEY is not set"), "{err}");
    // The hint names the fallback too.
    assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
}

#[test]
fn explain_reads_guard_explain_api_key_without_anthropic_api_key() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    let (base, requests) = stub_recording("Causa probable: el total.");
    let output = guard(dir.path())
        .arg("explain")
        .env("GUARD_EXPLAIN_API_KEY", "guard-test-key")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let request = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: guard-test-key"),
        "the guard key was not sent"
    );
    // The key is never echoed back.
    assert!(!stdout(&output).contains("guard-test-key"));
    assert!(!stderr(&output).contains("guard-test-key"));
}

#[test]
fn explain_asks_for_spanish_by_default_and_english_with_lang_en() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    let (base, requests) = stub_recording("ok");
    let spanish = guard(dir.path())
        .arg("explain")
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(spanish.status.code(), Some(0), "{}", stderr(&spanish));
    let prompt = sent_prompt(&requests.recv_timeout(Duration::from_secs(10)).unwrap());
    assert!(prompt.contains("in Spanish"), "{prompt}");
    assert!(prompt.contains("boom at order-total"), "{prompt}");

    let english = guard(dir.path())
        .args(["explain", "--lang", "en"])
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_LANG", "es")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(english.status.code(), Some(0), "{}", stderr(&english));
    let prompt = sent_prompt(&requests.recv_timeout(Duration::from_secs(10)).unwrap());
    assert!(prompt.contains("in English"), "{prompt}");
    assert!(!prompt.contains("Spanish"), "{prompt}");
}

#[test]
fn explain_lang_env_sets_the_language() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    let (base, requests) = stub_recording("ok");
    let output = guard(dir.path())
        .arg("explain")
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_LANG", "en")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let prompt = sent_prompt(&requests.recv_timeout(Duration::from_secs(10)).unwrap());
    assert!(prompt.contains("in English"), "{prompt}");
}

#[test]
fn explain_help_documents_lang_and_the_key_variables() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path())
        .args(["explain", "--help"])
        .output()
        .unwrap();
    let help = stdout(&output);
    assert!(help.contains("--lang <CODE>"), "{help}");
    assert!(help.contains("GUARD_EXPLAIN_API_KEY"), "{help}");
    assert!(help.contains("GUARD_EXPLAIN_LANG"), "{help}");
}

#[test]
fn explain_with_no_failures_is_green_and_needs_no_key() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "passed", "expected", "");
    let output = guard(dir.path()).arg("explain").output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).contains("All green"), "{}", stdout(&output));
}

#[test]
fn explain_prints_the_model_summary_from_the_stub() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    let base = stub_messages_api("Likely cause: the order total is computed wrong.");
    let output = guard(dir.path())
        .arg("explain")
        .env("ANTHROPIC_API_KEY", "test-key")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Likely cause: the order total"), "{text}");
    assert!(text.contains("1 failing/flaky journey"), "{text}");
}

#[test]
fn explain_json_carries_model_summary_and_journeys() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    let base = stub_messages_api("Root cause paragraph.");
    let output = guard(dir.path())
        .args(["explain", "--json"])
        .env("ANTHROPIC_API_KEY", "test-key")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(value["summary"], "Root cause paragraph.");
    assert_eq!(value["journeys"][0]["journey"], "checkout.split-payment");
    assert_eq!(value["journeys"][0]["status"], "failed");
    assert!(value["model"].is_string());
}

/// A local HTTP stub answering each request by the first route whose
/// `path?query` prefix matches (404 otherwise); hands back each raw request.
fn stub_routes(routes: Vec<(&'static str, u16, String)>) -> (String, Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let request = read_request(&mut stream);
            let target = request
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("")
                .to_string();
            let (status, body) = routes
                .iter()
                .find(|(prefix, _, _)| target.starts_with(prefix))
                .map(|(_, s, b)| (*s, b.clone()))
                .unwrap_or((404, r#"{"error":{"message":"not found"}}"#.to_string()));
            let _ = tx.send(request);
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://127.0.0.1:{port}"), rx)
}

#[test]
fn list_models_pages_through_the_anthropic_list_without_a_report() {
    let dir = TempDir::new().unwrap();
    let (base, requests) = stub_routes(vec![
        (
            "/v1/models?limit=1000&after_id=claude-sonnet-5-5",
            200,
            r#"{"data":[{"id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5"}],"has_more":false,"last_id":"claude-haiku-5-5"}"#.into(),
        ),
        (
            "/v1/models",
            200,
            r#"{"data":[{"id":"claude-opus-5-5","display_name":"Claude Opus 5.5"},{"id":"claude-sonnet-5-5"}],"has_more":true,"last_id":"claude-sonnet-5-5"}"#.into(),
        ),
    ]);
    let output = guard(dir.path())
        .args(["explain", "--list-models"])
        .env("GUARD_EXPLAIN_API_KEY", "list-test-key")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    let ids: Vec<&str> = text
        .lines()
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
        "{text}"
    );
    assert!(text.contains("Claude Opus 5.5"), "{text}");
    let first = requests.recv_timeout(Duration::from_secs(10)).unwrap();
    let lower = first.to_ascii_lowercase();
    assert!(lower.starts_with("get /v1/models?limit=1000 "), "{first}");
    assert!(lower.contains("x-api-key: list-test-key"), "{first}");
    assert!(lower.contains("anthropic-version: 2023-06-01"), "{first}");
    assert!(!text.contains("list-test-key"));
    assert!(!stderr(&output).contains("list-test-key"));
}

#[test]
fn list_models_json_is_an_array_of_ids_and_names() {
    let dir = TempDir::new().unwrap();
    let (base, _requests) = stub_routes(vec![(
        "/v1/models",
        200,
        r#"{"data":[{"id":"m1","display_name":"Model One"},{"id":"m2"}],"has_more":false}"#.into(),
    )]);
    let output = guard(dir.path())
        .args(["explain", "--list-models", "--json"])
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(
        value,
        serde_json::json!([{"id": "m1", "name": "Model One"}, {"id": "m2"}])
    );
}

#[test]
fn list_models_falls_back_to_the_openai_then_ollama_formats() {
    let dir = TempDir::new().unwrap();
    // /v1/models and /models answer 404: the Ollama tags endpoint is last.
    let (base, requests) = stub_routes(vec![(
        "/api/tags",
        200,
        r#"{"models":[{"name":"qwen3:8b"},{"name":"llama3.2:latest"}]}"#.into(),
    )]);
    let output = guard(dir.path())
        .args(["explain", "--list-models"])
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert_eq!(stdout(&output), "qwen3:8b\nllama3.2:latest\n");
    let tried: Vec<String> = requests
        .try_iter()
        .map(|r| r.lines().next().unwrap_or("").to_string())
        .collect();
    assert_eq!(tried.len(), 3, "{tried:?}");
    assert!(tried[1].starts_with("GET /models "), "{tried:?}");
}

#[test]
fn list_models_without_a_key_names_the_variable_and_makes_no_call() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path())
        .args(["explain", "--list-models"])
        .env("GUARD_EXPLAIN_BASE_URL", "http://127.0.0.1:9")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("GUARD_EXPLAIN_API_KEY is not set"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn list_models_redacts_the_key_from_an_echoed_error_body() {
    let dir = TempDir::new().unwrap();
    let (base, requests) = stub_routes(vec![(
        "/v1/models",
        401,
        r#"{"error":{"message":"invalid x-api-key sk-echoed-key"}}"#.into(),
    )]);
    let output = guard(dir.path())
        .args(["explain", "--list-models"])
        .env("GUARD_EXPLAIN_API_KEY", "sk-echoed-key")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let err = stderr(&output);
    assert!(err.contains("HTTP 401"), "{err}");
    assert!(err.contains("[redacted]"), "{err}");
    assert!(!err.contains("sk-echoed-key"), "{err}");
    assert!(!stdout(&output).contains("sk-echoed-key"));
    // A rejected key is the answer: no fallback endpoints are tried.
    assert_eq!(requests.try_iter().count(), 1);
}

#[test]
fn explain_help_documents_list_models() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path())
        .args(["explain", "--help"])
        .output()
        .unwrap();
    assert!(
        stdout(&output).contains("--list-models"),
        "{}",
        stdout(&output)
    );
}

/// A structured model reply (the JSON shape the system prompt asks for),
/// fenced the way models often wrap it.
const STRUCTURED: &str = "```json\n{\"rows\":[{\"tests\":\"Checkout: gift card and credit card (1 test)\",\"journeys\":[\"checkout.split-payment\"],\"what_happened\":\"The order total assertion failed: boom at order-total, expected 10 but received 12 after applying the gift card.\",\"probable_cause\":\"Bug in the order total. The total endpoint adds the gift card amount twice when a second payment method is present.\",\"kind\":\"app_bug\"}],\"verdict\":\"Row 1 points to a real application bug in the order total.\"}\n```";

fn explain_structured(dir: &Path, args: &[&str]) -> std::process::Output {
    write_report(dir, "failed", "unexpected", "boom at order-total");
    let base = stub_messages_api(STRUCTURED);
    guard(dir)
        .arg("explain")
        .args(args)
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .env("COLUMNS", "100")
        .output()
        .unwrap()
}

#[test]
fn explain_renders_a_structured_reply_as_a_box_table() {
    let dir = TempDir::new().unwrap();
    let output = explain_structured(dir.path(), &[]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    println!("{text}");
    for needle in [
        "┌",
        "┬",
        "├",
        "┼",
        "└",
        "┴",
        "│",
        "#",
        "Test",
        "Qué pasó",
        "Causa probable",
    ] {
        assert!(text.contains(needle), "missing {needle}: {text}");
    }
    assert!(text.contains("Checkout: gift card"), "{text}");
    assert!(text.contains("Bug in the order total."), "{text}");
    assert!(text.contains("real application bug"), "{text}");
    // Wrapped to COLUMNS: no line is wider than 100 characters.
    assert!(text.lines().all(|l| l.chars().count() <= 100), "{text}");
    // The fence and raw JSON never reach the user.
    assert!(!text.contains("```"), "{text}");
    assert!(!text.contains("\"rows\""), "{text}");
}

#[test]
fn explain_table_headers_follow_lang_en() {
    let dir = TempDir::new().unwrap();
    let output = explain_structured(dir.path(), &["--lang", "en"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("What happened"), "{text}");
    assert!(text.contains("Probable cause"), "{text}");
    assert!(!text.contains("Qué pasó"), "{text}");
}

#[test]
fn explain_free_text_reply_falls_back_to_the_markdown_summary() {
    let dir = TempDir::new().unwrap();
    write_report(dir.path(), "failed", "unexpected", "boom at order-total");
    let base = stub_messages_api("## checkout\nLikely cause: the total.");
    let output = guard(dir.path())
        .args(["explain", "--format", "table"])
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("Likely cause: the total."), "{text}");
    assert!(text.contains("1 failing/flaky journey"), "{text}");
    assert!(!text.contains("┌"), "{text}");

    let json = guard(dir.path())
        .args(["explain", "--format", "json"])
        .env("GUARD_EXPLAIN_API_KEY", "k")
        .env("GUARD_EXPLAIN_BASE_URL", &base)
        .output()
        .unwrap();
    assert_eq!(json.status.code(), Some(0), "{}", stderr(&json));
    let value: serde_json::Value = serde_json::from_str(stdout(&json).trim()).unwrap();
    assert_eq!(value["summary"], "## checkout\nLikely cause: the total.");
    assert_eq!(value["rows"], serde_json::json!([]));
    assert!(value["verdict"].is_null());
}

#[test]
fn explain_json_of_a_structured_reply_keeps_the_back_compat_keys() {
    let dir = TempDir::new().unwrap();
    let output = explain_structured(dir.path(), &["--format", "json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let value: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert!(value["model"].is_string());
    assert_eq!(value["rows"][0]["kind"], "app_bug");
    assert_eq!(value["rows"][0]["journeys"][0], "checkout.split-payment");
    assert!(
        value["verdict"]
            .as_str()
            .unwrap()
            .contains("real application bug")
    );
    assert!(value["summary"].as_str().unwrap().contains("| # |"));
    assert_eq!(value["journeys"][0]["journey"], "checkout.split-payment");
}

#[test]
fn explain_md_out_writes_the_file_and_says_so_on_stderr() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("explain.md");
    let output = explain_structured(
        dir.path(),
        &["--format", "md", "--out", out.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    assert!(stderr(&output).contains("wrote "), "{}", stderr(&output));
    let md = fs::read_to_string(&out).unwrap();
    assert!(md.starts_with("# guard explain — "), "{md}");
    assert!(
        md.contains("| # | Test | Qué pasó | Causa probable | Tipo |"),
        "{md}"
    );
    assert!(md.contains("Bug de la app"), "{md}");
    assert!(md.contains("**Veredicto:**"), "{md}");
    assert!(md.contains("checkout.split-payment"), "{md}");
}

#[test]
fn explain_xlsx_out_writes_a_workbook() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("explain.xlsx");
    let output = explain_structured(
        dir.path(),
        &["--format", "xlsx", "--out", out.to_str().unwrap()],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let bytes = fs::read(&out).unwrap();
    assert_eq!(&bytes[..2], b"PK");
}

#[test]
fn explain_xlsx_without_out_is_a_usage_error() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path())
        .args(["explain", "--format", "xlsx"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("--out"), "{}", stderr(&output));
}

#[test]
fn explain_json_conflicts_with_format() {
    let dir = TempDir::new().unwrap();
    let output = guard(dir.path())
        .args(["explain", "--json", "--format", "md"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
}
