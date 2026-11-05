use super::*;

const CONTRACT: &str = include_str!("../../../docs/contracts/mcp-tools.md");

/// The JSON code blocks of the `## Tools` section, in document order.
fn contract_schemas() -> Vec<Value> {
    let fence = "`".repeat(3);
    let tools = CONTRACT.split("## Tools").nth(1).expect("## Tools section");
    let tools = tools.split("## `guard repro").next().unwrap();
    tools
        .split(&format!("{fence}json"))
        .skip(1)
        .map(|chunk| {
            let body = chunk.split(fence.as_str()).next().unwrap();
            serde_json::from_str(body)
                .unwrap_or_else(|e| panic!("contract block is not JSON: {e}\n{body}"))
        })
        .collect()
}

#[test]
fn the_tool_list_is_exactly_the_contract_list() {
    let names: Vec<&str> = specs().iter().map(|s| s.name).collect();
    assert_eq!(names, TOOL_NAMES);
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
}

#[test]
fn no_tool_can_mutate_journeys_snapshots_or_the_verdict() {
    let forbidden = [
        "snapshot",
        "update",
        "edit",
        "delete",
        "write",
        "skip",
        "quarantine",
        "override",
        "bypass",
        "approve",
    ];
    for spec in specs() {
        for word in forbidden {
            assert!(
                !spec.name.contains(word),
                "{} looks mutating ({word})",
                spec.name
            );
        }
        let props = spec.input_schema["properties"].as_object().unwrap();
        for word in ["update_snapshots", "headed", "ui", "debug", "skip", "force"] {
            assert!(!props.contains_key(word), "{} accepts {word}", spec.name);
        }
        assert_eq!(
            spec.input_schema["additionalProperties"],
            json!(false),
            "{}",
            spec.name
        );
    }
}

#[test]
fn schemas_match_the_contract_document_exactly() {
    let blocks = contract_schemas();
    assert_eq!(blocks.len(), 10, "five Phase 2 tools x (input, output)");
    // The Phase 4 tools are pinned to their own contracts (memory.md and
    // guard-map.md) by the tests next to their definitions.
    for (index, spec) in specs().iter().take(5).enumerate() {
        assert_eq!(
            spec.input_schema,
            blocks[index * 2],
            "{} inputSchema",
            spec.name
        );
        assert_eq!(
            spec.output_schema,
            blocks[index * 2 + 1],
            "{} outputSchema",
            spec.name
        );
    }
}

#[test]
fn schemas_are_valid_draft_2020_12() {
    for spec in specs() {
        for schema in [&spec.input_schema, &spec.output_schema] {
            jsonschema::draft202012::meta::validate(schema)
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
            jsonschema::draft202012::new(schema).unwrap_or_else(|e| panic!("{}: {e}", spec.name));
        }
    }
}

#[test]
fn annotations_follow_the_contract() {
    for spec in specs() {
        let tool = serde_json::to_value(spec.to_tool()).unwrap();
        let runs = matches!(spec.name, "guard_test" | "guard_repro");
        assert_eq!(
            tool["annotations"]["readOnlyHint"],
            json!(!runs),
            "{}",
            spec.name
        );
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            json!(false),
            "{}",
            spec.name
        );
        assert_eq!(
            tool["annotations"]["openWorldHint"],
            json!(spec.name == "guard_repro"),
            "{}",
            spec.name
        );
        assert_eq!(tool["name"], json!(spec.name));
        assert_eq!(tool["inputSchema"], spec.input_schema);
        assert_eq!(tool["outputSchema"], spec.output_schema);
        assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
    }
}

#[test]
fn spec_lookup_finds_only_known_tools() {
    assert_eq!(spec("guard_catalog").unwrap().name, "guard_catalog");
    assert!(spec("guard_update_snapshots").is_none());
}

#[test]
fn phase_four_tools_reuse_their_own_contract_definitions() {
    assert_eq!(spec("guard_history").unwrap(), history::spec());
    let map = guard_map::tool::tool_definition();
    let spec = spec("guard_map").unwrap();
    assert_eq!(spec.input_schema, map.input_schema);
    assert_eq!(spec.output_schema, map.output_schema);
    assert_eq!(spec.description, map.description);
    assert!(spec.read_only && !spec.open_world);
}
