//! The AI Summary pieces of the window: the "AI settings" window (provider,
//! base URL, model, language, key), the Markdown summary panel, and the
//! explain controls shared by the Tests detail panel and the Problems tab.

use super::widgets;
use super::{ExplainState, ExportState, GuardApp};
use crate::ai_settings::{KeyStatus, Provider, SummaryLang, key_status};
use crate::commands::ExportFormat;
use crate::credentials;
use crate::markdown::{self, Block, Span};
use crate::models::FetchKey;
use crate::open;
use crate::theme;
use eframe::egui::{
    self, Align2, CornerRadius, Frame, Margin, RichText, Stroke, TextFormat, Ui, text::LayoutJob,
    vec2,
};
use std::time::Instant;

/// Id of the password field, to drop its state once the key is saved.
const KEY_FIELD: &str = "ai-key-field";
/// The outer width of every settings field (combos and text fields).
const FIELD_WIDTH: f32 = 300.0;
/// A TextEdit's default horizontal margin (4 px a side): `desired_width` is
/// the inner width, so this is taken off to match the combos' outer width.
const TEXT_EDIT_MARGIN_X: f32 = 8.0;
/// The square refresh button next to the Model combo.
const REFRESH_SIZE: f32 = 24.0;
/// The Model combo's entry that reveals a text field for any id.
const OTHER_MODEL: &str = "Other\u{2026}";

impl GuardApp {
    /// The key source for the settings window and the hints.
    pub(super) fn key_source(&self) -> KeyStatus {
        let lookup = |name: &str| std::env::var(name).ok();
        // The harness never reads the real environment for what it shows.
        if self.shot.is_some() {
            return key_status(self.ai_key_saved, &self.ai, |_| None);
        }
        key_status(self.ai_key_saved, &self.ai, lookup)
    }

    /// "with Claude in Spanish": who answers and in which language.
    pub(super) fn explain_target(&self) -> String {
        let who = match self.ai.provider {
            Provider::Anthropic => "Claude".to_string(),
            Provider::Custom => self.ai.model.trim().to_string(),
            p => p.label().to_string(),
        };
        let lang = match self.ai.lang {
            SummaryLang::Es => "Spanish",
            SummaryLang::En => "English",
        };
        format!("{who}, in {lang}")
    }

    /// The explain controls for one key (a journey id or every failure):
    /// the start button, a spinner with Cancel, the summary or the error.
    pub(super) fn explain_controls(
        &mut self,
        ctx: &egui::Context,
        ui: &mut Ui,
        key: &str,
        start_label: &str,
        hint: &str,
    ) {
        let busy = self.explain_job.is_some();
        match self.explain.get(key).cloned() {
            None => {
                ui.horizontal(|ui| {
                    if widgets::flat_button(ui, start_label, !busy).clicked() {
                        self.start_explain(ctx, key.to_string());
                    }
                    if widgets::flat_button(ui, "AI settings...", true).clicked() {
                        self.ai_window = true;
                    }
                });
                ui.label(
                    RichText::new(format!("{hint} ({}).", self.explain_target()))
                        .color(theme::TEXT_DISABLED)
                        .size(12.0),
                );
            }
            Some(ExplainState::Running) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Analyzing...").color(theme::TEXT_SECONDARY));
                    if ui.button("Cancel").clicked() {
                        self.cancel_explain(key);
                    }
                });
            }
            Some(ExplainState::Done(text)) => {
                // The actions come first so a long summary never pushes them
                // out of view.
                ui.horizontal(|ui| {
                    if widgets::flat_button(ui, "Re-explain", !busy).clicked() {
                        self.start_explain(ctx, key.to_string());
                    }
                    if widgets::flat_button(ui, "AI settings...", true).clicked() {
                        self.ai_window = true;
                    }
                    ui.separator();
                    let exporting = self.export_job.is_some();
                    for (format, label, tip) in [
                        (
                            ExportFormat::Markdown,
                            "Export .md (re-runs)",
                            "guard explain --format md --out .guard/explain/explain-<stamp>.md",
                        ),
                        (
                            ExportFormat::Excel,
                            "Export Excel (re-runs)",
                            "guard explain --format xlsx --out .guard/explain/explain-<stamp>.xlsx",
                        ),
                    ] {
                        if widgets::flat_button(ui, label, !exporting)
                            .on_hover_text(format!("{tip} (re-runs the explanation: asks {} again, so the file can differ from what is on screen)", self.explain_target()))
                            .clicked()
                        {
                            self.start_export(ctx, key.to_string(), format);
                        }
                    }
                });
                self.export_status(ui, key);
                ui.add_space(4.0);
                summary_panel(ui, &text);
            }
            Some(ExplainState::Error(e)) => {
                let friendly = if e.contains("GUARD_EXPLAIN_API_KEY is not set") {
                    "No API key. Save one in AI settings, or set GUARD_EXPLAIN_API_KEY.".to_string()
                } else {
                    e
                };
                Frame::new()
                    .fill(theme::BROKEN.gamma_multiply(0.08))
                    .stroke(Stroke::new(1.0, theme::BROKEN.gamma_multiply(0.35)))
                    .corner_radius(CornerRadius::same(5))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add(
                            egui::Label::new(RichText::new(friendly).color(theme::AT_RISK)).wrap(),
                        );
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if widgets::flat_button(ui, "Try again", !busy).clicked() {
                        self.start_explain(ctx, key.to_string());
                    }
                    if widgets::flat_button(ui, "AI settings...", true).clicked() {
                        self.ai_window = true;
                    }
                });
            }
        }
    }

    /// How the last export of `key` went: a spinner, the written file with
    /// "Open", or the error inline.
    fn export_status(&mut self, ui: &mut Ui, key: &str) {
        let Some(state) = self.exports.get(key).cloned() else {
            return;
        };
        match state {
            ExportState::Running(format) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let what = match format {
                        ExportFormat::Markdown => ".md",
                        ExportFormat::Excel => "Excel",
                    };
                    ui.label(
                        RichText::new(format!("Exporting {what}...")).color(theme::TEXT_SECONDARY),
                    );
                });
            }
            ExportState::Done(path) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Exported:").color(theme::TEXT_SECONDARY));
                    ui.label(
                        RichText::new(path.display().to_string())
                            .monospace()
                            .size(12.0),
                    );
                    if widgets::flat_button(ui, "Open", true).clicked() {
                        // A path this window generated, not one from a report.
                        if let Err(e) = open::open_path(&path) {
                            self.exports.insert(
                                key.to_string(),
                                ExportState::Error(format!("{}: {e}", path.display())),
                            );
                        }
                    }
                });
            }
            ExportState::Error(e) => {
                let friendly = if e.contains("GUARD_EXPLAIN_API_KEY is not set") {
                    "No API key. Save one in AI settings, or set GUARD_EXPLAIN_API_KEY.".to_string()
                } else {
                    e
                };
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("Export failed: {friendly}"))
                            .color(theme::AT_RISK)
                            .size(12.0),
                    )
                    .wrap(),
                );
            }
        }
    }

    /// The "AI settings" window: provider preset, base URL, model, summary
    /// language and the API key (saved only in the Credential Manager).
    pub(super) fn ai_settings_window(&mut self, ctx: &egui::Context) {
        if !self.ai_window {
            return;
        }
        let mut open = true;
        egui::Window::new("AI settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .default_width(460.0)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(
                    RichText::new(
                        "Used by guard explain (AI Summary). Any Anthropic-compatible Messages API.",
                    )
                    .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(4.0);
                egui::Grid::new("ai-settings-grid")
                    .num_columns(2)
                    .spacing(vec2(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.label("Provider");
                        let mut chosen = None;
                        egui::ComboBox::from_id_salt("ai-provider")
                            .width(FIELD_WIDTH)
                            .selected_text(self.ai.provider.label())
                            .show_ui(ui, |ui| {
                                for p in Provider::ALL {
                                    if ui
                                        .selectable_label(self.ai.provider == p, p.label())
                                        .clicked()
                                    {
                                        chosen = Some(p);
                                    }
                                }
                            });
                        if let Some(p) = chosen {
                            self.ai.choose(p);
                            self.model_other = false;
                        }
                        ui.end_row();

                        ui.label("Base URL");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.ai.base_url)
                                .desired_width(FIELD_WIDTH - TEXT_EDIT_MARGIN_X),
                        );
                        ui.end_row();

                        self.model_rows(ctx, ui);

                        ui.label("Summary language");
                        egui::ComboBox::from_id_salt("ai-lang")
                            .width(FIELD_WIDTH)
                            .selected_text(self.ai.lang.label())
                            .show_ui(ui, |ui| {
                                for lang in SummaryLang::ALL {
                                    ui.selectable_value(&mut self.ai.lang, lang, lang.label());
                                }
                            });
                        ui.end_row();

                        ui.label("");
                        ui.checkbox(
                            &mut self.ai.auto_summarize,
                            "Summarize failures automatically after a run",
                        )
                        .on_hover_text(
                            "When a test run ends with failed or flaky journeys, start Explain \
                             failures and show it in Problems",
                        );
                        ui.end_row();
                    });
                ui.separator();
                // The key row below is for the host just chosen or typed.
                self.sync_key_saved();
                self.key_row(ctx, ui);
            });
        if !open {
            self.ai_window = false;
            self.ai_message = None;
            forget_key_input(ctx, &mut self.ai_key_input);
        }
    }

    /// The Model rows: a combo of the provider's models with a refresh
    /// button, the "Other" text field, and why the list is a fallback.
    /// Fetches in the background whenever the provider, base URL or saved
    /// key changed (debounced), never in the screenshot harness.
    fn model_rows(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        self.sync_key_saved();
        let key = FetchKey::of(&self.ai, self.ai_key_saved);
        if self.shot.is_none() {
            let now = Instant::now();
            if self.models.observe(&key, now) {
                self.start_model_fetch(ctx, key.clone());
            } else if let Some(wait) = self.models.wait(&key, now) {
                ctx.request_repaint_after(wait);
            }
        }
        let choices = self.models.choices(&key, self.ai.provider, &self.ai.model);
        let current = self.ai.model.trim().to_string();
        let selected = if self.model_other {
            OTHER_MODEL.to_string()
        } else {
            choices
                .options
                .iter()
                .find(|o| o.id == current)
                .map_or_else(|| "Choose a model".to_string(), |o| o.label.clone())
        };

        ui.label("Model");
        let mut picked: Option<Option<String>> = None;
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            egui::ComboBox::from_id_salt("ai-model")
                .width(FIELD_WIDTH - REFRESH_SIZE - gap)
                .selected_text(selected)
                .truncate()
                .show_ui(ui, |ui| {
                    for option in &choices.options {
                        let on = !self.model_other && option.id == current;
                        if ui.selectable_label(on, &option.label).clicked() {
                            picked = Some(Some(option.id.clone()));
                        }
                    }
                    if !choices.options.is_empty() {
                        ui.separator();
                    }
                    if ui.selectable_label(self.model_other, OTHER_MODEL).clicked() {
                        picked = Some(None);
                    }
                });
            if self.models.is_loading(&key) {
                ui.add_sized(vec2(REFRESH_SIZE, REFRESH_SIZE), egui::Spinner::new());
            } else {
                let refresh = ui
                    .add_sized(
                        vec2(REFRESH_SIZE, REFRESH_SIZE),
                        egui::Button::new("\u{21bb}"),
                    )
                    .on_hover_text("Fetch the provider's model list again");
                if refresh.clicked() {
                    self.models.refresh();
                    ctx.request_repaint();
                }
            }
        });
        ui.end_row();
        match picked {
            Some(Some(id)) => {
                self.ai.model = id;
                self.model_other = false;
            }
            Some(None) => self.model_other = true,
            None => {}
        }

        if self.model_other {
            ui.label("");
            ui.add(
                egui::TextEdit::singleline(&mut self.ai.model)
                    .hint_text("Any model id the provider accepts")
                    .desired_width(FIELD_WIDTH - TEXT_EDIT_MARGIN_X),
            );
            ui.end_row();
        }
        if let Some(note) = &choices.note {
            ui.label("");
            ui.scope(|ui| {
                ui.set_max_width(FIELD_WIDTH);
                ui.add(
                    egui::Label::new(RichText::new(note).color(theme::TEXT_SECONDARY).size(12.0))
                        .wrap(),
                );
            });
            ui.end_row();
        }
    }

    /// API key: status, a password field, Save and Clear.
    fn key_row(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let host = self.ai_key_host.clone();
        ui.label(RichText::new(format!("API key for {host}")).font(theme::semibold_font(13.0)));
        let (color, status) = match self.key_source() {
            KeyStatus::Saved => (theme::VERIFIED, format!("Key saved for {host}")),
            KeyStatus::FromEnv(name) => (theme::VERIFIED, format!("No key saved: using {name}")),
            KeyStatus::NotRequired => (theme::TEXT_SECONDARY, "No key (not required)".to_string()),
            KeyStatus::Missing => (theme::AT_RISK, format!("No key for {host}")),
        };
        ui.horizontal(|ui| {
            widgets::dot(ui.painter(), ui.cursor().min + vec2(5.0, 9.0), 4.0, color);
            ui.add_space(14.0);
            ui.label(RichText::new(status).color(color));
        });
        let field_id = egui::Id::new(KEY_FIELD);
        let mut save = false;
        ui.horizontal(|ui| {
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.ai_key_input)
                    .id(field_id)
                    .password(true)
                    .hint_text("Paste a key to save it")
                    .desired_width(270.0),
            );
            save |= field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let has_input = !self.ai_key_input.trim().is_empty();
            save |= ui
                .add_enabled(has_input, egui::Button::new("Save key"))
                .clicked();
            if ui
                .add_enabled(self.ai_key_saved, egui::Button::new("Clear"))
                .clicked()
            {
                self.ai_message = Some(match credentials::clear(&host) {
                    Ok(()) => {
                        self.ai_key_saved = false;
                        (true, "Key removed from Windows Credential Manager.".into())
                    }
                    Err(e) => (false, e),
                });
            }
        });
        if save && !self.ai_key_input.trim().is_empty() && self.shot.is_none() {
            self.ai_message = Some(match credentials::save(&host, self.ai_key_input.trim()) {
                Ok(()) => {
                    self.ai_key_saved = true;
                    (true, "Key saved in Windows Credential Manager.".into())
                }
                Err(e) => (false, e),
            });
            // Saved or not, the typed key goes, with its undo history.
            forget_key_input(ctx, &mut self.ai_key_input);
        }
        if let Some((ok, message)) = &self.ai_message {
            let color = if *ok { theme::VERIFIED } else { theme::BROKEN };
            ui.label(RichText::new(message).color(color).size(12.0));
        }
        ui.add_space(2.0);
        for line in [
            "Stored only in Windows Credential Manager, never in a file.",
            "One key per host, only ever sent to that host.",
            "Passed to guard through its environment, never its command line.",
            "With no saved key, api.anthropic.com uses GUARD_EXPLAIN_API_KEY,",
            "then ANTHROPIC_API_KEY.",
        ] {
            ui.label(RichText::new(line).color(theme::TEXT_SECONDARY).size(12.0));
        }
    }
}

/// Clears the typed key and drops the password field's egui state (cursor,
/// undo history), so no copy of the key outlives the text.
fn forget_key_input(ctx: &egui::Context, input: &mut String) {
    input.clear();
    ctx.data_mut(|d| d.remove::<egui::text_edit::TextEditState>(egui::Id::new(KEY_FIELD)));
}

/// A Markdown summary in a focus-tinted panel.
pub(super) fn summary_panel(ui: &mut Ui, text: &str) {
    Frame::new()
        .fill(theme::FOCUS.gamma_multiply(0.07))
        .stroke(Stroke::new(1.0, theme::FOCUS.gamma_multiply(0.30)))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            render_markdown(ui, text);
        });
    ui.add_space(4.0);
}

/// Headings, bullets, paragraphs and code from [`markdown::blocks`], with
/// inline `code` in monospace and **bold** in the semibold face.
pub(super) fn render_markdown(ui: &mut Ui, text: &str) {
    ui.spacing_mut().item_spacing.y = 4.0;
    let blocks = markdown::blocks(text);
    for (i, block) in blocks.iter().enumerate() {
        match block {
            Block::Heading(level, spans) => {
                if i > 0 {
                    ui.add_space(6.0);
                }
                let size = match level {
                    1 => 17.0,
                    2 => 15.0,
                    _ => 14.0,
                };
                let job = layout(spans, size, true, ui.available_width());
                ui.add(egui::Label::new(job).selectable(true));
            }
            Block::Bullet(spans) => {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_space(4.0);
                    ui.label(RichText::new("\u{2022}").color(theme::TEXT_SECONDARY));
                    let job = layout(spans, 13.0, false, ui.available_width());
                    ui.add(egui::Label::new(job).selectable(true));
                });
            }
            Block::Paragraph(spans) => {
                let job = layout(spans, 13.0, false, ui.available_width());
                ui.add(egui::Label::new(job).selectable(true));
            }
            Block::Code(code) => {
                Frame::new()
                    .fill(theme::ISLAND)
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::same(6))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add(
                            egui::Label::new(
                                RichText::new(code.as_str())
                                    .monospace()
                                    .size(12.0)
                                    .color(theme::TEXT),
                            )
                            .selectable(true),
                        );
                    });
            }
            Block::Table { header, rows } => render_table(ui, i, header, rows),
        }
    }
}

/// A pipe table as a striped grid: a narrow first column (the `#` of the
/// structured explain summary), the others sharing the width and wrapping.
fn render_table(ui: &mut Ui, index: usize, header: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) {
    let columns = header.len().max(1);
    let gap = 12.0;
    let total = (ui.available_width() - gap * (columns as f32 - 1.0)).max(0.0);
    let first = if columns > 1 { 28.0 } else { total };
    let rest = ((total - first) / (columns as f32 - 1.0).max(1.0)).max(80.0);
    let width = |column: usize| if column == 0 { first } else { rest };
    let cell = |ui: &mut Ui, spans: &[Span], column: usize, bold: bool| {
        let w = width(column);
        ui.allocate_ui(vec2(w, 0.0), |ui| {
            ui.set_width(w);
            let job = layout(spans, 13.0, bold, w);
            ui.add(egui::Label::new(job).selectable(true));
        });
    };
    egui::ScrollArea::horizontal()
        .id_salt(("md-table", index))
        .show(ui, |ui| {
            egui::Grid::new(("md-table", index))
                .striped(true)
                .spacing(vec2(gap, 6.0))
                .show(ui, |ui| {
                    for (column, spans) in header.iter().enumerate() {
                        cell(ui, spans, column, true);
                    }
                    ui.end_row();
                    for row in rows {
                        for (column, spans) in row.iter().enumerate() {
                            cell(ui, spans, column, false);
                        }
                        ui.end_row();
                    }
                });
        });
}

/// One wrapped [`LayoutJob`] for a line of spans.
fn layout(spans: &[Span], size: f32, heading: bool, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    for span in spans {
        let format = if span.code {
            TextFormat {
                font_id: egui::FontId::monospace(size - 1.0),
                color: theme::FOCUS,
                background: theme::ISLAND,
                ..Default::default()
            }
        } else if heading || span.bold {
            TextFormat {
                font_id: theme::semibold_font(size),
                color: theme::TEXT,
                ..Default::default()
            }
        } else {
            TextFormat {
                font_id: theme::font(size),
                color: theme::TEXT,
                ..Default::default()
            }
        };
        job.append(&span.text, 0.0, format);
    }
    job
}
