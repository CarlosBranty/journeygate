//! `guard explain [--journey <id>] [--report <path>] [--model <id>] [--lang <code>]
//! [--format table|md|json|xlsx] [--out <path>] [--json]`: summarises the last
//! run's failing and flaky journeys with Claude (or any Anthropic-compatible
//! Messages API), grouped by root cause into a table ([`render`]). `guard explain --list-models [--json]`
//! lists the models the configured provider offers ([`models`]).
//!
//! Only the failure fields of the Playwright report are ever sent to the API:
//! journey id, title, file, line, status and the error message, plus each
//! journey's top `guard suspects` (short sha, author, date, subject, matched
//! file paths, the blamed spec line). The API key,
//! storageState/.auth files and environment values are never read or sent, and
//! the key is never printed or logged.

use super::suspects::{self, JourneySuspects};
#[cfg(test)]
use super::suspects::{Blame, Suspect};
use crate::cli::{ExplainArgs, ExplainFormat};
use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::runtime::block_on;
use guard_core::verdict::{Classification, TestOutcome, classify, parse_report};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod models;
pub mod render;

use render::Explanation;

/// Env var holding the API key. guard's own variable comes first: a user-level
/// `ANTHROPIC_API_KEY` is also read by other tools (Claude Code switches to API
/// billing when it sees one), so guard must never require it.
pub const API_KEY_ENV: &str = "GUARD_EXPLAIN_API_KEY";
/// Fallback key variable, read only when [`API_KEY_ENV`] is unset or blank.
pub const FALLBACK_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
/// Env var setting the summary language (a code such as `es` or `en`).
const LANG_ENV: &str = "GUARD_EXPLAIN_LANG";
/// Summaries are written in Spanish unless configured otherwise.
pub const DEFAULT_LANG: &str = "es";
/// Env var overriding the model id.
const MODEL_ENV: &str = "GUARD_EXPLAIN_MODEL";
/// Env var overriding the API base URL (tests point this at a local stub).
const BASE_URL_ENV: &str = "GUARD_EXPLAIN_BASE_URL";
const DEFAULT_MODEL: &str = "claude-haiku-5-5";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Suspect commits per journey put into the prompt.
const SUSPECTS_PER_JOURNEY: usize = 3;
/// Terminal width for the table when `COLUMNS` is unset or invalid.
const DEFAULT_COLUMNS: usize = 160;

/// Instruction given to the model; asks for failures grouped by root cause as
/// one JSON object ([`render::Explanation`]).
const SYSTEM_PROMPT: &str = "You are an expert end-to-end test engineer. You are given the \
failing and flaky journeys of a Playwright run. Group the failures by ROOT CAUSE: failures that \
share one cause go in one row. Answer ONLY with one JSON object, no prose and no code fence, of \
this shape: {\"rows\":[{\"tests\":\"<short human label of the affected test(s), with the count \
when more than one, e.g. 'Checkout: pay with a gift card (2 tests)'>\",\"journeys\":[\"<journey \
id>\"],\"what_happened\":\"<the observed symptom, quoting the key error>\",\"probable_cause\":\"<a \
short verdict sentence, then the detail and where it fails>\",\"kind\":\"app_bug|suite_maintenance|\
test_data|environment|unknown\"}],\"verdict\":\"<one or two sentences: which rows point to a real \
application bug and which to suite maintenance, test data or the environment>\"}. When the \
possibly related changes support it, name the most likely related commit (short sha) and its \
author in probable_cause; when they do not, say so explicitly. Write every string value in the \
language the user asks for. Do not invent details the error text does not support.";

/// One failing or flaky journey: the only data sent to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failure {
    pub journey: Option<String>,
    pub title: String,
    pub file: String,
    pub line: u32,
    /// The Playwright project it ran in (`chromium`, `CL`).
    pub project: String,
    pub status: String,
    pub error: Option<String>,
}

/// Sends a prompt to a model and returns the Markdown summary. The HTTP client
/// lives behind this trait so tests can inject a fake and never hit the network.
pub trait Explainer {
    fn explain(&self, system: &str, prompt: &str) -> Result<String, String>;
}

pub fn run(ctx: &Context, args: &ExplainArgs) -> CliResult<u8> {
    // Listing models needs no report: only the endpoint and the key.
    if args.list_models {
        return models::run(
            &base_url(),
            resolve_api_key(env_lookup),
            args.effective_format() == ExplainFormat::Json,
        );
    }
    // A workbook is binary: it never goes to the terminal.
    if args.effective_format() == ExplainFormat::Xlsx && args.out.is_none() {
        return Err(CliError::usage("--format xlsx needs --out <PATH>")
            .with_hint("for example: guard explain --format xlsx --out explain.xlsx"));
    }
    let report_path = args
        .report
        .clone()
        .map(absolutize)
        .unwrap_or_else(|| ctx.e2e_dir.join("results").join("report.json"));
    let text = std::fs::read_to_string(&report_path).map_err(|e| {
        CliError::env(format!("cannot read {}: {e}", ctx.display(&report_path)))
            .with_hint("run guard test first")
    })?;
    let report = parse_report(&text)
        .map_err(|e| CliError::env(format!("{}: {e}", ctx.display(&report_path))))?;
    let failures = select_failures(&classify(&report), args.journey.as_deref());
    let lang = resolve_lang(args.lang.as_deref(), env_lookup);
    let model = args
        .model
        .clone()
        .or_else(|| {
            std::env::var(MODEL_ENV)
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    // Nothing failed: say so and exit without calling (or needing) the API.
    if failures.is_empty() {
        output(
            args,
            &model,
            &lang,
            &all_green_message(args.journey.as_deref()),
            &[],
        )?;
        return Ok(EXIT_OK);
    }

    let api_key = resolve_api_key(env_lookup).ok_or_else(|| {
        CliError::env(format!("{API_KEY_ENV} is not set")).with_hint(format!(
            "set {API_KEY_ENV} to your API key to use guard explain \
             ({FALLBACK_API_KEY_ENV} is read as a fallback)"
        ))
    })?;
    let explainer = AnthropicExplainer::new(&base_url(), &api_key, &model);
    // Best effort: no git, no catalog or no base only means fewer hints.
    let suspects = suspects::collect(
        ctx,
        &report_path,
        args.journey.as_deref(),
        SUSPECTS_PER_JOURNEY,
    )
    .map(|report| report.journeys)
    .unwrap_or_default();
    explain_with(args, &explainer, &model, &lang, &failures, &suspects)
}

/// The process environment as a lookup for the pure resolvers below.
fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// [`BASE_URL_ENV`], else [`DEFAULT_BASE_URL`]; blank counts as unset.
fn base_url() -> String {
    std::env::var(BASE_URL_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string())
}

/// The API key, trimmed: [`API_KEY_ENV`], else [`FALLBACK_API_KEY_ENV`];
/// blank values count as unset.
pub fn resolve_api_key(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    [API_KEY_ENV, FALLBACK_API_KEY_ENV]
        .into_iter()
        .filter_map(&lookup)
        .map(|key| key.trim().to_string())
        .find(|key| !key.is_empty())
}

/// `text` with every occurrence of the key replaced, for anything echoed
/// back from an API (an error body may quote the key it rejected).
pub(crate) fn redact(text: &str, key: Option<&str>) -> String {
    match key.map(str::trim).filter(|k| !k.is_empty()) {
        Some(key) => text.replace(key, "[redacted]"),
        None => text.to_string(),
    }
}

/// The summary language code: `--lang`, else `GUARD_EXPLAIN_LANG`, else
/// [`DEFAULT_LANG`]. Trimmed and lowercased; blank counts as unset.
pub fn resolve_lang(flag: Option<&str>, lookup: impl Fn(&str) -> Option<String>) -> String {
    flag.map(str::to_string)
        .into_iter()
        .chain(lookup(LANG_ENV))
        .map(|code| code.trim().to_ascii_lowercase())
        .find(|code| !code.is_empty())
        .unwrap_or_else(|| DEFAULT_LANG.to_string())
}

/// The language a code names in the prompt.
pub fn language_name(code: &str) -> String {
    match code {
        "es" => "Spanish".to_string(),
        "en" => "English".to_string(),
        other => format!("the language with code {other}"),
    }
}

/// Runs one explanation against any [`Explainer`] and prints it.
fn explain_with(
    args: &ExplainArgs,
    explainer: &dyn Explainer,
    model: &str,
    lang: &str,
    failures: &[Failure],
    suspects: &[JourneySuspects],
) -> CliResult<u8> {
    let summary = explainer
        .explain(SYSTEM_PROMPT, &build_prompt(failures, suspects, lang))
        .map_err(|e| CliError::env(format!("Anthropic API: {e}")))?;
    output(args, model, lang, &summary, failures)?;
    Ok(EXIT_OK)
}

/// The failing and flaky journeys, optionally filtered to one journey id.
pub fn select_failures(tests: &[TestOutcome], journey: Option<&str>) -> Vec<Failure> {
    tests
        .iter()
        .filter(|t| matches!(t.status, Classification::Failed | Classification::Flaky))
        .filter(|t| journey.is_none_or(|j| t.journey.as_deref() == Some(j)))
        .map(|t| Failure {
            journey: t.journey.clone(),
            title: t.title.clone(),
            file: t.file.clone(),
            line: t.line,
            project: t.project.clone(),
            status: match t.status {
                Classification::Flaky => "flaky",
                _ => "failed",
            }
            .to_string(),
            error: t.error.clone(),
        })
        .collect()
}

/// The user prompt: one block per failure with only the report's failure
/// fields, then the language the whole summary must be written in.
pub fn build_prompt(failures: &[Failure], suspects: &[JourneySuspects], lang: &str) -> String {
    let mut out = String::from("Here are the failing and flaky journeys from the last run.\n\n");
    for (i, f) in failures.iter().enumerate() {
        out.push_str(&format!("### Failure {}\n", i + 1));
        if let Some(journey) = &f.journey {
            out.push_str(&format!("- journey: {journey}\n"));
        }
        out.push_str(&format!("- title: {}\n", f.title));
        out.push_str(&format!("- status: {}\n", f.status));
        out.push_str(&format!("- location: {}:{}\n", f.file, f.line));
        match &f.error {
            Some(error) => out.push_str(&format!("- error: {error}\n")),
            None => out.push_str("- error: (none recorded)\n"),
        }
        out.push('\n');
    }
    let has_changes = suspects
        .iter()
        .any(|j| !j.commits.is_empty() || !j.uncommitted.is_empty() || !j.blame.is_empty());
    if has_changes {
        out.push_str("### Possibly related changes\n");
        for j in suspects {
            out.push_str(&suspects_block(j));
        }
        out.push('\n');
    }
    out.push_str(
        "Group the journeys by root cause: for each group give what happened and the likely root \
         cause and where it fails, then one overall verdict.",
    );
    if has_changes {
        out.push_str(
            " For each group, name the most likely related commit (short sha) and its author \
             when the changes above support it; when they do not, say so explicitly instead of \
             guessing.",
        );
    }
    out.push_str(&format!(
        "\n\nWrite the whole summary in {}, every JSON string value included. Keep journey ids, file paths, \
         code identifiers and error text verbatim, exactly as given above: never translate them.",
        language_name(lang)
    ));
    out
}

/// One journey's suspects as prompt lines.
fn suspects_block(j: &JourneySuspects) -> String {
    let mut out = format!(
        "- journey: {}\n",
        j.journey.as_deref().unwrap_or("(no journey)")
    );
    for c in &j.commits {
        out.push_str(&format!(
            "  - commit {} by {} ({}){}{}: {} | files: {}\n",
            c.short,
            c.author,
            c.date.get(..10).unwrap_or(&c.date),
            c.repo
                .as_deref()
                .map(|r| format!(" in {r}"))
                .unwrap_or_default(),
            if c.recent {
                ", recent, not on this branch"
            } else {
                ""
            },
            c.subject,
            c.files.join(", ")
        ));
    }
    if !j.uncommitted.is_empty() {
        out.push_str(&format!(
            "  - uncommitted changes: {}\n",
            j.uncommitted.join(", ")
        ));
    }
    for b in &j.blame {
        let who = if b.uncommitted {
            "not committed yet".to_string()
        } else {
            format!(
                "last changed by {} in {}: {}",
                b.author.as_deref().unwrap_or("-"),
                b.short.as_deref().unwrap_or("-"),
                b.subject.as_deref().unwrap_or("")
            )
        };
        out.push_str(&format!("  - failing line {}:{} {who}\n", b.file, b.line));
    }
    if j.commits.is_empty() && j.uncommitted.is_empty() {
        out.push_str("  - no commit touching its paths was found\n");
    }
    out
}

/// Pulls the concatenated text blocks out of a Messages API response body.
pub fn parse_response(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid API response: {e}"))?;
    let blocks = value
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or("API response has no content array")?;
    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) == Some("text")
            && let Some(t) = block.get("text").and_then(|t| t.as_str())
        {
            text.push_str(t);
        }
    }
    let text = text.trim().to_string();
    if text.is_empty() {
        Err("API response had no text content".into())
    } else {
        Ok(text)
    }
}

/// Surfaces an API error body as a single line, falling back to the raw body.
fn api_error(status: u16, body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    match parsed["error"]["message"].as_str() {
        Some(message) => format!("HTTP {status}: {message}"),
        None => format!(
            "HTTP {status}: {}",
            body.chars().take(500).collect::<String>()
        ),
    }
}

fn all_green_message(journey: Option<&str>) -> String {
    match journey {
        Some(j) => format!("Journey {j} has no failing or flaky result to explain."),
        None => "All green: no failing or flaky journeys to explain.".to_string(),
    }
}

/// Renders the summary in the chosen format and prints it, or writes it to
/// `--out` and says so on stderr. A structured reply becomes a table; free
/// text keeps the plain Markdown summary.
fn output(
    args: &ExplainArgs,
    model: &str,
    lang: &str,
    summary: &str,
    failures: &[Failure],
) -> CliResult<()> {
    let (structured, summary) = match render::interpret(summary) {
        render::Reply::Structured(e) => (Some(e), summary.to_string()),
        render::Reply::Text(text) => (None, text),
    };
    let summary = summary.as_str();
    let format = args.effective_format();
    if format == ExplainFormat::Xlsx {
        let path = args
            .out
            .as_deref()
            .expect("xlsx needs --out (checked in run)");
        let explanation = structured.unwrap_or_else(|| Explanation {
            rows: Vec::new(),
            verdict: summary.to_string(),
        });
        render::write_xlsx(path, &explanation, lang, model, &render::today())
            .map_err(|e| CliError::env(format!("cannot write {}: {e}", path.display())))?;
        eprintln!("wrote {}", path.display());
        return Ok(());
    }
    let text = match (format, &structured) {
        (ExplainFormat::Json, Some(e)) => {
            let markdown = render::render_md(e, lang, model, &render::today());
            format!(
                "{}\n",
                render::render_json(model, &markdown, Some(e), failures)
            )
        }
        (ExplainFormat::Json, None) => {
            format!("{}\n", render::render_json(model, summary, None, failures))
        }
        (ExplainFormat::Md, Some(e)) => render::render_md(e, lang, model, &render::today()),
        (_, Some(e)) => format!(
            "{}\n{}",
            explained_header(model, failures),
            render::render_table(e, lang, terminal_width())
        ),
        (_, None) => render_markdown(model, summary, failures),
    };
    match &args.out {
        Some(path) => write_out(path, &text),
        None => {
            print!("{text}");
            Ok(())
        }
    }
}

/// Writes rendered text to `--out` and reports it on stderr.
fn write_out(path: &Path, text: &str) -> CliResult<()> {
    std::fs::write(path, text)
        .map_err(|e| CliError::env(format!("cannot write {}: {e}", path.display())))?;
    eprintln!("wrote {}", path.display());
    Ok(())
}

/// `COLUMNS`, else [`DEFAULT_COLUMNS`].
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&w| w > 0)
        .unwrap_or(DEFAULT_COLUMNS)
}

/// The one-line header above an explanation.
fn explained_header(model: &str, failures: &[Failure]) -> String {
    format!(
        "Explained {} failing/flaky journey(s) with {model}.\n",
        failures.len()
    )
}

/// The free-text output: a one-line header then the model's Markdown.
pub fn render_markdown(model: &str, summary: &str, failures: &[Failure]) -> String {
    if failures.is_empty() {
        return format!("{summary}\n");
    }
    format!("{}\n{summary}\n", explained_header(model, failures))
}

fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

/// The real client: `POST {base}/v1/messages` with the key as `x-api-key`.
pub struct AnthropicExplainer {
    base: String,
    api_key: String,
    model: String,
}

impl AnthropicExplainer {
    pub fn new(base: &str, api_key: &str, model: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
        }
    }

    /// The async request, separate from [`Explainer::explain`] so tests can
    /// await it directly without nesting tokio runtimes.
    async fn request(&self, system: &str, prompt: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system,
            "messages": [{"role": "user", "content": prompt}],
        });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("guard-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        let response = client
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| e.to_string())?;
        if !(200..300).contains(&status) {
            return Err(redact(&api_error(status, &text), Some(&self.api_key)));
        }
        parse_response(&text)
    }
}

impl Explainer for AnthropicExplainer {
    fn explain(&self, system: &str, prompt: &str) -> Result<String, String> {
        block_on(self.request(system, prompt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn outcome(journey: &str, status: Classification, error: Option<&str>) -> TestOutcome {
        TestOutcome {
            title: format!("{journey} title"),
            file: format!("tests/{journey}.spec.ts"),
            line: 10,
            project: "chromium".into(),
            tags: vec![format!("@journey:{journey}")],
            journey: Some(journey.into()),
            status,
            blocking: status == Classification::Failed,
            attempts: 1,
            duration_ms: 1000,
            error: error.map(str::to_string),
        }
    }

    fn tests() -> Vec<TestOutcome> {
        vec![
            outcome("a.passed", Classification::Passed, None),
            outcome(
                "b.failed",
                Classification::Failed,
                Some("boom at order-total"),
            ),
            outcome("c.flaky", Classification::Flaky, Some("timeout waiting")),
            outcome("d.skipped", Classification::Skipped, None),
        ]
    }

    #[test]
    fn select_keeps_only_failing_and_flaky() {
        let failures = select_failures(&tests(), None);
        let ids: Vec<_> = failures.iter().filter_map(|f| f.journey.clone()).collect();
        assert_eq!(ids, ["b.failed", "c.flaky"]);
        assert_eq!(failures[0].status, "failed");
        assert_eq!(failures[1].status, "flaky");
    }

    #[test]
    fn select_filters_to_one_journey() {
        let failures = select_failures(&tests(), Some("c.flaky"));
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].journey.as_deref(), Some("c.flaky"));
        // A passing journey filtered on its own selects nothing.
        assert!(select_failures(&tests(), Some("a.passed")).is_empty());
    }

    /// An env lookup over a fixed list, so key resolution never reads the
    /// real process environment.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn api_key_prefers_the_guard_variable() {
        let lookup = env(&[
            ("GUARD_EXPLAIN_API_KEY", "guard-key"),
            ("ANTHROPIC_API_KEY", "anthropic-key"),
        ]);
        assert_eq!(resolve_api_key(lookup).as_deref(), Some("guard-key"));
    }

    #[test]
    fn api_key_falls_back_to_anthropic_and_skips_blank_values() {
        let fallback = env(&[("ANTHROPIC_API_KEY", "anthropic-key")]);
        assert_eq!(resolve_api_key(fallback).as_deref(), Some("anthropic-key"));
        let blank = env(&[
            ("GUARD_EXPLAIN_API_KEY", "  "),
            ("ANTHROPIC_API_KEY", "anthropic-key"),
        ]);
        assert_eq!(resolve_api_key(blank).as_deref(), Some("anthropic-key"));
        assert_eq!(resolve_api_key(env(&[])), None);
        assert_eq!(resolve_api_key(env(&[("ANTHROPIC_API_KEY", "")])), None);
    }

    #[test]
    fn api_key_is_returned_trimmed() {
        // A key pasted with its line ending must not reach the header as is.
        let lookup = env(&[(
            "GUARD_EXPLAIN_API_KEY",
            "k
",
        )]);
        assert_eq!(resolve_api_key(lookup).as_deref(), Some("k"));
        let padded = env(&[("ANTHROPIC_API_KEY", "  sk-ant  ")]);
        assert_eq!(resolve_api_key(padded).as_deref(), Some("sk-ant"));
    }

    #[test]
    fn system_prompt_names_no_english_heading() {
        // The closing section is named by meaning, so a Spanish summary gets
        // a Spanish heading instead of a hardcoded `## Summary`.
        assert!(!SYSTEM_PROMPT.contains("## Summary"), "{SYSTEM_PROMPT}");
        assert!(
            SYSTEM_PROMPT.contains("in the language the user asks for"),
            "{SYSTEM_PROMPT}"
        );
    }

    #[test]
    fn redact_hides_the_key_in_error_text() {
        assert_eq!(
            redact("bad sk-1 and sk-1", Some("sk-1")),
            "bad [redacted] and [redacted]"
        );
        assert_eq!(redact("text", Some("  ")), "text");
        assert_eq!(redact("text", None), "text");
    }

    #[test]
    fn lang_defaults_to_spanish_and_the_flag_beats_the_env() {
        assert_eq!(resolve_lang(None, env(&[])), "es");
        assert_eq!(
            resolve_lang(None, env(&[("GUARD_EXPLAIN_LANG", "en")])),
            "en"
        );
        assert_eq!(
            resolve_lang(Some("pt"), env(&[("GUARD_EXPLAIN_LANG", "en")])),
            "pt"
        );
        assert_eq!(
            resolve_lang(Some(" "), env(&[("GUARD_EXPLAIN_LANG", " EN ")])),
            "en"
        );
    }

    #[test]
    fn language_names_cover_spanish_english_and_any_other_code() {
        assert_eq!(language_name("es"), "Spanish");
        assert_eq!(language_name("en"), "English");
        assert_eq!(language_name("fr"), "the language with code fr");
    }

    #[test]
    fn prompt_asks_for_spanish_by_default() {
        let prompt = build_prompt(&select_failures(&tests(), None), &[], DEFAULT_LANG);
        assert!(
            prompt.contains("Write the whole summary in Spanish"),
            "{prompt}"
        );
        // Identifiers and error text stay as they are.
        assert!(prompt.contains("verbatim"), "{prompt}");
        assert!(prompt.contains("journey ids"), "{prompt}");
    }

    #[test]
    fn prompt_asks_for_english_or_any_other_code() {
        let failures = select_failures(&tests(), None);
        let english = build_prompt(&failures, &[], "en");
        assert!(
            english.contains("Write the whole summary in English"),
            "{english}"
        );
        assert!(!english.contains("Spanish"), "{english}");
        let other = build_prompt(&failures, &[], "pt-BR");
        assert!(other.contains("in the language with code pt-BR"), "{other}");
    }

    #[test]
    fn prompt_carries_journey_title_location_and_error() {
        let prompt = build_prompt(&select_failures(&tests(), Some("b.failed")), &[], "en");
        assert!(prompt.contains("journey: b.failed"), "{prompt}");
        assert!(prompt.contains("status: failed"), "{prompt}");
        assert!(prompt.contains("tests/b.failed.spec.ts:10"), "{prompt}");
        assert!(prompt.contains("boom at order-total"), "{prompt}");
    }

    fn suspects() -> Vec<JourneySuspects> {
        vec![JourneySuspects {
            journey: Some("b.failed".into()),
            title: "b".into(),
            status: "failed".into(),
            tests: 1,
            commits: vec![Suspect {
                repo: Some("orders-api".into()),
                sha: "a1b2c3d4e5".into(),
                short: "a1b2c3d".into(),
                author: "Jane Doe".into(),
                date: "2026-10-21T10:00:00Z".into(),
                timestamp: 1,
                subject: "feat: order totals".into(),
                files: vec!["orders-api/Orders/Total.cs".into()],
                recent: false,
            }],
            uncommitted: vec!["web/checkout.ts".into()],
            blame: vec![Blame {
                file: "e2e/tests/b.spec.ts".into(),
                line: 12,
                sha: Some("ffff".into()),
                short: Some("fffffff".into()),
                author: Some("Quinn".into()),
                date: Some("2026-10-01T00:00:00Z".into()),
                timestamp: Some(1),
                subject: Some("test: assert total".into()),
                content: "expect(total).toBe(10)".into(),
                uncommitted: false,
            }],
        }]
    }

    #[test]
    fn prompt_carries_each_journeys_top_suspects() {
        let prompt = build_prompt(&select_failures(&tests(), None), &suspects(), "es");
        assert!(prompt.contains("a1b2c3d"), "{prompt}");
        assert!(prompt.contains("Jane Doe"), "{prompt}");
        assert!(prompt.contains("feat: order totals"), "{prompt}");
        assert!(prompt.contains("orders-api/Orders/Total.cs"), "{prompt}");
        assert!(prompt.contains("web/checkout.ts"), "{prompt}");
        assert!(prompt.contains("e2e/tests/b.spec.ts:12"), "{prompt}");
        assert!(prompt.contains("Quinn"), "{prompt}");
        // The model names the commit only when the evidence supports it.
        assert!(prompt.contains("most likely related commit"), "{prompt}");
        assert!(prompt.contains("say so explicitly"), "{prompt}");
        assert!(
            prompt.contains("Write the whole summary in Spanish"),
            "{prompt}"
        );
    }

    #[test]
    fn prompt_without_suspects_asks_for_no_commit() {
        let prompt = build_prompt(&select_failures(&tests(), None), &[], "en");
        assert!(!prompt.contains("most likely related commit"), "{prompt}");
        assert!(!prompt.contains("Possibly related changes"), "{prompt}");
    }

    #[test]
    fn parse_response_concatenates_text_blocks() {
        let body = r#"{"content":[{"type":"text","text":"Root "},{"type":"thinking","thinking":"x"},{"type":"text","text":"cause"}]}"#;
        assert_eq!(parse_response(body).unwrap(), "Root cause");
    }

    #[test]
    fn parse_response_errors_on_empty_or_malformed() {
        assert!(parse_response("not json").is_err());
        assert!(parse_response(r#"{"content":[]}"#).is_err());
        assert!(parse_response(r#"{"stop_reason":"end_turn"}"#).is_err());
    }

    #[test]
    fn api_error_surfaces_the_message() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(api_error(401, body), "HTTP 401: invalid x-api-key");
        assert!(api_error(500, "gateway down").contains("gateway down"));
    }

    #[test]
    fn json_output_has_model_summary_and_journeys() {
        let failures = select_failures(&tests(), Some("b.failed"));
        let json = render::render_json("claude-haiku-5-5", "analysis text", None, &failures);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["model"], "claude-haiku-5-5");
        assert_eq!(value["summary"], "analysis text");
        assert_eq!(value["journeys"][0]["journey"], "b.failed");
        assert_eq!(value["journeys"][0]["status"], "failed");
    }

    /// A fake [`Explainer`] proves the report -> prompt -> render pipeline works
    /// without any network, and records the prompt it was handed.
    struct FakeExplainer {
        reply: String,
        seen_prompt: RefCell<Option<String>>,
    }

    impl Explainer for FakeExplainer {
        fn explain(&self, _system: &str, prompt: &str) -> Result<String, String> {
            *self.seen_prompt.borrow_mut() = Some(prompt.to_string());
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn fake_explainer_feeds_the_renderer() {
        let failures = select_failures(&tests(), None);
        let fake = FakeExplainer {
            reply: "## Summary\nAll clear.".into(),
            seen_prompt: RefCell::new(None),
        };
        let summary = fake
            .explain(SYSTEM_PROMPT, &build_prompt(&failures, &[], "en"))
            .unwrap();
        assert_eq!(summary, "## Summary\nAll clear.");
        let markdown = render_markdown("m", &summary, &failures);
        assert!(markdown.contains("Explained 2 failing/flaky journey(s) with m"));
        assert!(markdown.contains("All clear."));
        assert!(
            fake.seen_prompt
                .borrow()
                .as_ref()
                .unwrap()
                .contains("boom at order-total")
        );
    }

    #[tokio::test]
    async fn posts_to_messages_with_the_key_and_parses_the_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(body_partial_json(serde_json::json!({
                "model": "claude-haiku-5-5",
                "messages": [{"role": "user"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": [{"type": "text", "text": "Root cause: total mismatch."}],
                "model": "claude-haiku-5-5",
                "stop_reason": "end_turn"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let explainer = AnthropicExplainer::new(&server.uri(), "test-key", "claude-haiku-5-5");
        let out = explainer.request(SYSTEM_PROMPT, "prompt").await.unwrap();
        assert_eq!(out, "Root cause: total mismatch.");
    }

    #[tokio::test]
    async fn non_2xx_bodies_are_surfaced() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "type": "error",
                "error": {"type": "invalid_request_error", "message": "max_tokens too large"}
            })))
            .mount(&server)
            .await;
        let explainer = AnthropicExplainer::new(&server.uri(), "sk-test-400", "claude-haiku-5-5");
        let error = explainer
            .request(SYSTEM_PROMPT, "prompt")
            .await
            .unwrap_err();
        assert_eq!(error, "HTTP 400: max_tokens too large");
    }

    #[tokio::test]
    async fn an_error_body_quoting_the_key_is_redacted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "invalid key sk-quoted-key"}
            })))
            .mount(&server)
            .await;
        let explainer = AnthropicExplainer::new(&server.uri(), "sk-quoted-key", "m");
        let error = explainer.request(SYSTEM_PROMPT, "p").await.unwrap_err();
        assert_eq!(error, "HTTP 401: invalid key [redacted]");
    }
}
