//! The tool definitions: names, descriptions, annotations and the exact
//! `inputSchema`/`outputSchema` of `docs/contracts/mcp-tools.md` (the five
//! Phase 2 tools), `memory.md` (`guard_history`) and `guard-map.md` (`guard_map`).
//!
//! This list is closed. Adding a tool that edits journeys or the catalog,
//! updates snapshots, changes config or skips/overrides the guard is a contract
//! violation; the tests assert the exact list and compare every schema with the
//! contract document.

pub mod history;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{Value, json};
use std::sync::Arc;

pub const GUARD_AFFECTED: &str = "guard_affected";
pub const GUARD_TEST: &str = "guard_test";
pub const GUARD_FAILURE: &str = "guard_failure";
pub const GUARD_REPRO: &str = "guard_repro";
pub const GUARD_CATALOG: &str = "guard_catalog";
pub use history::GUARD_HISTORY;
pub const GUARD_MAP: &str = guard_map::tool::TOOL_NAME;

/// Every tool name, in `tools/list` order.
pub const TOOL_NAMES: [&str; 7] = [
    GUARD_AFFECTED,
    GUARD_TEST,
    GUARD_FAILURE,
    GUARD_REPRO,
    GUARD_CATALOG,
    GUARD_HISTORY,
    GUARD_MAP,
];

/// Static description of one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Value,
    pub read_only: bool,
    pub open_world: bool,
}

impl ToolSpec {
    /// The wire definition (`tools/list` entry).
    pub fn to_tool(&self) -> Tool {
        let mut annotations = ToolAnnotations::default();
        annotations.read_only_hint = Some(self.read_only);
        annotations.destructive_hint = Some(false);
        annotations.open_world_hint = Some(self.open_world);
        let mut tool = Tool::new(self.name, self.description, object(&self.input_schema));
        tool.output_schema = Some(object(&self.output_schema));
        tool.annotations = Some(annotations);
        tool
    }
}

fn object(schema: &Value) -> Arc<JsonObject> {
    match schema {
        Value::Object(map) => Arc::new(map.clone()),
        _ => unreachable!("tool schemas are JSON objects"),
    }
}

/// The specs of every tool, in [`TOOL_NAMES`] order.
pub fn specs() -> Vec<ToolSpec> {
    vec![
        affected(),
        test(),
        failure(),
        repro(),
        catalog(),
        history::spec(),
        map(),
    ]
}

/// `guard_map`, from the definition guard-map owns (guard-map.md "Tool I/O").
fn map() -> ToolSpec {
    let d = guard_map::tool::tool_definition();
    ToolSpec {
        name: d.name,
        description: d.description,
        input_schema: d.input_schema,
        output_schema: d.output_schema,
        read_only: d.read_only_hint,
        open_world: d.open_world_hint,
    }
}

/// The spec of the tool `name`, if it exists.
pub fn spec(name: &str) -> Option<ToolSpec> {
    specs().into_iter().find(|s| s.name == name)
}

fn affected() -> ToolSpec {
    ToolSpec {
        name: GUARD_AFFECTED,
        description: "Journeys affected by the current diff and why (changed path or Jev impact \
                      analysis). Same selection as `guard affected --json`.",
        input_schema: json!({"type": "object", "additionalProperties": false, "properties": {
          "base": {"type": "string", "description": "Base ref for the diff; omitted = detected (cli.md, Base resolution)"}
        }}),
        output_schema: json!({"type": "object", "required": ["base", "base_source", "merge_base", "branch", "ticket", "changed_files", "tags", "journeys", "impact"], "properties": {
          "base": {"type": "string"},
          "base_source": {"enum": ["flag", "env", "ci", "remote_head", "remote_branch", "local_branch"]},
          "merge_base": {"type": "string"},
          "branch": {"type": ["string", "null"]},
          "ticket": {"type": ["string", "null"]},
          "changed_files": {"type": "array", "items": {"type": "string"}},
          "tags": {"type": "array", "items": {"type": "string"}},
          "journeys": {"type": "array", "items": {"type": "object", "required": ["id", "tag", "reasons"], "properties": {
            "id": {"type": "string"}, "tag": {"type": "string"},
            "reasons": {"type": "array", "items": {"type": "object", "required": ["kind"], "properties": {
              "kind": {"enum": ["path", "smoke", "impact", "all", "requested", "engine_unavailable"]},
              "file": {"type": "string"}, "glob": {"type": "string"}, "probability": {"type": "number"}
            }}}
          }}},
          "impact": {"type": "object", "required": ["source", "status"], "properties": {
            "source": {"enum": ["server", "local", "none"]},
            "status": {"enum": ["ok", "engine_unavailable", "not_configured"]},
            "threshold": {"type": "number"}
          }}
        }}),
        read_only: true,
        open_world: false,
    }
}

fn test() -> ToolSpec {
    ToolSpec {
        name: GUARD_TEST,
        description: "Runs the E2E tests headless (affected journeys by default) and returns what \
                      passed, what failed, the exact failing line and the screenshot path. Exit 1 \
                      (tests failed) is a normal result.",
        input_schema: json!({"type": "object", "additionalProperties": false, "properties": {
          "selection": {"enum": ["affected", "all", "journeys"], "default": "affected"},
          "journeys": {"type": "array", "items": {"type": "string"}, "description": "Journey ids; required when selection = journeys"},
          "base": {"type": "string", "description": "Base for selection = affected; omitted = detected"},
          "docker": {"type": "boolean", "default": false}
        }}),
        output_schema: json!({"type": "object", "required": ["exit_code", "counts", "duration_ms", "tags", "failures"], "properties": {
          "exit_code": {"type": "integer"},
          "counts": {"type": "object", "required": ["passed", "flaky", "failed", "skipped"], "properties": {
            "passed": {"type": "integer"}, "flaky": {"type": "integer"}, "failed": {"type": "integer"}, "skipped": {"type": "integer"}}},
          "duration_ms": {"type": "integer"},
          "tags": {"type": "array", "items": {"type": "string"}},
          "failures": {"type": "array", "items": {"$ref": "#/$defs/failure_summary"}}
        },
        "$defs": {"failure_summary": {"type": "object", "required": ["test_id", "title", "file", "line", "status", "error"], "properties": {
          "test_id": {"type": "string", "description": "<project>::<file>::<title>"},
          "title": {"type": "string"}, "file": {"type": "string"}, "line": {"type": "integer"},
          "journey": {"type": ["string", "null"]},
          "status": {"enum": ["failed", "flaky"]},
          "error": {"type": "string"},
          "location": {"type": ["string", "null"], "description": "file:line of the failing step"},
          "screenshot": {"type": ["string", "null"], "description": "Repo-relative path under e2e-playwright/test-results/"}
        }}}}),
        read_only: false,
        open_world: false,
    }
}

fn failure() -> ToolSpec {
    ToolSpec {
        name: GUARD_FAILURE,
        description: "Detail of one failure (error, steps, attempts, baseline, screenshot, trace) \
                      from the last local run, or with `pr` from the latest remote guard gate of \
                      that pull request.",
        input_schema: json!({"type": "object", "additionalProperties": false, "required": ["test_id"], "properties": {
          "test_id": {"type": "string"},
          "pr": {"type": "integer", "minimum": 1}
        }}),
        output_schema: json!({"type": "object", "required": ["test_id", "source", "title", "file", "line", "error", "steps", "attempts"], "properties": {
          "test_id": {"type": "string"},
          "source": {"enum": ["local", "remote"]},
          "title": {"type": "string"}, "file": {"type": "string"}, "line": {"type": "integer"},
          "journey": {"type": ["string", "null"]},
          "location": {"type": ["string", "null"]},
          "error": {"type": "string", "description": "Full error message, ANSI stripped"},
          "steps": {"type": "array", "items": {"type": "string"}, "description": "Titles of the steps up to the failing one"},
          "attempts": {"type": "array", "items": {"type": "object", "properties": {
            "retry": {"type": "integer"}, "status": {"type": "string"}, "duration_ms": {"type": "integer"}}}},
          "baseline": {"enum": ["regression", "pre_existing", "flaky", "unavailable", null]},
          "fingerprint": {"type": ["string", "null"]},
          "screenshot": {"type": ["string", "null"], "description": "Local path, or presigned URL for remote"},
          "trace_url": {"type": ["string", "null"], "description": "Local trace.zip path, or the trace.playwright.dev viewer URL"},
          "jira": {"type": ["string", "null"], "description": "Bug key for remote failures"}
        }}),
        read_only: true,
        open_world: false,
    }
}

fn repro() -> ToolSpec {
    ToolSpec {
        name: GUARD_REPRO,
        description: "Reproduces a remote guard failure locally against the current working tree \
                      (`guard repro <test-id> --pr <n>`) and reports whether it failed the same way.",
        input_schema: json!({"type": "object", "additionalProperties": false, "required": ["test_id", "pr"], "properties": {
          "test_id": {"type": "string"}, "pr": {"type": "integer", "minimum": 1}
        }}),
        output_schema: json!({"type": "object", "required": ["reproduced", "exit_code", "evidence_dir", "remote", "local"], "properties": {
          "reproduced": {"type": "boolean", "description": "The local run failed the same way (same fingerprint)"},
          "exit_code": {"type": "integer"},
          "evidence_dir": {"type": "string", "description": ".guard/repro/<fingerprint>/ with the downloaded screenshot and trace"},
          "remote": {"$ref": "#/$defs/failure_ref"},
          "local": {"oneOf": [{"$ref": "#/$defs/failure_ref"}, {"type": "null"}]}
        },
        "$defs": {"failure_ref": {"type": "object", "required": ["status", "fingerprint"], "properties": {
          "status": {"enum": ["passed", "flaky", "failed"]}, "fingerprint": {"type": ["string", "null"]}, "error": {"type": ["string", "null"]}}}}}),
        read_only: false,
        open_world: true,
    }
}

fn catalog() -> ToolSpec {
    ToolSpec {
        name: GUARD_CATALOG,
        description: "Searches the journey catalog by area, tag or text (read-only view of \
                      catalog/journeys.json). Filters combine with AND; no filter returns every \
                      journey.",
        input_schema: json!({"type": "object", "additionalProperties": false, "properties": {
          "area": {"type": "string"},
          "tag": {"type": "string", "description": "e.g. @smoke or @journey:checkout.customer-card"},
          "text": {"type": "string", "description": "Case-insensitive match on id, title and impact_question"},
          "include_quarantined": {"type": "boolean", "default": true}
        }}),
        output_schema: json!({"type": "object", "required": ["journeys"], "properties": {"journeys": {"type": "array", "items": {"type": "object",
          "required": ["id", "area", "title", "tags", "paths", "smoke", "quarantined"], "properties": {
          "id": {"type": "string"}, "area": {"type": "string"}, "title": {"type": "string"},
          "tags": {"type": "array", "items": {"type": "string"}, "description": "@journey:<id>, @area:<area>, plus @smoke when smoke"},
          "paths": {"type": "array", "items": {"type": "string"}},
          "smoke": {"type": "boolean"}, "quarantined": {"type": "boolean"}
        }}}}}),
        read_only: true,
        open_world: false,
    }
}

#[cfg(test)]
mod tests;
