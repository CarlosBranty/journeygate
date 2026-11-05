//! The Tests view: journeys grouped by area as a Rider-like tree, with
//! selection checkboxes, status icons and a details panel on the right.

use super::shell::{island, island_panel_frame};
use super::widgets::{self, Icon};
use super::{GuardApp, ProbeKind};
use crate::catalog::{self, TreeItem};
use crate::commands::GuardCommand;
use crate::open;
use crate::results;
use crate::theme::{self, TestState};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2,
    pos2, vec2,
};

const ROW_HEIGHT: f32 = 26.0;

impl GuardApp {
    pub(super) fn tests_screen(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("details")
            .resizable(true)
            .default_width(400.0)
            .width_range(280.0..=720.0)
            .show_separator_line(false)
            .frame(island_panel_frame())
            .show(ctx, |ui| {
                island(ui, Margin::same(14), |ui| self.details(ctx, ui));
            });
        egui::CentralPanel::default()
            .frame(island_panel_frame())
            .show(ctx, |ui| {
                island(ui, Margin::symmetric(10, 8), |ui| {
                    self.tests_header(ctx, ui);
                    ui.add_space(4.0);
                    if let Some(error) = &self.catalog_error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(error).color(theme::BROKEN));
                        return;
                    }
                    self.summary_bar(ui);
                    ui.add_space(4.0);
                    self.tree(ctx, ui);
                });
            });
    }

    fn tests_header(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            widgets::section_title(ui, "Journeys");
            ui.add_space(10.0);
            let idle = !self.running();
            let count = self.selected.len();
            if widgets::icon_button(
                ui,
                Icon::Play,
                theme::RUN_GREEN_HOVER,
                idle && count > 0,
                "Run the checked journeys: guard test --journey <id>...",
            )
            .clicked()
                && let Some(cmd) =
                    GuardCommand::test_journeys(self.selected.iter().cloned(), self.run_mode)
            {
                self.request_test(ctx, cmd);
            }
            let probing = self.probe.is_some();
            if widgets::icon_button(
                ui,
                Icon::Target,
                theme::FOCUS,
                !probing,
                "Preview affected: guard affected --json (runs nothing)",
            )
            .clicked()
            {
                self.start_probe(ctx, ProbeKind::Affected);
            }
            if widgets::icon_button(
                ui,
                Icon::Refresh,
                theme::TEXT,
                true,
                "Reload catalog and results",
            )
            .clicked()
            {
                self.reload_catalog();
                self.reload_results();
            }
            ui.add_space(6.0);
            if widgets::flat_button(ui, "Check visible", true).clicked() {
                let visible: Vec<String> = self
                    .rows
                    .iter()
                    .filter(|r| r.matches(&self.search, None) && r.safety.runnable())
                    .map(|r| r.id.clone())
                    .collect();
                self.selected.extend(visible);
            }
            if widgets::flat_button(ui, "Clear", !self.selected.is_empty()).clicked() {
                self.selected.clear();
            }
            if count > 0 {
                ui.add_space(4.0);
                widgets::chip(ui, &format!("{count} checked"), theme::ACCENT);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let collapse_all = !self.collapsed.is_empty();
                let label = if collapse_all {
                    "Expand all"
                } else {
                    "Collapse all"
                };
                if widgets::flat_button(ui, label, true).clicked() {
                    if collapse_all {
                        self.collapsed.clear();
                    } else {
                        self.collapsed = catalog::areas(&self.rows).into_iter().collect();
                    }
                }
                search_field(ui, &mut self.search);
            });
        });
        if self
            .probe
            .as_ref()
            .is_some_and(|p| p.kind == ProbeKind::Affected)
        {
            // Not ui.spinner(): it repaints every frame while shown.
            ui.label(
                RichText::new("Checking affected journeys...")
                    .color(theme::TEXT_SECONDARY)
                    .size(12.0),
            );
        } else {
            match &self.affected {
                Some(Ok(a)) => {
                    ui.label(
                        RichText::new(format!(
                            "Affected vs {}: {} journeys from {} changed files",
                            a.base,
                            a.ids.len(),
                            a.changed_files
                        ))
                        .color(theme::FOCUS)
                        .size(12.0),
                    );
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e).color(theme::BROKEN).size(12.0));
                }
                None => {}
            }
        }
    }

    fn summary_bar(&self, ui: &mut Ui) {
        let mut counts = [0usize; 7];
        let order = [
            TestState::Passed,
            TestState::Failed,
            TestState::Flaky,
            TestState::Affected,
            TestState::Quarantined,
            TestState::Skipped,
            TestState::NotRun,
        ];
        for row in &self.rows {
            let state = self.state_of(row);
            if let Some(i) = order.iter().position(|s| *s == state) {
                counts[i] += 1;
            }
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.label(
                RichText::new(format!("{} journeys", self.rows.len()))
                    .color(theme::TEXT_SECONDARY)
                    .size(12.0),
            );
            for (state, n) in order.iter().zip(counts) {
                if n == 0 {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                    widgets::paint_state(ui.painter(), r.center(), 5.0, *state);
                    ui.label(
                        RichText::new(format!("{n} {}", state.label()))
                            .color(theme::TEXT_SECONDARY)
                            .size(12.0),
                    );
                });
            }
            if let Some(note) = &self.results_note {
                ui.label(RichText::new(note).color(theme::TEXT_DISABLED).size(12.0));
            }
        });
        // Thin progress line: the share of each state, like Rider's test bar.
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 3.0), Sense::hover());
        let total = self.rows.len().max(1) as f32;
        let mut x = rect.left();
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(1), theme::BORDER);
        for (state, n) in order.iter().zip(counts) {
            if n == 0 || matches!(state, TestState::NotRun) {
                continue;
            }
            let w = rect.width() * n as f32 / total;
            p.rect_filled(
                egui::Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height())),
                CornerRadius::ZERO,
                state.color(),
            );
            x += w;
        }
    }

    fn tree(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let items = catalog::tree_items(&self.rows, &self.search, &self.collapsed);
        if items.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("No journey matches.").color(theme::TEXT_SECONDARY));
            return;
        }
        let affected: Vec<String> = self.affected_ids().to_vec();
        let mut focus = None;
        let mut toggle_area = None;
        let mut toggle_check: Option<(String, bool)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, ROW_HEIGHT, items.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for item in &items[range] {
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), ROW_HEIGHT),
                        Sense::click(),
                    );
                    match item {
                        TreeItem::Area { name, count } => {
                            if self.area_row(ui, rect, &response, name, *count) {
                                toggle_area = Some(name.clone());
                            }
                        }
                        TreeItem::Journey(index) => {
                            let row = &self.rows[*index];
                            let (clicked, check) = self.journey_row(
                                ui,
                                rect,
                                &response,
                                *index,
                                affected.contains(&row.id),
                            );
                            if let Some(checked) = check {
                                toggle_check = Some((row.id.clone(), checked));
                            } else if clicked {
                                focus = Some(row.id.clone());
                            }
                        }
                    }
                }
            });
        if let Some(area) = toggle_area
            && !self.collapsed.remove(&area)
        {
            self.collapsed.insert(area);
        }
        if let Some((id, checked)) = toggle_check {
            if checked {
                self.selected.insert(id);
            } else {
                self.selected.remove(&id);
            }
        }
        if let Some(id) = focus {
            self.set_focus(ctx, Some(id));
        }
    }

    /// Paints an area row; true when it was clicked (toggle).
    fn area_row(
        &self,
        ui: &mut Ui,
        rect: egui::Rect,
        response: &egui::Response,
        name: &str,
        count: usize,
    ) -> bool {
        let p = ui.painter_at(rect);
        if response.hovered() {
            p.rect_filled(rect, CornerRadius::same(4), theme::HOVER);
        }
        let open = !self.search.trim().is_empty() || !self.collapsed.contains(name);
        let chevron = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let y = rect.center().y;
        widgets::paint_icon(
            &p,
            egui::Rect::from_center_size(pos2(rect.left() + 12.0, y), Vec2::splat(12.0)),
            chevron,
            theme::TEXT_SECONDARY,
        );
        let states: Vec<TestState> = self
            .rows
            .iter()
            .filter(|r| r.area == name)
            .map(|r| self.state_of(r))
            .collect();
        let failed = states.iter().filter(|s| **s == TestState::Failed).count();
        widgets::paint_state(
            &p,
            pos2(rect.left() + 30.0, y),
            6.0,
            theme::area_state(states),
        );
        let w = widgets::text_at(
            &p,
            pos2(rect.left() + 44.0, y),
            name,
            theme::semibold_font(13.0),
            theme::TEXT,
        );
        let mut x = rect.left() + 52.0 + w;
        x += widgets::text_at(
            &p,
            pos2(x, y),
            &format!(
                "{count} {}",
                if count == 1 { "journey" } else { "journeys" }
            ),
            theme::font(12.0),
            theme::TEXT_SECONDARY,
        );
        if failed > 0 {
            widgets::text_at(
                &p,
                pos2(x + 10.0, y),
                &format!("{failed} failed"),
                theme::font(12.0),
                theme::BROKEN,
            );
        }
        response.clicked()
    }

    /// Paints a journey row; returns (row clicked, checkbox toggled to).
    fn journey_row(
        &self,
        ui: &mut Ui,
        rect: egui::Rect,
        response: &egui::Response,
        index: usize,
        affected: bool,
    ) -> (bool, Option<bool>) {
        let row = &self.rows[index];
        let focused = self.focused.as_deref() == Some(row.id.as_str());
        {
            let p = ui.painter_at(rect);
            if focused {
                p.rect_filled(rect, CornerRadius::same(4), theme::SELECTION);
            } else if response.hovered() {
                p.rect_filled(rect, CornerRadius::same(4), theme::HOVER);
            }
        }
        let y = rect.center().y;
        let checked = self.selected.contains(&row.id);
        let runnable = row.safety.runnable();
        let cb_rect = egui::Rect::from_center_size(pos2(rect.left() + 34.0, y), Vec2::splat(14.0));
        let cb = widgets::tree_checkbox(ui, cb_rect, response.id.with("check"), checked, runnable);
        let cb = if runnable {
            cb
        } else {
            cb.on_hover_text("Dangerous: not runnable from the GUI")
        };
        let check = cb.clicked().then_some(!checked);

        let p = ui.painter_at(rect);
        let state = self.state_of(row);
        widgets::paint_state(&p, pos2(rect.left() + 56.0, y), 6.0, state);
        let id_color = if affected { theme::FOCUS } else { theme::TEXT };
        let mut x = rect.left() + 70.0;
        x += widgets::text_at(&p, pos2(x, y), &row.id, theme::font(13.0), id_color);

        // Right side: chips and the duration, right-aligned.
        let result = self.results.get(&row.id);
        let duration = match result {
            Some(r) => Some((
                results::format_duration(r.duration_ms),
                theme::TEXT_SECONDARY,
            )),
            None => self
                .durations
                .as_ref()
                .and_then(|h| results::history_ms(h, &[row.conventional_spec_file()]))
                .map(|ms| {
                    (
                        format!("~{}", results::format_duration(ms)),
                        theme::TEXT_DISABLED,
                    )
                }),
        };
        let mut right = rect.right() - 10.0;
        if let Some((text, color)) = duration {
            let r = p.text(
                pos2(right, y),
                egui::Align2::RIGHT_CENTER,
                text,
                theme::font(12.0),
                color,
            );
            right = r.left() - 10.0;
        }
        let mut tags: Vec<(&str, Color32)> = Vec::new();
        if row.quarantined {
            tags.push(("quarantined", theme::CONFLICT));
        }
        if row.smoke {
            tags.push(("smoke", theme::TEXT_SECONDARY));
        }
        if affected && result.is_some() {
            tags.push(("affected", theme::FOCUS));
        }
        for (text, color) in tags {
            let galley = p.layout_no_wrap(text.to_string(), theme::font(11.0), color);
            let chip = egui::Rect::from_min_max(
                pos2(right - galley.size().x - 10.0, y - 8.0),
                pos2(right, y + 8.0),
            );
            p.rect_filled(chip, CornerRadius::same(4), color.gamma_multiply(0.15));
            p.galley(
                pos2(chip.left() + 5.0, y - galley.size().y / 2.0),
                galley,
                color,
            );
            right = chip.left() - 6.0;
        }
        // The title, clipped before the right-side items.
        let title_rect =
            egui::Rect::from_min_max(pos2(x + 10.0, rect.top()), pos2(right, rect.bottom()));
        if title_rect.width() > 20.0 {
            let p = ui.painter_at(title_rect);
            widgets::text_at(
                &p,
                pos2(title_rect.left(), y),
                &row.title,
                theme::font(12.5),
                theme::TEXT_SECONDARY,
            );
        }
        (response.clicked(), check)
    }

    fn details(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let Some(journey) = self.focused.as_deref().and_then(|id| self.row(id)).cloned() else {
            ui.add_space(ui.available_height() * 0.35);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Select a journey to see its details.")
                        .color(theme::TEXT_SECONDARY),
                );
            });
            return;
        };
        let state = self.state_of(&journey);
        let result = self.results.get(&journey.id).cloned();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    widgets::paint_state(ui.painter(), r.center(), 8.0, state);
                    widgets::chip(ui, state.label(), state.color());
                });
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(&journey.title)
                            .font(theme::semibold_font(16.0))
                            .color(theme::TEXT),
                    )
                    .wrap(),
                );
                ui.label(
                    RichText::new(&journey.id)
                        .monospace()
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(10.0);
                egui::Grid::new("journey-facts")
                    .num_columns(2)
                    .spacing(vec2(16.0, 6.0))
                    .show(ui, |ui| {
                        let key = |ui: &mut Ui, text: &str| {
                            ui.label(RichText::new(text).color(theme::TEXT_SECONDARY));
                        };
                        key(ui, "Area");
                        ui.label(&journey.area);
                        ui.end_row();
                        key(ui, "Smoke");
                        ui.label(if journey.smoke { "yes" } else { "no" });
                        ui.end_row();
                        key(ui, "Quarantined");
                        ui.label(if journey.quarantined { "yes" } else { "no" });
                        ui.end_row();
                        key(ui, "Safety");
                        ui.label(RichText::new(journey.safety.label()).color(theme::AT_RISK));
                        ui.end_row();
                        if let Some(r) = &result {
                            key(ui, "Last result");
                            ui.label(format!(
                                "{} in {} test(s)",
                                results::format_duration(r.duration_ms),
                                r.tests
                            ));
                            ui.end_row();
                        }
                    });
                ui.add_space(12.0);
                widgets::section_title(ui, "Paths");
                ui.add_space(2.0);
                code_block(ui, |ui| {
                    if journey.paths.is_empty() {
                        ui.label(
                            RichText::new("none (smoke, --all or --journey only)")
                                .color(theme::TEXT_SECONDARY),
                        );
                    }
                    for path in &journey.paths {
                        ui.label(RichText::new(path).monospace());
                    }
                });
                let Some(result) = result else {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("No result in the last report.").color(theme::TEXT_SECONDARY),
                    );
                    return;
                };
                if let Some(error) = &result.error {
                    ui.add_space(12.0);
                    widgets::section_title(ui, "Last error");
                    ui.add_space(2.0);
                    Frame::new()
                        .fill(theme::BROKEN.gamma_multiply(0.08))
                        .stroke(Stroke::new(1.0, theme::BROKEN.gamma_multiply(0.35)))
                        .corner_radius(CornerRadius::same(5))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(error.as_str())
                                        .monospace()
                                        .color(Color32::from_rgb(0xf2, 0xa5, 0xa7)),
                                )
                                .wrap()
                                .selectable(true),
                            );
                        });
                }
                // Possibly related changes (`guard suspects`), then the AI
                // Summary (`guard explain --json`).
                if matches!(
                    result.outcome,
                    results::Outcome::Failed | results::Outcome::Flaky
                ) {
                    ui.add_space(12.0);
                    widgets::section_title(ui, "Possibly related changes");
                    ui.add_space(2.0);
                    self.suspects_rows(ui, &journey.id, false);
                    self.explain_section(ctx, ui, &journey.id);
                }
                if result.attachments.is_empty() {
                    return;
                }
                ui.add_space(12.0);
                widgets::section_title(ui, "Attachments");
                let Some(repo) = self.repo().map(std::path::Path::to_path_buf) else {
                    return;
                };
                let e2e = catalog::e2e_dir(&repo);
                for attachment in &result.attachments {
                    let path = attachment.resolved(&e2e);
                    let exists = path.is_file();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(exists, egui::Button::new("Open"))
                            .on_hover_text(path.display().to_string())
                            .clicked()
                            && let Err(e) = open::checked(&e2e, &path).and_then(|p| {
                                open::open_path(&p).map_err(|e| open::OpenError::Io(e.to_string()))
                            })
                        {
                            self.notice = Some(format!("Cannot open {}: {e}", path.display()));
                        }
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        ui.label(format!("{} ({name})", attachment.name));
                        if !exists {
                            ui.label(RichText::new("missing").color(theme::TEXT_DISABLED));
                        }
                    });
                    if exists && attachment.content_type == "image/png" {
                        let uri = format!("file://{}", path.display());
                        if !self.shown_images.contains(&uri) {
                            self.shown_images.push(uri.clone());
                        }
                        ui.add(
                            egui::Image::new(uri)
                                .max_height(240.0)
                                .max_width(ui.available_width())
                                .maintain_aspect_ratio(true)
                                .corner_radius(CornerRadius::same(4)),
                        );
                    }
                }
            });
    }

    /// The AI Summary section of a failed or flaky journey.
    fn explain_section(&mut self, ctx: &egui::Context, ui: &mut Ui, journey_id: &str) {
        ui.add_space(12.0);
        widgets::section_title(ui, "AI Summary");
        ui.add_space(2.0);
        self.explain_controls(
            ctx,
            ui,
            journey_id,
            "Explain failure",
            "Sends only this failure's report data (guard explain)",
        );
    }
}

/// A search box with a magnifier, like Rider's tool-window search.
fn search_field(ui: &mut Ui, text: &mut String) {
    Frame::new()
        .fill(theme::ISLAND)
        .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            // The caller lays out right to left; the box reads left to right.
            ui.allocate_ui_with_layout(
                vec2(222.0, 22.0),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    widgets::paint_icon(ui.painter(), r, Icon::Search, theme::TEXT_SECONDARY);
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .hint_text("Search journeys")
                            .frame(false)
                            .desired_width(200.0),
                    );
                },
            );
        });
}

/// A darker rounded block for paths and code.
fn code_block(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(Color32::from_rgb(0x26, 0x28, 0x2c))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui);
        });
}
