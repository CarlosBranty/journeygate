//! A minimal Markdown reader for the AI Summary: headings, bullets,
//! paragraphs, fenced code, GitHub pipe tables (the structured `guard
//! explain` summary), and inline `code` / **bold** spans. Anything else is
//! shown as plain text. The egui layer only maps these blocks to styles.

#[cfg(test)]
mod tests;

/// A run of inline text with one style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub code: bool,
    pub bold: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// `#` .. `######`, with its level.
    Heading(u8, Vec<Span>),
    /// A `- ` or `* ` item.
    Bullet(Vec<Span>),
    /// Consecutive text lines, joined with spaces.
    Paragraph(Vec<Span>),
    /// A fenced block, verbatim.
    Code(String),
    /// A pipe table: `\|` is a literal pipe and `<br>` a line break inside a
    /// cell. Every row has as many cells as the header.
    Table {
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

/// Splits `md` into blocks.
pub fn blocks(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut fence: Option<Vec<String>> = None;
    let mut table: Vec<String> = Vec::new();
    let flush = |paragraph: &mut Vec<String>, out: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            out.push(Block::Paragraph(spans(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    for raw in md.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if fence.is_none() && trimmed.starts_with('|') {
            table.push(trimmed.to_string());
            continue;
        }
        flush_table(&mut table, &mut paragraph, &mut out, flush);
        if let Some(code) = &mut fence {
            if trimmed.starts_with("```") {
                out.push(Block::Code(code.join("\n")));
                fence = None;
            } else {
                code.push(line.to_string());
            }
            continue;
        }
        if trimmed.starts_with("```") {
            flush(&mut paragraph, &mut out);
            fence = Some(Vec::new());
        } else if trimmed.is_empty() {
            flush(&mut paragraph, &mut out);
        } else if let Some((level, text)) = heading(trimmed) {
            flush(&mut paragraph, &mut out);
            out.push(Block::Heading(level, spans(text)));
        } else if let Some(text) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            flush(&mut paragraph, &mut out);
            out.push(Block::Bullet(spans(text.trim())));
        } else if numbered(trimmed) {
            flush(&mut paragraph, &mut out);
            out.push(Block::Paragraph(spans(trimmed)));
        } else {
            paragraph.push(trimmed.to_string());
        }
    }
    flush_table(&mut table, &mut paragraph, &mut out, flush);
    // An unclosed fence still shows its lines.
    if let Some(code) = fence {
        out.push(Block::Code(code.join("\n")));
    }
    flush(&mut paragraph, &mut out);
    out
}

/// Pending `|` lines: a table when the second is a `| --- |` separator,
/// otherwise ordinary paragraph text.
fn flush_table(
    table: &mut Vec<String>,
    paragraph: &mut Vec<String>,
    out: &mut Vec<Block>,
    flush: impl Fn(&mut Vec<String>, &mut Vec<Block>),
) {
    if table.is_empty() {
        return;
    }
    let lines = std::mem::take(table);
    let is_table = lines.len() >= 2 && is_separator(&lines[1]);
    if !is_table {
        paragraph.extend(lines);
        return;
    }
    flush(paragraph, out);
    let header: Vec<Vec<Span>> = cells(&lines[0]).iter().map(|c| cell_spans(c)).collect();
    let rows = lines[2..]
        .iter()
        .map(|line| {
            let mut row: Vec<Vec<Span>> = cells(line).iter().map(|c| cell_spans(c)).collect();
            row.resize(header.len(), Vec::new());
            row
        })
        .collect();
    out.push(Block::Table { header, rows });
}

/// `| --- | :-: |`: every cell dashes with optional alignment colons.
fn is_separator(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim_matches(':');
            !c.is_empty() && c.bytes().all(|b| b == b'-')
        })
}

/// The trimmed cells of a `| a | b |` line; `\|` stays inside its cell.
fn cells(line: &str) -> Vec<String> {
    let inner = line.trim();
    let inner = inner.strip_prefix('|').unwrap_or(inner);
    let inner = inner
        .strip_suffix('|')
        .filter(|i| !i.ends_with('\\'))
        .unwrap_or(inner);
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => out.push(std::mem::take(&mut cell).trim().to_string()),
            _ => cell.push(c),
        }
    }
    out.push(cell.trim().to_string());
    out
}

/// A cell's spans, `<br>` as a line break.
fn cell_spans(cell: &str) -> Vec<Span> {
    spans(&cell.replace("<br>", "\n"))
}

/// `## text` as `(2, "text")`; a `#` run must be followed by a space.
fn heading(line: &str) -> Option<(u8, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let text = line[level..].strip_prefix(' ')?;
    Some((level as u8, text.trim().trim_end_matches('#').trim_end()))
}

/// `1. text`: kept on its own line instead of joining a paragraph.
fn numbered(line: &str) -> bool {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(". ")
}

/// Splits a line into plain, `code` and **bold** spans. Stars inside code are
/// literal; an unclosed backtick or `**` is plain text.
pub fn spans(line: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut push = |text: &str, code: bool, bold: bool| {
        if text.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(last) if last.code == code && last.bold == bold => last.text.push_str(text),
            _ => out.push(Span {
                text: text.to_string(),
                code,
                bold,
            }),
        }
    };
    let mut rest = line;
    let mut bold = false;
    while !rest.is_empty() {
        let tick = rest.find('`');
        let stars = rest.find("**");
        match (tick, stars) {
            (Some(t), s) if s.is_none_or(|s| t < s) => {
                match rest[t + 1..].find('`') {
                    Some(len) => {
                        push(&rest[..t], false, bold);
                        push(&rest[t + 1..t + 1 + len], true, false);
                        rest = &rest[t + 2 + len..];
                    }
                    None => {
                        // Unclosed: the rest is text (bold may still close).
                        push(&rest[..t + 1], false, bold);
                        rest = &rest[t + 1..];
                    }
                }
            }
            (_, Some(s)) => {
                let closes = bold || rest[s + 2..].contains("**");
                push(&rest[..s], false, bold);
                if closes {
                    bold = !bold;
                } else {
                    push("**", false, bold);
                }
                rest = &rest[s + 2..];
            }
            _ => {
                push(rest, false, bold);
                rest = "";
            }
        }
    }
    out
}
