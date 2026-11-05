//! Self mode (target-toml.md "Self mode"): the developer starts the app and
//! `target.toml` keeps the detected commands as a commented block.

#[cfg(test)]
mod tests;

/// First header line of the commented block.
pub const SELF_MODE_HEADER: &str =
    "# guard does not start the app: you run it yourself (guard init --runner self).";
/// Second header line; the block's commands follow it.
pub const SELF_MODE_MARKER: &str =
    "# Detected commands, uncomment to let guard start it (needed in CI):";

/// The commented block after [`SELF_MODE_MARKER`] with its `# ` (or `#`)
/// removed, up to the first line that is not a comment.
fn block(target_toml: &str) -> Option<String> {
    let mut lines = target_toml.lines().map(|l| l.trim_end_matches('\r'));
    lines.by_ref().find(|l| l.trim_end() == SELF_MODE_MARKER)?;
    let mut out = String::new();
    for line in lines {
        let Some(rest) = line.strip_prefix('#') else {
            break;
        };
        out.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        out.push('\n');
    }
    Some(out)
}

/// The detected commands of the block: `[app].start` first, then each
/// service as `<name> (in <cwd>): <start>`. Empty when there is no block or
/// it does not parse.
fn commands(target_toml: &str) -> Vec<String> {
    let Some(table) = block(target_toml).and_then(|b| b.parse::<toml::Table>().ok()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(start) = table.get("start").and_then(|v| v.as_str()) {
        out.push(start.to_string());
    }
    let services = table.get("services").and_then(|v| v.as_array());
    for service in services.into_iter().flatten() {
        let field = |key: &str| service.get(key).and_then(|v| v.as_str());
        let (Some(name), Some(start)) = (field("name"), field("start")) else {
            continue;
        };
        match field("cwd").filter(|cwd| *cwd != "." && !cwd.is_empty()) {
            Some(cwd) => out.push(format!("{name} (in {cwd}): {start}")),
            None => out.push(format!("{name}: {start}")),
        }
    }
    out
}

/// The `hint:` lines (without the prefix) for a self-mode app that is not
/// ready (cli.md "Self-mode hint").
pub fn hint_lines(target_toml: &str) -> Vec<String> {
    let commands = commands(target_toml);
    if commands.is_empty() {
        return vec!["start your app, then run guard up again".into()];
    }
    let mut out = vec![
        "guard does not start the app (no [app].start in target.toml); start it yourself:".into(),
    ];
    out.extend(commands.into_iter().map(|c| format!("  {c}")));
    out.push("then run guard up again (or uncomment those lines so guard starts them)".into());
    out
}
