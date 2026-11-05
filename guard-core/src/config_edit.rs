//! Minimal, layout-preserving edits of `target.toml` (`guard connect`).
//!
//! Only top-level `[table]` headers are understood: a key of that table is
//! replaced in place, a missing key is added after the table's last entry and
//! a missing table is appended. Every other byte, comments included, stays as
//! it was. The caller validates the result (`config::parse_target`).

/// A value [`set_table_values`] writes.
#[derive(Debug, Clone, Copy)]
pub enum Value<'a> {
    /// A TOML basic string.
    Str(&'a str),
    /// An inline array of basic strings.
    Strings(&'a [String]),
}

impl Value<'_> {
    fn render(self) -> String {
        match self {
            Self::Str(s) => toml::Value::String(s.to_string()).to_string(),
            Self::Strings(items) => {
                toml::Value::Array(items.iter().cloned().map(toml::Value::String).collect())
                    .to_string()
            }
        }
    }
}

/// `text` with `table.key = "value"` set for every entry, values quoted as
/// TOML basic strings. Errors when the table is defined some other way
/// (inline table, dotted keys), which this editor does not rewrite.
pub fn set_table_keys(text: &str, table: &str, entries: &[(&str, &str)]) -> Result<String, String> {
    let entries: Vec<(&str, Value)> = entries.iter().map(|&(k, v)| (k, Value::Str(v))).collect();
    set_table_values(text, table, &entries)
}

/// [`set_table_keys`] for any [`Value`].
pub fn set_table_values(
    text: &str,
    table: &str,
    entries: &[(&str, Value)],
) -> Result<String, String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    if defines_table_otherwise(&lines, table) {
        return Err(format!(
            "[{table}] is not written as a [{table}] table header; edit it by hand"
        ));
    }
    let header = lines.iter().position(|l| is_header(l, table));
    let rendered = |key: &str, value: &Value| format!("{key} = {}", value.render());
    match header {
        None => {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("[{table}]"));
            for (key, value) in entries {
                lines.push(rendered(key, value));
            }
        }
        Some(header) => {
            for (key, value) in entries {
                let end = table_end(&lines, header);
                match (header + 1..end).find(|&i| line_key(&lines[i]) == Some(*key)) {
                    Some(i) => lines[i] = rendered(key, value),
                    None => {
                        let last = (header + 1..end)
                            .rev()
                            .find(|&i| line_key(&lines[i]).is_some())
                            .unwrap_or(header);
                        lines.insert(last + 1, rendered(key, value));
                    }
                }
            }
        }
    }
    let mut out = lines.join(newline);
    out.push_str(newline);
    Ok(out)
}

fn is_header(line: &str, table: &str) -> bool {
    let line = strip_comment(line).trim();
    line.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .is_some_and(|name| !name.starts_with('[') && name.trim() == table)
}

/// `[table.sub]` / `[[table.sub]]` anywhere, or `table = {...}` /
/// `table.key = ...` among the root keys (before the first header).
fn defines_table_otherwise(lines: &[String], table: &str) -> bool {
    let dotted = format!("{table}.");
    let mut at_root = true;
    for line in lines {
        let trimmed = strip_comment(line).trim();
        if let Some(inner) = trimmed.strip_prefix('[') {
            at_root = false;
            let inner = inner.trim_start_matches('[').trim_end_matches(']').trim();
            if inner.starts_with(&dotted) {
                return true;
            }
        } else if at_root
            && line_key(line).is_some_and(|key| key == table || key.starts_with(&dotted))
        {
            return true;
        }
    }
    false
}

/// Index of the first line after the table that starts at `header`.
fn table_end(lines: &[String], header: usize) -> usize {
    (header + 1..lines.len())
        .find(|&i| lines[i].trim_start().starts_with('['))
        .unwrap_or(lines.len())
}

/// The bare key of a `key = value` line.
fn line_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('[') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

fn strip_comment(line: &str) -> &str {
    line.split_once('#').map_or(line, |(before, _)| before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_keys_in_place_and_keeps_everything_else() {
        let text = "# repo\n[app]\nbase_url = \"http://localhost:3000\"\n\n[tracker]\nkind = \"jira\"\nticket_pattern = \"[A-Z]+-\\\\d+\" # keep\n\n[areas.shop]\npaths = [\"src/**\"]\n";
        let out =
            set_table_keys(text, "tracker", &[("kind", "jira"), ("project", "SHOP")]).unwrap();
        assert_eq!(
            out,
            "# repo\n[app]\nbase_url = \"http://localhost:3000\"\n\n[tracker]\nkind = \"jira\"\nticket_pattern = \"[A-Z]+-\\\\d+\" # keep\nproject = \"SHOP\"\n\n[areas.shop]\npaths = [\"src/**\"]\n"
        );
    }

    #[test]
    fn replaces_an_existing_value() {
        let text = "[code_host]\nkind = \"github\"\n";
        let out = set_table_keys(text, "code_host", &[("kind", "gitlab")]).unwrap();
        assert_eq!(out, "[code_host]\nkind = \"gitlab\"\n");
    }

    #[test]
    fn appends_a_missing_table_after_one_blank_line() {
        let text = "[app]\nbase_url = \"http://x\"\n\n\n";
        let out = set_table_keys(text, "code_host", &[("kind", "github")]).unwrap();
        assert_eq!(
            out,
            "[app]\nbase_url = \"http://x\"\n\n[code_host]\nkind = \"github\"\n"
        );
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let text = "[app]\r\nbase_url = \"http://x\"\r\n";
        let out = set_table_keys(text, "code_host", &[("kind", "github")]).unwrap();
        assert_eq!(
            out,
            "[app]\r\nbase_url = \"http://x\"\r\n\r\n[code_host]\r\nkind = \"github\"\r\n"
        );
    }

    #[test]
    fn a_header_in_a_comment_or_a_subtable_is_not_the_table() {
        let text = "# [tracker]\n[app]\nbase_url = \"http://x\"\n";
        let out = set_table_keys(text, "tracker", &[("kind", "jira")]).unwrap();
        assert!(out.ends_with("\n[tracker]\nkind = \"jira\"\n"), "{out}");
        assert!(
            set_table_keys("[tracker.extra]\nx = 1\n", "tracker", &[("kind", "jira")]).is_err()
        );
        assert!(
            set_table_keys(
                "tracker = { kind = \"jira\" }\n",
                "tracker",
                &[("kind", "jira")]
            )
            .is_err()
        );
        // A `tracker = ...` key inside another table is not the table.
        let inside = "[app]\ntracker = \"x\"\n";
        assert!(set_table_keys(inside, "tracker", &[("kind", "jira")]).is_ok());
    }

    #[test]
    fn writes_string_arrays_in_place_and_in_a_new_table() {
        let roles = ["admin".to_string(), "customer".to_string()];
        let text = "[testability]
# who logs in
roles = [\"x\"]
login_path = \"/a\"
";
        let out =
            set_table_values(text, "testability", &[("roles", Value::Strings(&roles))]).unwrap();
        assert_eq!(
            out,
            "[testability]
# who logs in
roles = [\"admin\", \"customer\"]
login_path = \"/a\"
"
        );
        let out = set_table_values(
            "[app]
",
            "testability",
            &[
                ("token_env", Value::Str("T")),
                ("roles", Value::Strings(&[])),
            ],
        )
        .unwrap();
        assert_eq!(
            out,
            "[app]

[testability]
token_env = \"T\"
roles = []
"
        );
    }
}
