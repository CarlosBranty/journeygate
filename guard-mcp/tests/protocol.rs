//! Real JSON-RPC 2.0 round trips (newline-delimited) against the server, with a
//! fake [`GuardActions`] standing in for guard-cli.

use async_trait::async_trait;
use guard_core::catalog::{Catalog, Journey};
use guard_mcp::types::*;
use guard_mcp::{GuardActions, GuardMcpServer, ServerOptions, ToolError, serve, serve_handler};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

// ------------------------------------------------------------------- fake

#[derive(Debug, Clone, PartialEq)]
enum Called {
    Affected(AffectedRequest),
    Test(TestRequest),
    Failure(FailureRequest),
    Repro(ReproRequest),
    Catalog,
}

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Called>>,
    test_result: Mutex<Option<Result<TestOutput, ToolError>>>,
    failure_result: Mutex<Option<Result<FailureOutput, ToolError>>>,
    test_delay: Option<Duration>,
}

impl Fake {
    fn record(&self, call: Called) {
        self.calls.lock().unwrap().push(call);
    }

    fn calls(&self) -> Vec<Called> {
        self.calls.lock().unwrap().clone()
    }
}

fn affected_output(base: &str) -> AffectedOutput {
    AffectedOutput {
        base: base.to_string(),
        base_source: BaseSource::RemoteHead,
        merge_base: "abc123".into(),
        branch: Some("feat/PAY-12-card".into()),
        ticket: Some("PAY-12".into()),
        changed_files: vec!["src/checkout/card.ts".into()],
        tags: vec!["@journey:checkout.customer-card".into(), "@smoke".into()],
        journeys: vec![AffectedJourney {
            id: "checkout.customer-card".into(),
            tag: "@journey:checkout.customer-card".into(),
            reasons: vec![
                SelectionReason {
                    kind: ReasonKind::Path,
                    file: Some("src/checkout/card.ts".into()),
                    glob: Some("src/checkout/**".into()),
                    probability: None,
                },
                SelectionReason {
                    kind: ReasonKind::Impact,
                    file: Some("src/checkout/card.ts".into()),
                    glob: None,
                    probability: Some(0.82),
                },
            ],
        }],
        impact: ImpactInfo {
            source: ImpactSource::Server,
            status: ImpactStatus::Ok,
            threshold: Some(0.5),
        },
    }
}

fn failing_test_output() -> TestOutput {
    TestOutput {
        exit_code: 1,
        counts: TestCounts {
            passed: 3,
            flaky: 0,
            failed: 1,
            skipped: 0,
        },
        duration_ms: 41_200,
        tags: vec!["@journey:checkout.customer-card".into()],
        failures: vec![FailureSummary {
            test_id: "chromium::journeys/checkout.spec.ts::pays by card".into(),
            title: "pays by card".into(),
            file: "journeys/checkout.spec.ts".into(),
            line: 14,
            journey: Some("checkout.customer-card".into()),
            status: FailureStatus::Failed,
            error: "expect(locator).toBeVisible() failed".into(),
            location: Some("journeys/checkout.spec.ts:22".into()),
            screenshot: Some("e2e-playwright/test-results/pays/test-failed-1.png".into()),
        }],
    }
}

fn failure_output(test_id: &str) -> FailureOutput {
    FailureOutput {
        test_id: test_id.to_string(),
        source: FailureSource::Local,
        title: "pays by card".into(),
        file: "journeys/checkout.spec.ts".into(),
        line: 14,
        journey: Some("checkout.customer-card".into()),
        location: Some("journeys/checkout.spec.ts:22".into()),
        error: "expect(locator).toBeVisible() failed".into(),
        steps: vec!["open cart".into(), "pay".into()],
        attempts: vec![Attempt {
            retry: 0,
            status: "failed".into(),
            duration_ms: 9000,
        }],
        baseline: None,
        fingerprint: Some("f00d".into()),
        screenshot: None,
        trace_url: Some("e2e-playwright/test-results/pays/trace.zip".into()),
        jira: None,
    }
}

#[async_trait]
impl GuardActions for Fake {
    async fn affected(&self, request: AffectedRequest) -> Result<AffectedOutput, ToolError> {
        let base = request.base.clone().unwrap_or_else(|| "origin/main".into());
        self.record(Called::Affected(request));
        Ok(affected_output(&base))
    }

    async fn test(&self, request: TestRequest) -> Result<TestOutput, ToolError> {
        self.record(Called::Test(request));
        if let Some(delay) = self.test_delay {
            tokio::time::sleep(delay).await;
        }
        self.test_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(failing_test_output()))
    }

    async fn failure(&self, request: FailureRequest) -> Result<FailureOutput, ToolError> {
        let id = request.test_id.clone();
        self.record(Called::Failure(request));
        self.failure_result
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Ok(failure_output(&id)))
    }

    async fn repro(&self, request: ReproRequest) -> Result<ReproOutput, ToolError> {
        self.record(Called::Repro(request));
        Ok(ReproOutput {
            reproduced: true,
            exit_code: 0,
            evidence_dir: ".guard/repro/f00d/".into(),
            remote: FailureRef {
                status: RunStatus::Failed,
                fingerprint: Some("f00d".into()),
                error: Some("boom".into()),
            },
            local: None,
        })
    }

    async fn catalog(&self) -> Result<Catalog, ToolError> {
        self.record(Called::Catalog);
        let journey = |id: &str, area: &str, smoke: bool, quarantined: bool| Journey {
            id: id.into(),
            area: area.into(),
            title: format!("Title of {id}"),
            impact_question: format!("Does {id} work?"),
            paths: vec![format!("src/{area}/**")],
            smoke,
            quarantined,
        };
        Ok(Catalog {
            schema: None,
            version: 1,
            journeys: vec![
                journey("checkout.customer-card", "checkout", true, false),
                journey("auth.login", "auth", false, true),
            ],
        })
    }
}

// ----------------------------------------------------------------- client

struct Client {
    writer: DuplexStream,
    lines: Lines<BufReader<DuplexStream>>,
    next_id: u64,
    _server: tokio::task::JoinHandle<()>,
}

impl Client {
    fn start(fake: Arc<Fake>, options: ServerOptions) -> Self {
        Self::start_server(GuardMcpServer::new(fake, options))
    }

    fn start_server(handler: GuardMcpServer) -> Self {
        let (client_out, server_in) = tokio::io::duplex(1 << 20);
        let (server_out, client_in) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            serve_handler(handler, server_in, server_out).await.unwrap();
        });
        Self {
            writer: client_out,
            lines: BufReader::new(client_in).lines(),
            next_id: 1,
            _server: server,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut line = serde_json::to_string(&message).unwrap();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    async fn read(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("server answered in time")
            .unwrap()
            .expect("server closed stdout");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout is not JSON-RPC: {e}: {line}"))
    }

    /// Sends a request and returns the whole response envelope.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let response = self.read().await;
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], json!(id), "{response}");
        response
    }

    async fn initialize_with(&mut self, version: &str) -> Value {
        let response = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": version,
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "1.0.0"}
                }),
            )
            .await;
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        response
    }

    async fn ready(fake: Arc<Fake>) -> Self {
        Self::ready_with(fake, ServerOptions::default()).await
    }

    async fn ready_with(fake: Arc<Fake>, options: ServerOptions) -> Self {
        let mut client = Self::start(fake, options);
        client.initialize_with("2025-06-18").await;
        client
    }

    async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": arguments}))
            .await
    }
}

fn output_schema(tool: &str) -> Value {
    guard_mcp::tools::spec(tool).unwrap().output_schema
}

/// The successful-result contract: text = pretty JSON of structuredContent,
/// which conforms to the tool's outputSchema.
fn assert_ok_result(tool: &str, response: &Value) -> Value {
    let result = &response["result"];
    assert_eq!(result["isError"], json!(false), "{response}");
    let structured = result["structuredContent"].clone();
    assert_eq!(result["content"].as_array().unwrap().len(), 1);
    assert_eq!(result["content"][0]["type"], "text");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, serde_json::to_string_pretty(&structured).unwrap());
    let validator = jsonschema::draft202012::new(&output_schema(tool)).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&structured)
        .map(|e| e.to_string())
        .collect();
    assert!(
        errors.is_empty(),
        "{tool} output violates its schema: {errors:?}"
    );
    assert!(result.get("resultType").is_none(), "{response}");
    structured
}

fn assert_tool_error(response: &Value, code: &str, exit_code: i64) -> Value {
    let result = &response["result"];
    assert_eq!(result["isError"], json!(true), "{response}");
    let structured = result["structuredContent"].clone();
    assert_eq!(structured["error"], code, "{response}");
    assert_eq!(structured["exit_code"], exit_code, "{response}");
    assert!(structured["detail"].as_str().is_some_and(|d| !d.is_empty()));
    let text = result["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, serde_json::to_string_pretty(&structured).unwrap());
    structured
}

fn assert_rpc_error(response: &Value, code: i64) {
    assert!(response.get("result").is_none(), "{response}");
    assert_eq!(response["error"]["code"], code, "{response}");
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn initialize_answers_the_contract_result() {
    let mut client = Client::start(Arc::new(Fake::default()), ServerOptions::default());
    let response = client.initialize_with("2025-06-18").await;
    assert_eq!(
        response["result"],
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "guard", "version": env!("CARGO_PKG_VERSION")}
        })
    );
}

#[tokio::test]
async fn any_other_requested_version_gets_2025_06_18() {
    for version in ["2024-11-05", "2025-03-26", "2025-11-25", "2099-01-01"] {
        let mut client = Client::start(Arc::new(Fake::default()), ServerOptions::default());
        let response = client.initialize_with(version).await;
        assert_eq!(
            response["result"]["protocolVersion"], "2025-06-18",
            "client asked {version}: {response}"
        );
    }
}

#[tokio::test]
async fn initialized_notification_gets_no_response_and_ping_answers_empty() {
    let mut client = Client::ready(Arc::new(Fake::default())).await;
    // The notification sent by `ready` produced nothing: the next line is the ping answer.
    let response = client.request("ping", json!({})).await;
    assert_eq!(response["result"], json!({}));
}

#[tokio::test]
async fn unknown_methods_are_method_not_found() {
    // `server/discover` (protocol 2026-07-28) is left out on purpose: rmcp
    // validates its `_meta` before dispatch, so a bare call is -32602. With
    // `_meta` it reaches the handler, which answers -32601.
    let mut client = Client::ready(Arc::new(Fake::default())).await;
    for method in [
        "resources/list",
        "resources/templates/list",
        "resources/read",
        "prompts/list",
        "prompts/get",
        "completion/complete",
        "logging/setLevel",
        "guard/update_snapshots",
        "foo",
    ] {
        let response = client.request(method, json!({})).await;
        assert_rpc_error(&response, -32601);
    }
}

#[tokio::test]
async fn tools_list_is_exactly_the_contract_tools() {
    let mut client = Client::ready(Arc::new(Fake::default())).await;
    let response = client.request("tools/list", json!({})).await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "guard_affected",
            "guard_test",
            "guard_failure",
            "guard_repro",
            "guard_catalog",
            "guard_history",
            "guard_map"
        ]
    );
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        let spec = guard_mcp::tools::spec(name).unwrap();
        assert_eq!(tool["inputSchema"], spec.input_schema, "{name}");
        assert_eq!(tool["outputSchema"], spec.output_schema, "{name}");
        let runs = name == "guard_test" || name == "guard_repro";
        assert_eq!(
            tool["annotations"],
            json!({
                "readOnlyHint": !runs,
                "destructiveHint": false,
                "openWorldHint": name == "guard_repro"
            }),
            "{name}"
        );
    }
}

#[tokio::test]
async fn guard_affected_forwards_the_base_and_returns_the_affected_object() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    let response = client.call("guard_affected", json!({})).await;
    let structured = assert_ok_result("guard_affected", &response);
    assert_eq!(
        structured,
        serde_json::to_value(affected_output("origin/main")).unwrap()
    );
    assert_eq!(structured["base_source"], "remote_head");
    client
        .call("guard_affected", json!({"base": "develop"}))
        .await;
    assert_eq!(
        fake.calls(),
        vec![
            Called::Affected(AffectedRequest { base: None }),
            Called::Affected(AffectedRequest {
                base: Some("develop".into())
            }),
        ]
    );
}

#[tokio::test]
async fn guard_test_failed_tests_are_a_normal_result() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    let response = client
        .call(
            "guard_test",
            json!({"selection": "journeys", "journeys": ["checkout.customer-card"], "docker": true}),
        )
        .await;
    let structured = assert_ok_result("guard_test", &response);
    assert_eq!(structured["exit_code"], 1);
    assert_eq!(structured["failures"][0]["line"], 14);
    assert_eq!(
        structured["failures"][0]["screenshot"],
        "e2e-playwright/test-results/pays/test-failed-1.png"
    );
    assert_eq!(
        fake.calls(),
        vec![Called::Test(TestRequest {
            selection: TestSelection::Journeys(vec!["checkout.customer-card".into()]),
            base: None,
            docker: true,
        })]
    );
}

#[tokio::test]
async fn guard_test_config_and_environment_failures_are_is_error() {
    let fake = Arc::new(Fake::default());
    *fake.test_result.lock().unwrap() = Some(Err(ToolError::config("target.toml: [app] missing")));
    let mut client = Client::ready(fake.clone()).await;
    let response = client.call("guard_test", json!({})).await;
    let structured = assert_tool_error(&response, "config_error", 2);
    assert_eq!(structured["detail"], "target.toml: [app] missing");

    *fake.test_result.lock().unwrap() = Some(Err(ToolError::environment("app not ready")));
    let response = client.call("guard_test", json!({})).await;
    assert_tool_error(&response, "environment_error", 3);
}

#[tokio::test]
async fn an_ok_run_with_exit_2_or_3_is_still_reported_as_is_error() {
    let fake = Arc::new(Fake::default());
    let mut crashed = failing_test_output();
    crashed.exit_code = 3;
    *fake.test_result.lock().unwrap() = Some(Ok(crashed));
    let mut client = Client::ready(fake.clone()).await;
    assert_tool_error(
        &client.call("guard_test", json!({})).await,
        "environment_error",
        3,
    );

    let mut bad_config = failing_test_output();
    bad_config.exit_code = 2;
    *fake.test_result.lock().unwrap() = Some(Ok(bad_config));
    assert_tool_error(
        &client.call("guard_test", json!({})).await,
        "config_error",
        2,
    );
}

#[tokio::test]
async fn long_runs_time_out_with_is_error_timeout() {
    let fake = Arc::new(Fake {
        test_delay: Some(Duration::from_secs(5)),
        ..Fake::default()
    });
    let options = ServerOptions {
        long_run_timeout: Duration::from_millis(50),
    };
    let mut client = Client::ready_with(fake, options).await;
    let response = client.call("guard_test", json!({})).await;
    assert_tool_error(&response, "timeout", 3);
    // The server keeps serving after a timeout.
    let response = client.request("ping", json!({})).await;
    assert_eq!(response["result"], json!({}));
}

#[test]
fn the_default_long_run_timeout_is_thirty_minutes() {
    assert_eq!(
        ServerOptions::default().long_run_timeout,
        Duration::from_secs(30 * 60)
    );
}

#[tokio::test]
async fn guard_failure_returns_detail_and_passes_tool_errors_through() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    let response = client
        .call(
            "guard_failure",
            json!({"test_id": "chromium::a.spec.ts::pays"}),
        )
        .await;
    let structured = assert_ok_result("guard_failure", &response);
    assert_eq!(structured["test_id"], "chromium::a.spec.ts::pays");
    assert_eq!(structured["baseline"], Value::Null);

    *fake.failure_result.lock().unwrap() = Some(Err(ToolError::not_found("no failure x")));
    assert_tool_error(
        &client.call("guard_failure", json!({"test_id": "x"})).await,
        "not_found",
        2,
    );
    *fake.failure_result.lock().unwrap() = Some(Err(ToolError::server_not_configured(
        "no [server] in target.toml",
    )));
    assert_tool_error(
        &client
            .call("guard_failure", json!({"test_id": "x", "pr": 4}))
            .await,
        "server_not_configured",
        2,
    );
    assert_eq!(
        fake.calls().last(),
        Some(&Called::Failure(FailureRequest {
            test_id: "x".into(),
            pr: Some(4)
        }))
    );
}

#[tokio::test]
async fn guard_repro_forwards_test_id_and_pr() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    let response = client
        .call("guard_repro", json!({"test_id": "t", "pr": 9}))
        .await;
    let structured = assert_ok_result("guard_repro", &response);
    assert_eq!(structured["reproduced"], true);
    assert_eq!(structured["local"], Value::Null);
    assert_eq!(
        fake.calls(),
        vec![Called::Repro(ReproRequest {
            test_id: "t".into(),
            pr: 9
        })]
    );
}

#[tokio::test]
async fn guard_catalog_filters_the_catalog_from_the_actions() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    let all = assert_ok_result(
        "guard_catalog",
        &client.call("guard_catalog", json!({})).await,
    );
    let ids: Vec<&str> = all["journeys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["auth.login", "checkout.customer-card"]);
    let active = assert_ok_result(
        "guard_catalog",
        &client
            .call(
                "guard_catalog",
                json!({"include_quarantined": false, "tag": "@smoke"}),
            )
            .await,
    );
    assert_eq!(active["journeys"].as_array().unwrap().len(), 1);
    assert_eq!(
        active["journeys"][0]["tags"],
        json!([
            "@journey:checkout.customer-card",
            "@area:checkout",
            "@smoke"
        ])
    );
}

#[tokio::test]
async fn invalid_arguments_are_invalid_params_and_never_reach_the_actions() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    let bad = [
        ("guard_test", json!({"update_snapshots": true})),
        ("guard_test", json!({"headed": true})),
        ("guard_test", json!({"selection": "journeys"})),
        ("guard_failure", json!({})),
        ("guard_failure", json!({"test_id": "t", "pr": 0})),
        ("guard_repro", json!({"test_id": "t"})),
        ("guard_affected", json!({"base": 1})),
        ("guard_catalog", json!({"journey_id": "x"})),
    ];
    for (tool, arguments) in bad {
        let response = client.call(tool, arguments.clone()).await;
        assert_rpc_error(&response, -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|m| !m.is_empty()),
            "{tool} {arguments}: {response}"
        );
    }
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
}

#[tokio::test]
async fn tools_that_mutate_the_guard_do_not_exist() {
    let fake = Arc::new(Fake::default());
    let mut client = Client::ready(fake.clone()).await;
    for tool in [
        "guard_update_snapshots",
        "guard_edit_journey",
        "guard_delete_journey",
        "guard_skip",
        "guard_quarantine",
        "guard_override_verdict",
    ] {
        let response = client.call(tool, json!({})).await;
        assert_rpc_error(&response, -32602);
    }
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn closing_stdin_ends_the_server_cleanly() {
    let (client_out, server_in) = tokio::io::duplex(1024);
    let (server_out, _client_in) = tokio::io::duplex(1024);
    let server = tokio::spawn(serve(
        Arc::new(Fake::default()),
        ServerOptions::default(),
        server_in,
        server_out,
    ));
    drop(client_out);
    let result = tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server stops when stdin closes")
        .unwrap();
    // Closing before `initialize` is a normal end, not a crash.
    assert!(result.is_ok(), "{result:?}");
}

// ------------------------------------------------------------ Phase 4 tools

async fn phase_four_client() -> (Client, Arc<guard_core::ports::fakes::InMemoryMemoryFacts>) {
    use guard_core::ports::{FactScope, FactSource, MemoryFactsWriter, NewFact};
    let facts = Arc::new(guard_core::ports::fakes::InMemoryMemoryFacts::new());
    facts
        .record_fact(&NewFact {
            repo: "acme/shop".into(),
            scope: FactScope::Journey,
            scope_key: Some("checkout.customer-card".into()),
            topic_key: "flaky/checkout.customer-card".into(),
            body: "Flaky on retries of the card iframe.".into(),
            commit_sha: "abc1234abc1234abc1234abc1234abc1234abc12".into(),
            run_id: Some("r1".into()),
            evidence_uri: None,
            source: FactSource::FlakyHistory,
            valid_from: "2026-10-01T00:00:00Z".into(),
            review_after: None,
            referenced_paths: vec![],
        })
        .await
        .unwrap();
    let history = guard_mcp::tools::history::HistoryContext {
        reader: facts.reader(),
        repo: "acme/shop".into(),
        evidence: None,
    };
    let map: Arc<dyn guard_map::GraphSource> = Arc::new(
        guard_map::fake::FakeGraphSource::new("guard-impact", "abc123")
            .edge("src/a.ts", "src/b.ts"),
    );
    let handler = GuardMcpServer::new(Arc::new(Fake::default()), ServerOptions::default())
        .with_history(history)
        .with_map(map);
    let mut client = Client::start_server(handler);
    client.initialize_with("2025-06-18").await;
    (client, facts)
}

#[tokio::test]
async fn guard_history_reads_facts_through_the_reader() {
    let (mut client, _facts) = phase_four_client().await;
    let response = client
        .call("guard_history", json!({"scope": "journey"}))
        .await;
    let out = assert_ok_result("guard_history", &response);
    assert_eq!(out["repo"], "acme/shop");
    assert_eq!(out["facts"].as_array().unwrap().len(), 1);
    assert_eq!(out["facts"][0]["topic_key"], "flaky/checkout.customer-card");
    assert_eq!(out["facts"][0]["needs_review"], false);
    let bad = client.call("guard_history", json!({"limit": 0})).await;
    assert_rpc_error(&bad, -32602);
    let bad = client.call("guard_history", json!({"record": true})).await;
    assert_rpc_error(&bad, -32602);
}

#[tokio::test]
async fn guard_history_without_a_reader_is_server_not_configured() {
    let mut client = Client::ready(Arc::new(Fake::default())).await;
    let response = client.call("guard_history", json!({})).await;
    assert_tool_error(&response, "server_not_configured", 2);
}

#[tokio::test]
async fn guard_map_returns_a_budgeted_map() {
    let (mut client, _facts) = phase_four_client().await;
    let response = client
        .call(
            "guard_map",
            json!({"focus_files": ["src/a.ts"], "token_budget": 256}),
        )
        .await;
    let out = assert_ok_result("guard_map", &response);
    assert_eq!(out["backend"], "guard-impact");
    assert_eq!(out["commit"], "abc123");
    assert!(out["estimated_tokens"].as_u64().unwrap() <= 256);
    let bad = client.call("guard_map", json!({"token_budget": 5})).await;
    assert_rpc_error(&bad, -32602);
}

#[tokio::test]
async fn guard_map_without_a_graph_is_an_environment_error() {
    let mut client = Client::ready(Arc::new(Fake::default())).await;
    let response = client.call("guard_map", json!({})).await;
    assert_tool_error(&response, "environment_error", 3);
}
