//! The `guard_map` MCP tool definition and call handler, transport-free.
//!
//! `guard-mcp` (its integrator) registers [`tool_definition`] next to its own
//! tools and routes `tools/call` for [`TOOL_NAME`] to [`call_tool`], mapping
//! [`ToolCallError::InvalidParams`] to JSON-RPC `-32602` and
//! [`ToolCallError::Tool`] to an `isError: true` result whose
//! `structuredContent` is [`ToolCallError::structured_content`]
//! (mcp-tools.md conventions). This crate does not depend on `rmcp`.

use crate::{GraphSource, MapError, MapRequest, build_map};
use serde_json::{Value, json};

pub const TOOL_NAME: &str = "guard_map";

/// One `tools/list` entry, as plain JSON values.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Value,
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub open_world_hint: bool,
}

impl ToolDefinition {
    /// The wire form of a `tools/list` entry.
    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
            "outputSchema": self.output_schema,
            "annotations": {
                "readOnlyHint": self.read_only_hint,
                "destructiveHint": self.destructive_hint,
                "openWorldHint": self.open_world_hint,
            }
        })
    }
}

/// The exact schemas of docs/contracts/guard-map.md, "Tool I/O".
pub fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_NAME,
        description: "Ranked, token-budgeted map of the repository files and definitions most \
                      relevant to the ticket (personalized PageRank over the import graph, \
                      seeded by focus files and mentioned identifiers). Never returns file \
                      contents.",
        input_schema: json!({"type": "object", "additionalProperties": false, "properties": {
          "focus_files": {"type": "array", "items": {"type": "string"}, "description": "Files the ticket or diff names"},
          "mentioned": {"type": "array", "items": {"type": "string"}, "description": "Identifiers from the ticket"},
          "token_budget": {"type": "integer", "minimum": 128, "maximum": 4096, "default": 1024}
        }}),
        output_schema: json!({"type": "object", "required": ["backend", "commit", "token_budget", "estimated_tokens", "entries", "omitted"], "properties": {
          "backend": {"type": "string"},
          "commit": {"type": "string"},
          "token_budget": {"type": "integer"},
          "estimated_tokens": {"type": "integer"},
          "entries": {"type": "array", "items": {"type": "object", "required": ["path", "rank", "symbols"], "properties": {
            "path": {"type": "string"}, "rank": {"type": "number"},
            "symbols": {"type": "array", "items": {"type": "string"}}
          }}},
          "omitted": {"type": "integer"}
        }}),
        read_only_hint: true,
        destructive_hint: false,
        open_world_hint: false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolCallError {
    /// Arguments do not match the input schema → JSON-RPC `-32602`.
    #[error("invalid params: {0}")]
    InvalidParams(String),
    /// Tool-level failure → `isError: true`.
    #[error("{code}: {detail}")]
    Tool {
        code: &'static str,
        detail: String,
        exit_code: i32,
    },
}

impl ToolCallError {
    /// `structuredContent` of an `isError: true` result; `None` for `InvalidParams`.
    pub fn structured_content(&self) -> Option<Value> {
        match self {
            ToolCallError::InvalidParams(_) => None,
            ToolCallError::Tool {
                code,
                detail,
                exit_code,
            } => Some(json!({"error": code, "detail": detail, "exit_code": exit_code})),
        }
    }
}

/// Validates `arguments` against the input schema, then builds the map.
/// `arguments: null` (omitted) means `{}`.
pub fn call_tool(source: &dyn GraphSource, arguments: Value) -> Result<Value, ToolCallError> {
    let request = parse_arguments(arguments)?;
    match build_map(source, &request) {
        Ok(r) => Ok(serde_json::to_value(r).expect("MapResponse serializes")),
        Err(MapError::InvalidRequest(m)) => Err(ToolCallError::InvalidParams(m)),
        Err(MapError::Unavailable(m)) => Err(ToolCallError::Tool {
            code: "environment_error",
            detail: m,
            exit_code: 3,
        }),
    }
}

/// Validates `arguments` against the input schema (`null` = `{}`); used by
/// guard-mcp to answer `-32602` before dispatch.
pub fn parse_arguments(arguments: Value) -> Result<MapRequest, ToolCallError> {
    let obj = match arguments {
        Value::Null => serde_json::Map::new(),
        Value::Object(o) => o,
        _ => {
            return Err(ToolCallError::InvalidParams(
                "arguments must be an object".into(),
            ));
        }
    };
    if let Some(k) = obj
        .keys()
        .find(|k| !matches!(k.as_str(), "focus_files" | "mentioned" | "token_budget"))
    {
        return Err(ToolCallError::InvalidParams(format!(
            "unknown argument {k}"
        )));
    }
    let request: MapRequest = serde_json::from_value(Value::Object(obj))
        .map_err(|e| ToolCallError::InvalidParams(e.to_string()))?;
    request
        .validate()
        .map_err(|e| ToolCallError::InvalidParams(e.to_string()))?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeGraphSource;

    fn src() -> FakeGraphSource {
        FakeGraphSource::new("guard-impact", "abc").edge("a.ts", "b.ts")
    }

    #[test]
    fn definition_matches_the_contract_document() {
        let def = tool_definition();
        let doc = include_str!("../../docs/contracts/guard-map.md");
        let blocks: Vec<Value> = doc
            .split("```json")
            .skip(1)
            .map(|b| serde_json::from_str(b.split("```").next().unwrap()).unwrap())
            .collect();
        assert_eq!(
            blocks.len(),
            2,
            "Tool I/O has an input and an output schema"
        );
        assert_eq!(def.input_schema, blocks[0]);
        assert_eq!(def.output_schema, blocks[1]);
        let wire = def.to_json();
        assert_eq!(wire["name"], "guard_map");
        assert_eq!(wire["annotations"]["readOnlyHint"], true);
        assert_eq!(wire["annotations"]["destructiveHint"], false);
        assert_eq!(wire["annotations"]["openWorldHint"], false);
    }

    #[test]
    fn call_returns_the_map_json() {
        let out = call_tool(
            &src(),
            json!({"focus_files": ["a.ts"], "token_budget": 256}),
        )
        .unwrap();
        assert_eq!(out["backend"], "guard-impact");
        let paths: Vec<&str> = out["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"a.ts") && paths.contains(&"b.ts"));
        assert!(out["estimated_tokens"].as_u64().unwrap() <= 256);
        assert!(call_tool(&src(), Value::Null).is_ok());
    }

    #[test]
    fn bad_arguments_are_invalid_params() {
        for bad in [
            json!({"token_budget": 5}),
            json!({"token_budget": 99999}),
            json!({"focus_files": "a.ts"}),
            json!({"path": "x"}),
            json!([1]),
        ] {
            let err = call_tool(&src(), bad.clone()).unwrap_err();
            assert!(matches!(err, ToolCallError::InvalidParams(_)), "{bad}");
            assert_eq!(err.structured_content(), None);
        }
    }

    #[test]
    fn unavailable_index_is_an_environment_error() {
        let mut s = src();
        s.unavailable = Some("index built at another commit".into());
        let err = call_tool(&s, json!({})).unwrap_err();
        assert_eq!(
            err.structured_content().unwrap(),
            json!({"error": "environment_error", "detail": "index built at another commit", "exit_code": 3})
        );
    }
}
