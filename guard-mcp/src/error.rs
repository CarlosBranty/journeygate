//! Tool-level failures: reported as `isError: true` results, never as JSON-RPC
//! errors (those are reserved for protocol and argument problems).

use serde::Serialize;

/// The `error` code of an `isError` result (`docs/contracts/mcp-tools.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    /// Bad configuration or arguments the guard rejects (exit `2`).
    ConfigError,
    /// App not ready, Playwright crash, missing tool (exit `3`).
    EnvironmentError,
    /// Unknown test id, no gate result for the PR.
    NotFound,
    /// `pr` given but `[server]` is missing from `target.toml`.
    ServerNotConfigured,
    /// `guard serve` answered with an error or is unreachable.
    ServerError,
    /// A long run (`guard_test`, `guard_repro`) exceeded its timeout.
    Timeout,
}

impl ToolErrorCode {
    /// The guard exit code reported when the caller does not give one.
    pub fn default_exit_code(self) -> i32 {
        match self {
            Self::ConfigError | Self::NotFound | Self::ServerNotConfigured => 2,
            Self::EnvironmentError | Self::ServerError | Self::Timeout => 3,
        }
    }
}

/// A tool-level failure; serializes to the `structuredContent` of an `isError`
/// result: `{"error": "<code>", "detail": "<text>", "exit_code": <n>}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{error:?}: {detail}")]
pub struct ToolError {
    pub error: ToolErrorCode,
    pub detail: String,
    pub exit_code: i32,
}

impl ToolError {
    /// A failure with the code's default exit code.
    pub fn new(error: ToolErrorCode, detail: impl Into<String>) -> Self {
        Self {
            error,
            detail: detail.into(),
            exit_code: error.default_exit_code(),
        }
    }

    pub fn with_exit_code(mut self, exit_code: i32) -> Self {
        self.exit_code = exit_code;
        self
    }

    pub fn config(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::ConfigError, detail)
    }

    pub fn environment(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::EnvironmentError, detail)
    }

    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::NotFound, detail)
    }

    pub fn server_not_configured(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::ServerNotConfigured, detail)
    }

    pub fn server(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::ServerError, detail)
    }

    pub fn timeout(detail: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::Timeout, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_the_contract_error_shape() {
        let value = serde_json::to_value(ToolError::not_found("no test a::b::c")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"error": "not_found", "detail": "no test a::b::c", "exit_code": 2})
        );
    }

    #[test]
    fn config_and_environment_errors_carry_guard_exit_codes() {
        assert_eq!(ToolError::config("x").exit_code, 2);
        assert_eq!(ToolError::environment("x").exit_code, 3);
        assert_eq!(ToolError::timeout("x").exit_code, 3);
        assert_eq!(ToolError::server("x").with_exit_code(4).exit_code, 4);
    }

    #[test]
    fn every_code_uses_snake_case() {
        let codes = [
            (ToolErrorCode::ConfigError, "config_error"),
            (ToolErrorCode::EnvironmentError, "environment_error"),
            (ToolErrorCode::NotFound, "not_found"),
            (ToolErrorCode::ServerNotConfigured, "server_not_configured"),
            (ToolErrorCode::ServerError, "server_error"),
            (ToolErrorCode::Timeout, "timeout"),
        ];
        for (code, text) in codes {
            assert_eq!(serde_json::to_value(code).unwrap(), serde_json::json!(text));
        }
    }
}
