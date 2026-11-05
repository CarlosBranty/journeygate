//! Validation of `tools/call` arguments against each tool's `inputSchema`.
//!
//! A mismatch is an [`ArgError`], which the server answers with JSON-RPC error
//! `-32602` (invalid params), never with an `isError` result.

use crate::catalog_view::CatalogQuery;
use crate::tools::history::{self, HistoryRequest};
use crate::tools::{
    GUARD_AFFECTED, GUARD_CATALOG, GUARD_FAILURE, GUARD_HISTORY, GUARD_MAP, GUARD_REPRO, GUARD_TEST,
};
use crate::types::{AffectedRequest, FailureRequest, ReproRequest, TestRequest, TestSelection};
use serde::Deserialize;
use serde_json::{Map, Value};

/// A validated tool invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCall {
    Affected(AffectedRequest),
    Test(TestRequest),
    Failure(FailureRequest),
    Repro(ReproRequest),
    Catalog(CatalogQuery),
    History(HistoryRequest),
    Map(guard_map::MapRequest),
}

/// Unknown tool or arguments that do not match the tool's `inputSchema`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ArgError(pub String);

/// Parses `arguments` (absent = `{}`) for the tool `name`.
pub fn parse_call(name: &str, arguments: Option<Map<String, Value>>) -> Result<ToolCall, ArgError> {
    if name == GUARD_HISTORY {
        return history::parse_args(arguments).map(ToolCall::History);
    }
    let arguments = Value::Object(arguments.unwrap_or_default());
    if name == GUARD_MAP {
        return guard_map::tool::parse_arguments(arguments)
            .map(ToolCall::Map)
            .map_err(|e| ArgError(format!("{name}: {e}")));
    }
    match name {
        GUARD_AFFECTED => {
            let a: AffectedArgs = decode(name, arguments)?;
            Ok(ToolCall::Affected(AffectedRequest { base: a.base }))
        }
        GUARD_TEST => {
            let a: TestArgs = decode(name, arguments)?;
            let selection = match a.selection {
                SelectionArg::Affected => TestSelection::Affected,
                SelectionArg::All => TestSelection::All,
                SelectionArg::Journeys => match a.journeys {
                    Some(ids) if !ids.is_empty() => TestSelection::Journeys(ids),
                    _ => {
                        return Err(ArgError(format!(
                            "{name}: `journeys` (non-empty) is required when selection = journeys"
                        )));
                    }
                },
            };
            Ok(ToolCall::Test(TestRequest {
                selection,
                base: a.base,
                docker: a.docker,
            }))
        }
        GUARD_FAILURE => {
            let a: FailureArgs = decode(name, arguments)?;
            let pr = a.pr.map(|pr| positive(name, pr)).transpose()?;
            Ok(ToolCall::Failure(FailureRequest {
                test_id: a.test_id,
                pr,
            }))
        }
        GUARD_REPRO => {
            let a: ReproArgs = decode(name, arguments)?;
            Ok(ToolCall::Repro(ReproRequest {
                test_id: a.test_id,
                pr: positive(name, a.pr)?,
            }))
        }
        GUARD_CATALOG => {
            let a: CatalogArgs = decode(name, arguments)?;
            Ok(ToolCall::Catalog(CatalogQuery {
                area: a.area,
                tag: a.tag,
                text: a.text,
                include_quarantined: a.include_quarantined,
            }))
        }
        other => Err(ArgError(format!("unknown tool `{other}`"))),
    }
}

fn decode<T: serde::de::DeserializeOwned>(tool: &str, arguments: Value) -> Result<T, ArgError> {
    serde_json::from_value(arguments).map_err(|e| ArgError(format!("{tool}: {e}")))
}

/// `{"type": "integer", "minimum": 1}`; serde already rejected non-integers.
fn positive(tool: &str, pr: i64) -> Result<u64, ArgError> {
    u64::try_from(pr)
        .ok()
        .filter(|pr| *pr >= 1)
        .ok_or_else(|| ArgError(format!("{tool}: `pr` must be an integer >= 1, got {pr}")))
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AffectedArgs {
    #[serde(default)]
    base: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum SelectionArg {
    #[default]
    Affected,
    All,
    Journeys,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestArgs {
    #[serde(default)]
    selection: SelectionArg,
    #[serde(default)]
    journeys: Option<Vec<String>>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    docker: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FailureArgs {
    test_id: String,
    #[serde(default)]
    pr: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReproArgs {
    test_id: String,
    pr: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogArgs {
    #[serde(default)]
    area: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default = "yes")]
    include_quarantined: bool,
}

#[cfg(test)]
mod tests;
