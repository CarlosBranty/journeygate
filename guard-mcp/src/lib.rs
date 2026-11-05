//! `guard mcp`: a Model Context Protocol server over stdio (JSON-RPC 2.0,
//! newline-delimited; logs go to stderr only).
//!
//! Exposes exactly the read/run tools of `docs/contracts/mcp-tools.md`:
//! `guard_affected`, `guard_test`, `guard_failure`, `guard_repro`,
//! `guard_catalog`, and the Phase 4 read-only `guard_history` (memory facts
//! through a reader) and `guard_map` (repository map). There is deliberately
//! no tool that edits journeys, the catalog or memory, or the
//! catalog, updates snapshots, changes config or skips/overrides the guard; the
//! [`GuardActions`] port does not even offer such operations.
//!
//! The protocol layer is the official Rust MCP SDK (`rmcp`, pinned), narrowed to
//! protocol revision `2025-06-18`.

pub mod actions;
pub mod args;
pub mod catalog_view;
pub mod error;
pub mod server;
pub mod tools;
pub mod types;

pub use actions::GuardActions;
pub use error::{ToolError, ToolErrorCode};
pub use server::{GuardMcpServer, PROTOCOL_VERSION, SERVER_NAME, ServerOptions};

use rmcp::ServiceExt;
use rmcp::service::ServerInitializeError;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// The server could not start or stopped abnormally.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("mcp initialize failed: {0}")]
    Initialize(String),
    #[error("mcp server task failed: {0}")]
    Task(String),
    #[error("cannot start the async runtime: {0}")]
    Runtime(String),
}

/// Serves MCP on `reader`/`writer` until the client closes the input.
pub async fn serve<R, W>(
    actions: Arc<dyn GuardActions>,
    options: ServerOptions,
    reader: R,
    writer: W,
) -> Result<(), ServeError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    serve_handler(GuardMcpServer::new(actions, options), reader, writer).await
}

/// Serves an already configured handler (with `guard_history` / `guard_map`).
pub async fn serve_handler<R, W>(
    handler: GuardMcpServer,
    reader: R,
    writer: W,
) -> Result<(), ServeError>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let running = match handler.serve((reader, writer)).await {
        Ok(running) => running,
        // The client went away before `initialize`: a normal end.
        Err(ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(error) => return Err(ServeError::Initialize(error.to_string())),
    };
    running
        .waiting()
        .await
        .map(|_| ())
        .map_err(|e| ServeError::Task(e.to_string()))
}

/// `guard mcp`: serves on the process stdin/stdout with default options,
/// blocking the calling thread (guard-cli is synchronous).
pub fn run_stdio(actions: Arc<dyn GuardActions>) -> Result<(), ServeError> {
    run_stdio_with(GuardMcpServer::new(actions, ServerOptions::default()))
}

/// `guard mcp` with a configured handler.
pub fn run_stdio_with(handler: GuardMcpServer) -> Result<(), ServeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| ServeError::Runtime(e.to_string()))?;
    runtime.block_on(serve_handler(
        handler,
        tokio::io::stdin(),
        tokio::io::stdout(),
    ))
}
