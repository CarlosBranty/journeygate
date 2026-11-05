//! Atlassian Document Format (ADF) builder and flattener.
//!
//! Builders return `serde_json::Value` nodes so callers compose documents with
//! plain function calls and tests compare them against golden JSON. Text nodes
//! with empty text are dropped: Jira rejects them (`INVALID_INPUT`).
//!
//! Supported nodes: `doc`, `heading`, `paragraph`, `bulletList`/`listItem`,
//! `codeBlock`, `table`/`tableRow`/`tableHeader`/`tableCell`, `text` with the
//! marks `strong`, `code` and `link`.

use serde_json::{Value, json};

/// A complete document: `{"type": "doc", "version": 1, "content": [...]}`.
pub fn doc(blocks: Vec<Value>) -> Value {
    json!({"type": "doc", "version": 1, "content": blocks})
}

/// Plain text node.
pub fn text(s: impl Into<String>) -> Value {
    json!({"type": "text", "text": s.into()})
}

/// Bold text.
pub fn strong(s: impl Into<String>) -> Value {
    json!({"type": "text", "text": s.into(), "marks": [{"type": "strong"}]})
}

/// Inline code.
pub fn code(s: impl Into<String>) -> Value {
    json!({"type": "text", "text": s.into(), "marks": [{"type": "code"}]})
}

/// A hyperlink with visible `label`.
pub fn link(label: impl Into<String>, href: impl Into<String>) -> Value {
    json!({
        "type": "text",
        "text": label.into(),
        "marks": [{"type": "link", "attrs": {"href": href.into()}}]
    })
}

fn inline_content(inlines: Vec<Value>) -> Vec<Value> {
    inlines
        .into_iter()
        .filter(|n| n.get("type") != Some(&json!("text")) || n["text"].as_str() != Some(""))
        .collect()
}

/// `heading` of `level` 1..=6 (clamped).
pub fn heading(level: u8, inlines: Vec<Value>) -> Value {
    json!({
        "type": "heading",
        "attrs": {"level": level.clamp(1, 6)},
        "content": inline_content(inlines)
    })
}

pub fn paragraph(inlines: Vec<Value>) -> Value {
    json!({"type": "paragraph", "content": inline_content(inlines)})
}

/// A bullet list; each item is one paragraph of inline nodes.
pub fn bullet_list(items: Vec<Vec<Value>>) -> Value {
    let items: Vec<Value> = items
        .into_iter()
        .map(|inlines| json!({"type": "listItem", "content": [paragraph(inlines)]}))
        .collect();
    json!({"type": "bulletList", "content": items})
}

/// A code block; `language` is omitted when `None`.
pub fn code_block(body: &str, language: Option<&str>) -> Value {
    let content: Vec<Value> = if body.is_empty() {
        vec![]
    } else {
        vec![text(body)]
    };
    let mut node = json!({"type": "codeBlock", "content": content});
    if let Some(lang) = language {
        node["attrs"] = json!({"language": lang});
    }
    node
}

/// A table with one header row and body rows; every cell is one paragraph.
pub fn table(header: &[&str], rows: Vec<Vec<Vec<Value>>>) -> Value {
    let cell = |kind: &str, inlines: Vec<Value>| json!({"type": kind, "attrs": {}, "content": [paragraph(inlines)]});
    let mut table_rows = Vec::with_capacity(rows.len() + 1);
    if !header.is_empty() {
        let cells: Vec<Value> = header
            .iter()
            .map(|h| cell("tableHeader", vec![strong(*h)]))
            .collect();
        table_rows.push(json!({"type": "tableRow", "content": cells}));
    }
    for row in rows {
        let cells: Vec<Value> = row.into_iter().map(|c| cell("tableCell", c)).collect();
        table_rows.push(json!({"type": "tableRow", "content": cells}));
    }
    json!({
        "type": "table",
        "attrs": {"isNumberColumnEnabled": false, "layout": "default"},
        "content": table_rows
    })
}

/// Joins inline groups with `separator` text (e.g. ` · `).
pub fn join(groups: Vec<Value>, separator: &str) -> Vec<Value> {
    let mut out = Vec::with_capacity(groups.len() * 2);
    for (i, node) in groups.into_iter().enumerate() {
        if i > 0 {
            out.push(text(separator));
        }
        out.push(node);
    }
    out
}

/// Flattens an ADF document (or a plain string) to text: blocks separated by a
/// blank line, list items on their own line prefixed with `- `, table rows
/// with cells joined by ` | `, hard breaks as newlines. `null` is empty.
pub fn to_plain_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        _ => blocks_text(value.get("content").unwrap_or(&Value::Null)),
    }
}

fn children(node: &Value) -> &[Value] {
    node.get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn blocks_text(content: &Value) -> String {
    let blocks: Vec<String> = content
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(block_text)
        .filter(|s| !s.is_empty())
        .collect();
    blocks.join("\n\n")
}

fn block_text(node: &Value) -> String {
    match node["type"].as_str().unwrap_or_default() {
        "bulletList" | "orderedList" | "taskList" => list_lines(node).join("\n"),
        "table" => children(node)
            .iter()
            .map(|row| {
                children(row)
                    .iter()
                    .map(|cell| blocks_text(&Value::Array(children(cell).to_vec())))
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        "paragraph" | "heading" | "codeBlock" => inline_text(node),
        "rule" => String::new(),
        _ if node.get("text").is_some() => inline_text(node),
        // blockquote, panel, expand, ...: recurse into their blocks.
        _ => blocks_text(node.get("content").unwrap_or(&Value::Null)),
    }
}

fn list_lines(list: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for item in children(list) {
        let mut own = Vec::new();
        let mut nested = Vec::new();
        if item["type"] == "taskItem" {
            own.push(inline_text(item));
        } else {
            for child in children(item) {
                match child["type"].as_str().unwrap_or_default() {
                    "bulletList" | "orderedList" | "taskList" => {
                        nested.extend(list_lines(child).into_iter().map(|l| format!("  {l}")))
                    }
                    _ => own.push(block_text(child)),
                }
            }
        }
        lines.push(format!("- {}", own.join(" ").trim()));
        lines.extend(nested);
    }
    lines
}

fn inline_text(node: &Value) -> String {
    if let Some(t) = node.get("text").and_then(Value::as_str) {
        return t.to_string();
    }
    let mut out = String::new();
    for child in children(node) {
        match child["type"].as_str().unwrap_or_default() {
            "hardBreak" => out.push('\n'),
            "mention" | "emoji" | "status" => {
                out.push_str(child["attrs"]["text"].as_str().unwrap_or_default())
            }
            "inlineCard" => out.push_str(child["attrs"]["url"].as_str().unwrap_or_default()),
            _ => out.push_str(&inline_text(child)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_nodes_and_marks() {
        assert_eq!(text("a"), json!({"type": "text", "text": "a"}));
        assert_eq!(
            strong("b"),
            json!({"type": "text", "text": "b", "marks": [{"type": "strong"}]})
        );
        assert_eq!(
            code("c"),
            json!({"type": "text", "text": "c", "marks": [{"type": "code"}]})
        );
        assert_eq!(
            link("Trace", "https://x"),
            json!({"type": "text", "text": "Trace", "marks": [{"type": "link", "attrs": {"href": "https://x"}}]})
        );
    }

    #[test]
    fn blocks() {
        assert_eq!(
            doc(vec![
                heading(9, vec![text("H")]),
                paragraph(vec![text(""), text("p")])
            ]),
            json!({"type": "doc", "version": 1, "content": [
                {"type": "heading", "attrs": {"level": 6}, "content": [{"type": "text", "text": "H"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "p"}]}
            ]})
        );
        assert_eq!(
            code_block("x()", Some("ts")),
            json!({"type": "codeBlock", "attrs": {"language": "ts"}, "content": [{"type": "text", "text": "x()"}]})
        );
        assert_eq!(
            code_block("", None),
            json!({"type": "codeBlock", "content": []})
        );
        assert_eq!(
            bullet_list(vec![vec![text("one")]]),
            json!({"type": "bulletList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "one"}]}]}
            ]})
        );
    }

    #[test]
    fn table_layout() {
        assert_eq!(
            table(
                &["Test", "Jira"],
                vec![vec![vec![code("a")], vec![link("SHOP-1", "u")]]]
            ),
            json!({"type": "table", "attrs": {"isNumberColumnEnabled": false, "layout": "default"}, "content": [
                {"type": "tableRow", "content": [
                    {"type": "tableHeader", "attrs": {}, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Test", "marks": [{"type": "strong"}]}]}]},
                    {"type": "tableHeader", "attrs": {}, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "Jira", "marks": [{"type": "strong"}]}]}]}
                ]},
                {"type": "tableRow", "content": [
                    {"type": "tableCell", "attrs": {}, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "a", "marks": [{"type": "code"}]}]}]},
                    {"type": "tableCell", "attrs": {}, "content": [{"type": "paragraph", "content": [{"type": "text", "text": "SHOP-1", "marks": [{"type": "link", "attrs": {"href": "u"}}]}]}]}
                ]}
            ]})
        );
    }

    #[test]
    fn join_inserts_separators() {
        assert_eq!(
            join(vec![text("a"), text("b")], " · "),
            vec![text("a"), text(" · "), text("b")]
        );
        assert!(join(vec![], " · ").is_empty());
    }

    #[test]
    fn plain_text_flattening() {
        let d = doc(vec![
            heading(3, vec![text("Title")]),
            paragraph(vec![
                text("a "),
                code("b"),
                json!({"type": "hardBreak"}),
                text("c"),
            ]),
            bullet_list(vec![vec![text("one")], vec![text("two")]]),
            code_block("x()", None),
            table(&["H1", "H2"], vec![vec![vec![text("1")], vec![text("2")]]]),
        ]);
        assert_eq!(
            to_plain_text(&d),
            "Title\n\na b\nc\n\n- one\n- two\n\nx()\n\nH1 | H2\n1 | 2"
        );
        assert_eq!(to_plain_text(&json!("wiki *text*")), "wiki *text*");
        assert_eq!(to_plain_text(&Value::Null), "");
    }

    #[test]
    fn nested_lists_and_mentions() {
        let d = json!({"type": "doc", "version": 1, "content": [
            {"type": "orderedList", "content": [
                {"type": "listItem", "content": [
                    {"type": "paragraph", "content": [
                        {"type": "mention", "attrs": {"id": "1", "text": "@Ana"}},
                        {"type": "text", "text": " checks"}
                    ]},
                    {"type": "bulletList", "content": [
                        {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "inner"}]}]}
                    ]}
                ]}
            ]},
            {"type": "blockquote", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "quoted"}]}]}
        ]});
        assert_eq!(to_plain_text(&d), "- @Ana checks\n  - inner\n\nquoted");
    }
}
