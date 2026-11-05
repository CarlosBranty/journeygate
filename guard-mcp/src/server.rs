//! The MCP server: an rmcp [`ServerHandler`] that validates `tools/call`
//! arguments, calls [`GuardActions`] and shapes the results per
//! `docs/contracts/mcp-tools.md`.

use crate::actions::GuardActions;
use crate::args::{ToolCall, parse_call};
use crate::catalog_view;
use crate::error::{ToolError, ToolErrorCode};
use crate::tools::history::{self, HistoryContext};
use crate::tools::{self, TOOL_NAMES};
use guard_map::{GraphSource, MapError};
use rmcp::ErrorData as McpError;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CompleteRequestMethod,
    CompleteRequestParams, CompleteResult, ContentBlock, DiscoverRequestMethod, DiscoverResult,
    Implementation, InitializeResult, ListPromptsRequestMethod, ListPromptsResult,
    ListResourceTemplatesRequestMethod, ListResourceTemplatesResult, ListResourcesRequestMethod,
    ListResourcesResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
    ServerCapabilities, Tool, ToolsCapability,
};
use rmcp::service::{RequestContext, RoleServer};
use serde::Serialize;
use serde_json::Value;
use std::borrow::Cow;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// The only protocol revision this server speaks; every `initialize` gets it.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The name in `serverInfo`.
pub const SERVER_NAME: &str = "guard";

/// Runtime knobs of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOptions {
    /// Timeout of `guard_test` and `guard_repro` (contract: 30 minutes).
    pub long_run_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            long_run_timeout: Duration::from_secs(30 * 60),
        }
    }
}

/// The `guard mcp` handler.
///
/// The Phase 4 tools are read-only by construction: `guard_history` holds a
/// [`HistoryContext`] (a `MemoryFactsReader`, never a writer) and `guard_map`
/// a [`GraphSource`] (an in-memory graph). Without them the tools answer
/// `server_not_configured` / `environment_error`.
#[derive(Clone)]
pub struct GuardMcpServer {
    actions: Arc<dyn GuardActions>,
    options: ServerOptions,
    history: Option<HistoryContext>,
    map: Option<Arc<dyn GraphSource>>,
    map_unavailable: Option<String>,
}

impl GuardMcpServer {
    pub fn new(actions: Arc<dyn GuardActions>, options: ServerOptions) -> Self {
        Self {
            actions,
            options,
            history: None,
            map: None,
            map_unavailable: None,
        }
    }

    /// `guard_history` reads facts through this reader-only context.
    pub fn with_history(mut self, history: HistoryContext) -> Self {
        self.history = Some(history);
        self
    }

    /// `guard_map` ranks this graph.
    pub fn with_map(mut self, source: Arc<dyn GraphSource>) -> Self {
        self.map = Some(source);
        self
    }

    /// Why no graph is available (reported by `guard_map` as `environment_error`).
    pub fn with_map_unavailable(mut self, reason: impl Into<String>) -> Self {
        self.map_unavailable = Some(reason.into());
        self
    }

    async fn map(&self, request: guard_map::MapRequest) -> Result<Value, ToolError> {
        let Some(source) = self.map.clone() else {
            return Err(ToolError::environment(
                self.map_unavailable
                    .clone()
                    .unwrap_or_else(|| "guard_map: no code graph is configured".into()),
            ));
        };
        let built = tokio::task::spawn_blocking(move || guard_map::build_map(&*source, &request))
            .await
            .map_err(|e| ToolError::environment(format!("guard_map task failed: {e}")))?;
        match built {
            Ok(response) => serde_json::to_value(response)
                .map_err(|e| ToolError::environment(format!("cannot serialize the map: {e}"))),
            Err(MapError::InvalidRequest(m)) => Err(ToolError::config(m)),
            Err(MapError::Unavailable(m)) => Err(ToolError::environment(m)),
        }
    }

    /// Runs one validated call and shapes its `tools/call` result.
    pub async fn dispatch(&self, call: ToolCall) -> CallToolResult {
        let outcome = match call {
            ToolCall::Affected(request) => to_value(self.actions.affected(request).await),
            ToolCall::Test(request) => {
                let run = self
                    .long_run("guard_test", self.actions.test(request))
                    .await;
                to_value(run.and_then(|out| check_exit("guard test", out.exit_code, out)))
            }
            ToolCall::Failure(request) => to_value(self.actions.failure(request).await),
            ToolCall::Repro(request) => {
                let run = self
                    .long_run("guard_repro", self.actions.repro(request))
                    .await;
                to_value(run.and_then(|out| check_exit("guard repro", out.exit_code, out)))
            }
            ToolCall::Catalog(query) => to_value(
                self.actions
                    .catalog()
                    .await
                    .map(|catalog| catalog_view::search(&catalog, &query)),
            ),
            ToolCall::History(request) => {
                to_value(history::run(self.history.as_ref(), request).await)
            }
            ToolCall::Map(request) => self.map(request).await,
        };
        match outcome {
            Ok(value) => tool_result(value, false),
            Err(error) => tool_result(
                serde_json::to_value(&error).expect("ToolError serializes"),
                true,
            ),
        }
    }

    async fn long_run<T>(
        &self,
        tool: &str,
        run: impl Future<Output = Result<T, ToolError>>,
    ) -> Result<T, ToolError> {
        let limit = self.options.long_run_timeout;
        tokio::time::timeout(limit, run).await.unwrap_or_else(|_| {
            Err(ToolError::timeout(format!(
                "{tool} did not finish within {}s",
                limit.as_secs_f64()
            )))
        })
    }
}

/// Exit `2`/`3` are tool-level failures even when the run produced an output.
fn check_exit<T>(command: &str, exit_code: i32, output: T) -> Result<T, ToolError> {
    let code = match exit_code {
        2 => ToolErrorCode::ConfigError,
        3 => ToolErrorCode::EnvironmentError,
        _ => return Ok(output),
    };
    Err(
        ToolError::new(code, format!("{command} exited with code {exit_code}"))
            .with_exit_code(exit_code),
    )
}

fn to_value<T: Serialize>(outcome: Result<T, ToolError>) -> Result<Value, ToolError> {
    outcome.and_then(|output| {
        serde_json::to_value(output)
            .map_err(|e| ToolError::environment(format!("cannot serialize tool output: {e}")))
    })
}

/// `{"content": [{"type": "text", "text": <pretty JSON>}], "structuredContent": <value>, "isError": <bool>}`.
fn tool_result(value: Value, is_error: bool) -> CallToolResult {
    let text = serde_json::to_string_pretty(&value).expect("JSON values serialize");
    let mut result = if is_error {
        CallToolResult::structured_error(value)
    } else {
        CallToolResult::structured(value)
    };
    result.content = vec![ContentBlock::text(text)];
    result
}

impl ServerHandler for GuardMcpServer {
    fn get_info(&self) -> InitializeResult {
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(tools);
        let mut info = InitializeResult::new(capabilities);
        info.protocol_version = ProtocolVersion::V_2025_06_18;
        info.server_info = Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION"));
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![ProtocolVersion::V_2025_06_18])
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            tools::specs()
                .iter()
                .map(tools::ToolSpec::to_tool)
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let call = parse_call(&request.name, request.arguments)
            .map_err(|e| McpError::invalid_params(e.0, None))?;
        Ok(CallToolResponse::Complete(self.dispatch(call).await))
    }

    // rmcp answers these with empty successes by default; the contract says
    // anything but initialize/ping/tools/* is -32601 (method not found).

    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, McpError> {
        Err(McpError::method_not_found::<DiscoverRequestMethod>())
    }

    async fn complete(
        &self,
        _request: CompleteRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, McpError> {
        Err(McpError::method_not_found::<CompleteRequestMethod>())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Err(McpError::method_not_found::<ListPromptsRequestMethod>())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Err(McpError::method_not_found::<ListResourcesRequestMethod>())
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Err(McpError::method_not_found::<
            ListResourceTemplatesRequestMethod,
        >())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        TOOL_NAMES
            .contains(&name)
            .then(|| tools::spec(name).map(|s| s.to_tool()))
            .flatten()
    }
}
