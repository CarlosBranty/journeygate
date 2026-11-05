//! The structured `guard explain` reply (failures grouped by root cause) and
//! its renderers: a box-drawing terminal table, a GitHub Markdown table, the
//! JSON the GUI parses and an Excel workbook.

use super::Failure;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The model's structured answer: one row per root cause, then a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Explanation {
    /// Required key; `null` reads as no rows.
    #[serde(deserialize_with = "or_default")]
    pub rows: Vec<Row>,
    #[serde(default, deserialize_with = "or_default")]
    pub verdict: String,
}

/// One root cause and the tests it explains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    #[serde(default, deserialize_with = "or_default")]
    pub tests: String,
    #[serde(default, deserialize_with = "or_default")]
    pub journeys: Vec<String>,
    #[serde(default, deserialize_with = "or_default")]
    pub what_happened: String,
    #[serde(default, deserialize_with = "or_default")]
    pub probable_cause: String,
    #[serde(default = "unknown_kind", deserialize_with = "known_kind")]
    pub kind: String,
}

const KINDS: [&str; 5] = [
    "app_bug",
    "suite_maintenance",
    "test_data",
    "environment",
    "unknown",
];

/// A `null` value as the type's default, so one `null` the model writes does
/// not throw the whole structured reply away.
fn or_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn unknown_kind() -> String {
    "unknown".to_string()
}

/// Any kind outside [`KINDS`] (or not a string) becomes `unknown`.
fn known_kind<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(value
        .as_str()
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| KINDS.contains(&k.as_str()))
        .unwrap_or_else(unknown_kind))
}

/// What the model's reply is shown as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// At least one root-cause row: rendered as a table.
    Structured(Explanation),
    /// Free text (or an object without rows): the plain Markdown summary.
    Text(String),
}

/// Reads the model's reply. An object without rows shows its verdict rather
/// than the raw JSON; with no verdict either, the reply is kept as is.
pub fn interpret(text: &str) -> Reply {
    match parse_structured(text) {
        Some(e) if !e.rows.is_empty() => Reply::Structured(e),
        Some(e) if !e.verdict.trim().is_empty() => Reply::Text(e.verdict.trim().to_string()),
        _ => Reply::Text(text.to_string()),
    }
}

/// The structured reply in `text` (rows possibly empty), or `None` when the
/// model answered free text. Accepts a ```` ```json ```` fence and prose
/// around the object.
pub fn parse_structured(text: &str) -> Option<Explanation> {
    let trimmed = text.trim();
    let candidates = [
        strip_fence(trimmed),
        trimmed
            .find('{')
            .zip(trimmed.rfind('}'))
            .filter(|(start, end)| start < end)
            .map(|(start, end)| &trimmed[start..=end]),
    ];
    candidates
        .into_iter()
        .flatten()
        .find_map(|json| serde_json::from_str::<Explanation>(json).ok())
}

/// The body of a fenced block that is the whole text, if it is one.
fn strip_fence(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("```")?;
    let body = rest.split_once('\n').map(|(_, body)| body)?;
    Some(body.trim_end().strip_suffix("```").unwrap_or(body).trim())
}

/// Column headers and labels in the summary language: Spanish for `es`,
/// English otherwise.
pub struct Labels {
    pub number: &'static str,
    pub test: &'static str,
    pub what_happened: &'static str,
    pub probable_cause: &'static str,
    pub kind: &'static str,
    pub verdict: &'static str,
    pub summary_sheet: &'static str,
    pub model: &'static str,
    pub date: &'static str,
}

pub fn labels(lang: &str) -> Labels {
    if lang == "es" {
        Labels {
            number: "#",
            test: "Test",
            what_happened: "Qué pasó",
            probable_cause: "Causa probable",
            kind: "Tipo",
            verdict: "Veredicto",
            summary_sheet: "Resumen",
            model: "Modelo",
            date: "Fecha",
        }
    } else {
        Labels {
            number: "#",
            test: "Test",
            what_happened: "What happened",
            probable_cause: "Probable cause",
            kind: "Kind",
            verdict: "Verdict",
            summary_sheet: "Resumen",
            model: "Model",
            date: "Date",
        }
    }
}

/// A kind's display label in the summary language.
pub fn kind_label(kind: &str, lang: &str) -> &'static str {
    let es = lang == "es";
    match (kind, es) {
        ("app_bug", true) => "Bug de la app",
        ("app_bug", false) => "App bug",
        ("suite_maintenance", true) => "Mantenimiento del suite",
        ("suite_maintenance", false) => "Suite maintenance",
        ("test_data", true) => "Datos de prueba",
        ("test_data", false) => "Test data",
        ("environment", true) => "Ambiente",
        ("environment", false) => "Environment",
        (_, true) => "Sin determinar",
        (_, false) => "Unknown",
    }
}

/// `text` without Markdown bold markers, for the table and the workbook.
fn plain(text: &str) -> String {
    text.replace("**", "")
}

/// The terminal table: `#`, test, what happened and probable cause, wrapped
/// to `width` columns, then a blank line and the verdict.
pub fn render_table(explanation: &Explanation, lang: &str, width: usize) -> String {
    let l = labels(lang);
    let header = [l.number, l.test, l.what_happened, l.probable_cause].map(str::to_string);
    let rows: Vec<[String; 4]> = explanation
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            [
                (i + 1).to_string(),
                plain(&r.tests),
                plain(&r.what_happened),
                plain(&r.probable_cause),
            ]
        })
        .collect();
    let widths = column_widths(&header, &rows, width);
    let mut out = String::new();
    out.push_str(&border(&widths, '┌', '┬', '┐'));
    push_row(&mut out, &header, &widths);
    for row in &rows {
        out.push_str(&border(&widths, '├', '┼', '┤'));
        push_row(&mut out, row, &widths);
    }
    out.push_str(&border(&widths, '└', '┴', '┘'));
    if !explanation.verdict.trim().is_empty() {
        out.push_str(&format!(
            "\n{}: {}\n",
            l.verdict,
            plain(explanation.verdict.trim())
        ));
    }
    out
}

/// Narrowest a wrapped text column gets, whatever the terminal width.
const MIN_COLUMN: usize = 8;

/// Each column's content width. `#` keeps its natural width; the text
/// columns share what is left of `total` (borders and padding taken out) by
/// water-filling: a column that fits in its fair share keeps its natural
/// width, the rest split the remainder.
fn column_widths(header: &[String; 4], rows: &[[String; 4]], total: usize) -> [usize; 4] {
    let natural: Vec<usize> = (0..4)
        .map(|c| {
            std::iter::once(&header[c])
                .chain(rows.iter().map(|r| &r[c]))
                .flat_map(|cell| cell.lines())
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    // 5 vertical bars plus one space of padding on each side of 4 columns.
    let chrome = 5 + 8 + natural[0];
    let mut available = total.saturating_sub(chrome).max(3 * MIN_COLUMN);
    let mut widths = [natural[0], 0, 0, 0];
    let mut open: Vec<usize> = vec![1, 2, 3];
    loop {
        let share = available / open.len();
        let fitting: Vec<usize> = open
            .iter()
            .copied()
            .filter(|&c| natural[c] <= share)
            .collect();
        if fitting.is_empty() {
            let last = *open.last().expect("open columns");
            for &c in &open {
                widths[c] = share.max(MIN_COLUMN);
            }
            widths[last] += available - share * open.len();
            break;
        }
        for c in fitting {
            widths[c] = natural[c];
            available -= natural[c];
            open.retain(|&o| o != c);
        }
        if open.is_empty() {
            break;
        }
    }
    widths
}

fn border(widths: &[usize; 4], left: char, middle: char, right: char) -> String {
    let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
    format!("{left}{}{right}\n", parts.join(&middle.to_string()))
}

fn push_row(out: &mut String, cells: &[String; 4], widths: &[usize; 4]) {
    let wrapped: Vec<Vec<String>> = cells
        .iter()
        .zip(widths)
        .map(|(cell, &w)| wrap(cell, w))
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    for line in 0..height {
        out.push('│');
        for (cell, &w) in wrapped.iter().zip(widths) {
            let text = cell.get(line).map(String::as_str).unwrap_or("");
            let pad = w.saturating_sub(text.chars().count());
            out.push_str(&format!(" {text}{} │", " ".repeat(pad)));
        }
        out.push('\n');
    }
}

/// Word-wraps `text` to `width` characters; words longer than `width` are
/// split, and explicit newlines are kept.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let mut word: Vec<char> = word.chars().collect();
            loop {
                let used = line.chars().count();
                let gap = usize::from(used > 0);
                if used + gap + word.len() <= width {
                    if gap == 1 {
                        line.push(' ');
                    }
                    line.extend(word.iter());
                    break;
                }
                if used > 0 {
                    lines.push(std::mem::take(&mut line));
                    continue;
                }
                let rest = word.split_off(width);
                lines.push(word.into_iter().collect());
                word = rest;
            }
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// One Markdown table cell: pipes escaped, newlines as `<br>`.
pub fn md_cell(text: &str) -> String {
    text.trim()
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

/// The probable cause with its leading verdict sentence in bold, unless the
/// model already bolded something.
fn bold_lead(cause: &str) -> String {
    let cause = cause.trim();
    if cause.contains("**") || cause.is_empty() {
        return cause.to_string();
    }
    match cause.find(". ") {
        Some(end) => format!("**{}** {}", &cause[..=end], cause[end + 2..].trim()),
        None => format!("**{cause}**"),
    }
}

/// The Markdown export: a title line, the five-column table, the verdict and
/// a footnote with the journey ids of each row.
pub fn render_md(explanation: &Explanation, lang: &str, model: &str, date: &str) -> String {
    let l = labels(lang);
    let mut out = format!("# guard explain — {date} · {model}\n\n");
    out.push_str(&format!(
        "| {} | {} | {} | {} | {} |\n| --- | --- | --- | --- | --- |\n",
        l.number, l.test, l.what_happened, l.probable_cause, l.kind
    ));
    for (i, r) in explanation.rows.iter().enumerate() {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            i + 1,
            md_cell(&r.tests),
            md_cell(&r.what_happened),
            md_cell(&bold_lead(&r.probable_cause)),
            kind_label(&r.kind, lang)
        ));
    }
    if !explanation.verdict.trim().is_empty() {
        out.push_str(&format!(
            "\n**{}:** {}\n",
            l.verdict,
            explanation.verdict.trim()
        ));
    }
    out.push_str("\nJourneys:\n\n");
    for (i, r) in explanation.rows.iter().enumerate() {
        let ids = if r.journeys.is_empty() {
            "-".to_string()
        } else {
            r.journeys
                .iter()
                .map(|j| format!("`{j}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!("{}. {ids}\n", i + 1));
    }
    out
}

/// The JSON the GUI parses, one object on one line. `summary` stays the
/// Markdown string older consumers render; `rows` is empty and `verdict`
/// null when the model answered free text.
pub fn render_json(
    model: &str,
    summary: &str,
    explanation: Option<&Explanation>,
    failures: &[Failure],
) -> String {
    let doc = serde_json::json!({
        "model": model,
        "rows": explanation.map(|e| e.rows.as_slice()).unwrap_or_default(),
        "verdict": explanation.map(|e| e.verdict.as_str()),
        "summary": summary,
        "journeys": failures,
    });
    serde_json::to_string(&doc).expect("explain json serialises")
}

/// Writes the workbook: an `Explain` sheet (bold frozen header with an
/// autofilter, wrapped cells) and a summary sheet with model, date and
/// verdict.
pub fn write_xlsx(
    path: &Path,
    explanation: &Explanation,
    lang: &str,
    model: &str,
    date: &str,
) -> Result<(), String> {
    use rust_xlsxwriter::{Format, FormatAlign, Workbook};
    let l = labels(lang);
    let header_format = Format::new().set_bold().set_align(FormatAlign::Top);
    let wrap_format = Format::new().set_text_wrap().set_align(FormatAlign::Top);
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name("Explain").map_err(|e| e.to_string())?;
    let headers = [
        l.number,
        l.test,
        l.what_happened,
        l.probable_cause,
        l.kind,
        "Journeys",
    ];
    let widths = [5.0, 40.0, 60.0, 70.0, 22.0, 35.0];
    for (c, (title, width)) in headers.iter().zip(widths).enumerate() {
        let c = c as u16;
        sheet
            .write_string_with_format(0, c, *title, &header_format)
            .map_err(|e| e.to_string())?;
        sheet
            .set_column_width(c, width)
            .map_err(|e| e.to_string())?;
    }
    for (i, r) in explanation.rows.iter().enumerate() {
        let row = (i + 1) as u32;
        sheet
            .write_number_with_format(row, 0, (i + 1) as f64, &wrap_format)
            .map_err(|e| e.to_string())?;
        let cells = [
            plain(&r.tests),
            plain(&r.what_happened),
            plain(&r.probable_cause),
            kind_label(&r.kind, lang).to_string(),
            r.journeys.join("\n"),
        ];
        for (c, text) in cells.iter().enumerate() {
            sheet
                .write_string_with_format(row, (c + 1) as u16, text, &wrap_format)
                .map_err(|e| e.to_string())?;
        }
    }
    sheet.set_freeze_panes(1, 0).map_err(|e| e.to_string())?;
    let last_row = explanation.rows.len() as u32;
    sheet
        .autofilter(0, 0, last_row, (headers.len() - 1) as u16)
        .map_err(|e| e.to_string())?;

    let summary = workbook.add_worksheet();
    summary
        .set_name(l.summary_sheet)
        .map_err(|e| e.to_string())?;
    summary
        .set_column_width(0, 14.0)
        .map_err(|e| e.to_string())?;
    summary
        .set_column_width(1, 100.0)
        .map_err(|e| e.to_string())?;
    for (row, (key, value)) in [
        (l.model, model),
        (l.date, date),
        (l.verdict, explanation.verdict.trim()),
    ]
    .into_iter()
    .enumerate()
    {
        let row = row as u32;
        summary
            .write_string_with_format(row, 0, key, &header_format)
            .map_err(|e| e.to_string())?;
        summary
            .write_string_with_format(row, 1, plain(value), &wrap_format)
            .map_err(|e| e.to_string())?;
    }
    workbook.save(path).map_err(|e| e.to_string())
}

/// Today's UTC date as `YYYY-MM-DD`, without a date library.
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil_date((secs / 86_400) as i64)
}

/// Days since 1970-01-01 as a proleptic Gregorian `YYYY-MM-DD` (Howard
/// Hinnant's `civil_from_days`).
fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests;
