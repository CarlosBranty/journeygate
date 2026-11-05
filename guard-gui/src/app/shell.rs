//! The Rider-like window shell: main toolbar (project selector, branch, run
//! controls, services, settings), the left tool-window stripe, the bottom
//! tool window (Run log, Problems), the status bar and the welcome screen.

use super::widgets::{self, Icon};
use super::{BottomTab, GuardApp, ProbeKind, View};
use crate::commands::{GuardCommand, RunMode};
use crate::log_buffer::{LineKind, line_kind};
use crate::projects;
use crate::results::Outcome;
use crate::theme::{self, TestState};
use eframe::egui::{
    self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2,
    pos2, vec2,
};
use std::path::PathBuf;

/// What the project selector asked for (applied after the popup closes over
/// `self`).
enum ProjectAction {
    Open(PathBuf),
    Remove(PathBuf),
    Browse,
}

/// The side gutters of every island panel: the gap to the next island.
fn island_panel_margin() -> Margin {
    Margin {
        left: 0,
        right: theme::GAP,
        top: 0,
        bottom: theme::GAP,
    }
}

pub(super) fn island_panel_frame() -> Frame {
    Frame::new()
        .fill(theme::GROUND)
        .inner_margin(island_panel_margin())
}

/// Fills the panel with one island and runs `add` inside it.
pub(super) fn island<R>(ui: &mut Ui, margin: Margin, add: impl FnOnce(&mut Ui) -> R) -> R {
    theme::island()
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        })
        .inner
}

fn badge_color(name: &str) -> Color32 {
    theme::BADGES[projects::badge_index(name, theme::BADGES.len())]
}

/// The two-letter project badge.
fn paint_badge(ui: &Ui, rect: egui::Rect, name: &str) {
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(4), badge_color(name));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        projects::initials(name),
        theme::semibold_font(rect.height() * 0.5),
        Color32::WHITE,
    );
}

impl GuardApp {
    pub(super) fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar")
            .exact_height(40.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::GROUND)
                    .inner_margin(Margin::symmetric(8, 0)),
            )
            .show(ctx, |ui| {
                if let Some(project) = &self.project {
                    // Rider tints the left of the toolbar with the project colour.
                    let rect = ui.max_rect().expand2(vec2(8.0, 0.0));
                    let w = rect.width().min(560.0);
                    let tint = badge_color(&project.name).gamma_multiply(0.22);
                    let mut mesh = egui::Mesh::default();
                    mesh.colored_vertex(rect.left_top(), tint);
                    mesh.colored_vertex(pos2(rect.left() + w, rect.top()), Color32::TRANSPARENT);
                    mesh.colored_vertex(rect.left_bottom(), tint);
                    mesh.colored_vertex(pos2(rect.left() + w, rect.bottom()), Color32::TRANSPARENT);
                    mesh.add_triangle(0, 1, 2);
                    mesh.add_triangle(1, 3, 2);
                    ui.painter().add(mesh);
                }
                ui.horizontal_centered(|ui| {
                    app_icon(ui);
                    ui.add_space(8.0);
                    self.project_selector(ctx, ui);
                    if self.project.is_some() {
                        ui.add_space(4.0);
                        self.branch_label(ui);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.toolbar_right(ctx, ui);
                    });
                });
            });
    }

    fn project_selector(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let name = self
            .project
            .as_ref()
            .map_or_else(|| "Open project".to_string(), |p| p.name.clone());
        let font = theme::semibold_font(13.0);
        let galley = ui.painter().layout_no_wrap(name.clone(), font, theme::TEXT);
        let has_badge = self.project.is_some();
        let lead = if has_badge { 30.0 } else { 8.0 };
        let size = vec2(lead + galley.size().x + 26.0, 28.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        let open = egui::Popup::is_id_open(ctx, egui::Popup::default_response_id(&response));
        if response.hovered() || open {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(5), Color32::from_white_alpha(14));
        }
        if has_badge {
            let badge = egui::Rect::from_min_size(
                pos2(rect.left() + 5.0, rect.center().y - 10.0),
                Vec2::splat(20.0),
            );
            paint_badge(ui, badge, &name);
        }
        ui.painter().galley(
            pos2(rect.left() + lead, rect.center().y - galley.size().y / 2.0),
            galley,
            theme::TEXT,
        );
        widgets::paint_icon(
            ui.painter(),
            egui::Rect::from_center_size(
                pos2(rect.right() - 12.0, rect.center().y),
                Vec2::splat(12.0),
            ),
            Icon::ChevronDown,
            theme::TEXT_SECONDARY,
        );
        let response = response.on_hover_text("Switch project");
        let mut action = None;
        egui::Popup::menu(&response)
            .gap(4.0)
            .width(400.0)
            .show(|ui| {
                ui.set_min_width(400.0);
                ui.add_space(2.0);
                ui.label(
                    RichText::new("Recent projects")
                        .color(theme::TEXT_SECONDARY)
                        .size(12.0),
                );
                if self.recents.paths().is_empty() {
                    ui.label(RichText::new("No recent projects yet.").color(theme::TEXT_DISABLED));
                }
                let current = self.project.as_ref().map(|p| p.root.clone());
                for path in self.recents.paths() {
                    let is_current = current.as_ref() == Some(path);
                    if let Some(a) = recent_row(ui, path, is_current) {
                        action = Some(a);
                    }
                }
                ui.separator();
                let browsing = self.folder_rx.is_some();
                if menu_item(ui, Icon::Folder, "Open project...", !browsing).clicked() {
                    action = Some(ProjectAction::Browse);
                }
                if let Some(path) = current.filter(|p| self.recents.contains(p))
                    && menu_item(ui, Icon::Close, "Remove from recent", true)
                        .on_hover_text("Forget the current project in this list")
                        .clicked()
                {
                    action = Some(ProjectAction::Remove(path));
                }
            });
        match action {
            Some(ProjectAction::Open(path)) => self.open_project(ctx, path, true),
            Some(ProjectAction::Remove(path)) => {
                self.recents.remove(&path);
            }
            Some(ProjectAction::Browse) => self.browse_for_project(ctx),
            None => {}
        }
    }

    fn branch_label(&self, ui: &mut Ui) {
        let (text, color) = match (&self.branch, &self.branch_rx) {
            (Some(b), _) => (b.clone(), theme::TEXT),
            (None, Some(_)) => ("...".to_string(), theme::TEXT_SECONDARY),
            (None, None) => ("no git branch".to_string(), theme::TEXT_SECONDARY),
        };
        let response = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                widgets::paint_icon(ui.painter(), r, Icon::Branch, theme::TEXT_SECONDARY);
                ui.label(RichText::new(text).color(color));
            })
            .response;
        response.on_hover_text("Current git branch of the project (read-only)");
    }

    fn toolbar_right(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.x = 4.0;
        // Laid out right to left.
        let settings = widgets::icon_button(ui, Icon::Gear, theme::TEXT, true, "Settings");
        self.settings_menu(ctx, &settings);
        ui.add_space(6.0);
        self.services_indicator(ui);
        ui.add_space(6.0);
        toolbar_divider(ui);
        ui.add_space(4.0);
        let has_project = self.project.is_some();
        let idle = !self.running() && has_project;
        if widgets::icon_button(
            ui,
            Icon::Stop,
            theme::STOP_RED,
            self.running(),
            "Stop the current run (kills Playwright and its browsers)",
        )
        .clicked()
            && let Some(active) = &self.run
        {
            active.run.cancel();
        }
        if widgets::icon_button(
            ui,
            Icon::PlayAll,
            theme::RUN_GREEN_HOVER,
            idle,
            "Run all: guard test --all",
        )
        .clicked()
        {
            self.request_test(ctx, GuardCommand::TestAll(self.run_mode));
        }
        ui.add_space(2.0);
        if widgets::run_button(ui, "Run affected", idle)
            .on_hover_text("guard test --affected")
            .clicked()
        {
            self.request_test(ctx, GuardCommand::TestAffected(self.run_mode));
        }
        ui.add_space(4.0);
        self.run_mode_selector(ui, !self.running());
        if self.running() {
            ui.add_space(8.0);
            ui.label(RichText::new("Running...").color(theme::FOCUS));
        }
    }

    /// "Run mode": how every `guard test` the window starts drives the
    /// browser (Headless, `--headed` or `--ui`). Kept with the layout.
    pub(super) fn run_mode_selector(&mut self, ui: &mut Ui, enabled: bool) {
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(("run-mode", ui.id()))
                .selected_text(self.run_mode.short_label())
                .show_ui(ui, |ui| {
                    for mode in RunMode::ALL {
                        ui.selectable_value(&mut self.run_mode, mode, mode.label());
                    }
                })
                .response
                .on_hover_text(format!(
                    "Run mode: {}. Applies to Run all, Run affected and the checked journeys.{}",
                    self.run_mode.label(),
                    if self.run_mode.interactive() {
                        " Playwright's UI is interactive: progress counters may not update and no report summary is read."
                    } else {
                        ""
                    }
                ));
        });
    }

    fn services_indicator(&mut self, ui: &mut Ui) {
        let (color, label) = match (&self.status, &self.probe) {
            (_, Some(p)) if p.kind == ProbeKind::Status => (theme::AT_RISK, "Checking"),
            (Some(Ok(s)), _) if s.ready => (theme::VERIFIED, "Services"),
            (Some(Ok(_)), _) => (theme::BROKEN, "Services"),
            (Some(Err(_)), _) => (theme::BROKEN, "Services"),
            (None, _) => (theme::NEUTRAL, "Services"),
        };
        let galley = ui
            .painter()
            .layout_no_wrap(label.to_string(), theme::font(13.0), theme::TEXT);
        let size = vec2(galley.size().x + 30.0, 26.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, CornerRadius::same(5), theme::CONTROL_HOVER);
        }
        widgets::dot(
            ui.painter(),
            pos2(rect.left() + 12.0, rect.center().y),
            4.0,
            color,
        );
        ui.painter().galley(
            pos2(rect.left() + 22.0, rect.center().y - galley.size().y / 2.0),
            galley,
            theme::TEXT,
        );
        let response = response.on_hover_ui(|ui| match &self.status {
            None => {
                ui.label("Services status not loaded yet.");
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(theme::BROKEN));
            }
            Some(Ok(status)) => {
                ui.label(format!("base_url: {}", status.base_url));
                for entry in &status.entries {
                    let (c, word) = match entry.ready {
                        Some(true) => (theme::VERIFIED, "ready"),
                        Some(false) => (theme::BROKEN, "not ready"),
                        None => (theme::NEUTRAL, "-"),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("\u{25CF}").color(c));
                        ui.label(format!("{} ({}): {word}", entry.name, entry.kind));
                    });
                }
            }
        });
        if response.clicked() {
            self.view = View::Run;
        }
    }

    fn settings_menu(&mut self, ctx: &egui::Context, response: &egui::Response) {
        egui::Popup::menu(response)
            .gap(4.0)
            .align(egui::RectAlign::BOTTOM_END)
            .show(|ui| {
                ui.set_min_width(260.0);
                match &self.guard {
                    Ok(path) => {
                        ui.label(RichText::new("guard found").color(theme::VERIFIED))
                            .on_hover_text(path.display().to_string());
                    }
                    Err(e) => {
                        ui.label(RichText::new(e.to_string()).color(theme::BROKEN));
                    }
                }
                if menu_item(ui, Icon::Refresh, "Recheck guard and E2E_* variables", true).clicked()
                {
                    self.guard = crate::guard_bin::from_process();
                    self.env_names = crate::env_status::from_process();
                }
                if menu_item(
                    ui,
                    Icon::Refresh,
                    "Reload catalog and results",
                    self.project.is_some(),
                )
                .clicked()
                {
                    self.reload_catalog();
                    self.reload_results();
                }
                if menu_item(ui, Icon::Gear, "AI settings...", true).clicked() {
                    self.ai_window = true;
                    ui.close();
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("guard up timeout");
                    ui.add(
                        egui::DragValue::new(&mut self.up_timeout)
                            .range(1..=3600)
                            .suffix(" s"),
                    );
                });
            });
        let _ = ctx;
    }

    pub(super) fn stripe(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("stripe")
            .exact_width(44.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::new().fill(theme::GROUND).inner_margin(Margin {
                left: 6,
                right: 6,
                top: 2,
                bottom: 6,
            }))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let has_project = self.project.is_some();
                for (view, icon, tip) in [
                    (View::Tests, Icon::Tests, "Tests"),
                    (View::Flow, Icon::Flow, "Flow"),
                    (View::Run, Icon::RunView, "Run and services"),
                ] {
                    let active = self.view == view && has_project;
                    if widgets::stripe_button(ui, icon, active, false, tip).clicked() {
                        self.view = view;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                    let problems = self.problem_ids().len();
                    for (tab, icon, tip) in [
                        (BottomTab::Problems, Icon::Problems, "Problems"),
                        (BottomTab::Log, Icon::Log, "Run log"),
                    ] {
                        let open = self.bottom_open && self.bottom_tab == tab && has_project;
                        let response = widgets::stripe_button(ui, icon, false, open, tip);
                        if tab == BottomTab::Problems && problems > 0 {
                            ui.painter().circle_filled(
                                response.rect.right_top() + vec2(-6.0, 6.0),
                                3.5,
                                theme::BROKEN,
                            );
                        }
                        if response.clicked() {
                            if open {
                                self.bottom_open = false;
                            } else {
                                self.bottom_open = true;
                                self.bottom_tab = tab;
                            }
                        }
                    }
                });
            });
    }

    /// Failed and flaky journeys in catalog order.
    pub(super) fn problem_ids(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter(|r| {
                self.results
                    .get(&r.id)
                    .is_some_and(|res| matches!(res.outcome, Outcome::Failed | Outcome::Flaky))
            })
            .map(|r| r.id.clone())
            .collect()
    }

    pub(super) fn tool_window(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("tool-window")
            .resizable(true)
            .default_height(250.0)
            .height_range(120.0..=700.0)
            .show_separator_line(false)
            .frame(island_panel_frame())
            .show(ctx, |ui| {
                island(ui, Margin::symmetric(8, 6), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let run_label = match &self.run {
                            Some(active) => format!("Run: {}", active.command.display()),
                            None => "Run".to_string(),
                        };
                        if widgets::tab(ui, &run_label, None, self.bottom_tab == BottomTab::Log)
                            .clicked()
                        {
                            self.bottom_tab = BottomTab::Log;
                        }
                        let problems = self.problem_ids().len();
                        let badge = (problems > 0).then(|| (problems.to_string(), theme::BROKEN));
                        if widgets::tab(
                            ui,
                            "Problems",
                            badge,
                            self.bottom_tab == BottomTab::Problems,
                        )
                        .clicked()
                        {
                            self.bottom_tab = BottomTab::Problems;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if widgets::icon_button(
                                ui,
                                Icon::Minimize,
                                theme::TEXT_SECONDARY,
                                true,
                                "Hide",
                            )
                            .clicked()
                            {
                                self.bottom_open = false;
                            }
                        });
                    });
                    ui.add_space(2.0);
                    match self.bottom_tab {
                        BottomTab::Log => self.log_tab(ctx, ui),
                        BottomTab::Problems => self.problems_tab(ctx, ui),
                    }
                });
            });
    }

    fn log_tab(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            match (&self.run, &self.last_exit) {
                (Some(_), _) => {
                    widgets::chip(ui, "Running", theme::FOCUS);
                }
                (None, Some((_, exit))) => {
                    widgets::chip(ui, &exit.label(), exit_color(*exit));
                }
                (None, None) => {
                    widgets::chip(ui, "Idle", theme::NEUTRAL);
                }
            }
            let command = match (&self.run, &self.last_exit) {
                (Some(active), _) => active.command.display(),
                (None, Some((command, _))) => command.clone(),
                (None, None) => "No run yet.".into(),
            };
            ui.label(
                RichText::new(command)
                    .monospace()
                    .color(theme::TEXT_SECONDARY),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if widgets::icon_button(
                    ui,
                    Icon::Stop,
                    theme::STOP_RED,
                    self.running(),
                    "Cancel: kills the whole process tree",
                )
                .clicked()
                    && let Some(active) = &self.run
                {
                    active.run.cancel();
                }
                if widgets::flat_button(ui, "Open HTML report", !self.running())
                    .on_hover_text("guard report: serves the report until you press Cancel")
                    .clicked()
                {
                    self.start(ctx, GuardCommand::Report);
                }
                if self.log.dropped() > 0 {
                    ui.label(
                        RichText::new(format!("{} older lines dropped", self.log.dropped()))
                            .color(theme::TEXT_SECONDARY)
                            .size(12.0),
                    );
                }
            });
        });
        ui.add_space(2.0);
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 1.0;
        Frame::new()
            .fill(Color32::from_rgb(0x19, 0x1a, 0x1c))
            .corner_radius(CornerRadius::same(5))
            .inner_margin(Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if self.log.is_empty() {
                    ui.label(RichText::new("Run output appears here.").color(theme::TEXT_DISABLED));
                    return;
                }
                // Only the visible rows are laid out each frame.
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show_rows(ui, row_height, self.log.len(), |ui, range| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        for index in range {
                            if let Some(line) = self.log.get(index) {
                                let color = match line_kind(line) {
                                    LineKind::Command => theme::FOCUS,
                                    LineKind::Exit => theme::TEXT_SECONDARY,
                                    LineKind::Error => theme::BROKEN,
                                    LineKind::Pass => theme::VERIFIED,
                                    LineKind::Plain => theme::TEXT,
                                };
                                ui.label(RichText::new(line).monospace().color(color));
                            }
                        }
                    });
            });
    }

    fn problems_tab(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        let ids = self.problem_ids();
        if ids.is_empty() {
            ui.add_space(8.0);
            ui.label(
                RichText::new("No problems: no failed or flaky journey in the last report.")
                    .color(theme::TEXT_SECONDARY),
            );
            return;
        }
        // "Explain failures": every failed/flaky journey in one summary,
        // shown beside the list once asked for.
        let mut focus = None;
        if self.explain.contains_key(super::ALL_FAILURES) {
            let height = ui.available_height();
            let list_width = (ui.available_width() * 0.42).max(260.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    vec2(list_width, height),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_min_size(vec2(list_width, height));
                        self.explain_failures_bar(ui, ids.len(), false);
                        focus = self.problem_list(ui, &ids);
                    },
                );
                ui.separator();
                ui.allocate_ui_with_layout(
                    vec2(ui.available_width(), height),
                    Layout::top_down(Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("problems-summary")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.add_space(4.0);
                                widgets::section_title(ui, "AI Summary: all failures");
                                ui.add_space(2.0);
                                self.explain_controls(
                                    ctx,
                                    ui,
                                    super::ALL_FAILURES,
                                    "Explain failures",
                                    "Sends only the failures' report data (guard explain)",
                                );
                            });
                    },
                );
            });
        } else {
            self.explain_failures_bar(ui, ids.len(), true);
            focus = self.problem_list(ui, &ids);
        }
        if let Some(id) = focus {
            if let Some(area) = self.row(&id).map(|r| r.area.clone()) {
                self.collapsed.remove(&area);
            }
            self.view = View::Tests;
            self.set_focus(ctx, Some(id));
        }
    }

    /// The Problems tab header: the count, until a summary is shown the
    /// "Explain failures" action, and always who answers in which language
    /// with the "AI settings..." button to change it.
    fn explain_failures_bar(&mut self, ui: &mut Ui, count: usize, with_button: bool) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{count} failed or flaky"))
                    .color(theme::TEXT_SECONDARY)
                    .size(12.5),
            );
            let target = self.explain_target();
            if with_button {
                ui.add_space(6.0);
                let busy = self.explain_job.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new("Explain failures"))
                    .on_hover_text(format!(
                        "guard explain: summarizes every failed and flaky journey with {target}"
                    ))
                    .clicked()
                {
                    let ctx = ui.ctx().clone();
                    self.start_explain(&ctx, super::ALL_FAILURES.to_string());
                }
            }
            ui.add_space(6.0);
            ui.label(RichText::new(target).color(theme::TEXT_DISABLED).size(12.0));
            if widgets::flat_button(ui, "AI settings...", true).clicked() {
                self.ai_window = true;
            }
        });
        if let Some(hint) = &self.auto_summary_hint {
            ui.label(RichText::new(hint).color(theme::TEXT_DISABLED).size(12.0));
        }
    }

    /// The failed/flaky rows; returns the one clicked.
    fn problem_list(&mut self, ui: &mut Ui, ids: &[String]) -> Option<String> {
        let mut focus = None;
        egui::ScrollArea::vertical()
            .id_salt("problems-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for id in ids {
                    let Some(result) = self.results.get(id) else {
                        continue;
                    };
                    let state = if result.outcome == Outcome::Failed {
                        TestState::Failed
                    } else {
                        TestState::Flaky
                    };
                    let error = result
                        .error
                        .as_deref()
                        .and_then(|e| e.lines().find(|l| !l.trim().is_empty()))
                        .unwrap_or("no error message")
                        .trim()
                        .to_string();
                    let (rect, response) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                    let p = ui.painter_at(rect);
                    if self.focused.as_deref() == Some(id.as_str()) {
                        p.rect_filled(rect, CornerRadius::same(4), theme::SELECTION);
                    } else if response.hovered() {
                        p.rect_filled(rect, CornerRadius::same(4), theme::HOVER);
                    }
                    widgets::paint_state(&p, pos2(rect.left() + 14.0, rect.center().y), 6.0, state);
                    let w = widgets::text_at(
                        &p,
                        pos2(rect.left() + 28.0, rect.center().y),
                        id,
                        theme::font(13.0),
                        theme::TEXT,
                    );
                    widgets::text_at(
                        &p,
                        pos2(rect.left() + 40.0 + w, rect.center().y),
                        &error,
                        theme::font(12.5),
                        theme::TEXT_SECONDARY,
                    );
                    if response.clicked() {
                        focus = Some(id.clone());
                    }
                    // Its possibly related changes, under the row.
                    if self.has_suspects(id) {
                        ui.horizontal(|ui| {
                            ui.add_space(28.0);
                            ui.vertical(|ui| self.suspects_rows(ui, id, true));
                        });
                    }
                }
            });
        focus
    }

    pub(super) fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status-bar")
            .exact_height(26.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(theme::GROUND)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let small =
                        |text: String, color: Color32| RichText::new(text).size(12.0).color(color);
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    widgets::paint_icon(ui.painter(), r, Icon::Folder, theme::TEXT_SECONDARY);
                    match &self.project {
                        Some(project) => {
                            ui.label(small(
                                project.root.display().to_string(),
                                theme::TEXT_SECONDARY,
                            ));
                            if let Some(summary) = project.validation.summary() {
                                ui.label(small(summary, theme::AT_RISK));
                            }
                        }
                        None => {
                            ui.label(small("No project".into(), theme::TEXT_SECONDARY));
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        match &self.last_exit {
                            Some((command, exit)) => {
                                ui.label(small(
                                    format!("Last run: {}", exit.label()),
                                    exit_color(*exit),
                                ))
                                .on_hover_text(command);
                            }
                            None => match crate::results::report_outcome(&self.results) {
                                // Results read from disk, no run this session.
                                Some(outcome) => {
                                    let age = self
                                        .report_stamp
                                        .and_then(|s| s.modified)
                                        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok());
                                    ui.label(small(
                                        crate::results::last_report_label(outcome, age),
                                        outcome_color(outcome),
                                    ))
                                    .on_hover_text(
                                        "From e2e-playwright/results/report.json, not run this session",
                                    );
                                }
                                None => {
                                    ui.label(small("Last run: none".into(), theme::TEXT_SECONDARY));
                                }
                            },
                        }
                        let names = &self.env_names;
                        let env = ui.label(small(
                            format!("E2E_* {} set", names.len()),
                            if names.is_empty() {
                                theme::TEXT_SECONDARY
                            } else {
                                theme::TEXT
                            },
                        ));
                        if names.is_empty() {
                            env.on_hover_text("No E2E_* variable is set");
                        } else {
                            // Names only: values are never read or shown.
                            env.on_hover_text(names.join("\n"));
                        }
                        match &self.guard {
                            Ok(path) => {
                                ui.label(small("guard found".into(), theme::VERIFIED))
                                    .on_hover_text(path.display().to_string());
                            }
                            Err(e) => {
                                ui.label(small("guard not found".into(), theme::BROKEN))
                                    .on_hover_text(e.to_string());
                            }
                        }
                        if self.project.is_some() {
                            ui.label(small(
                                format!("{} journeys", self.rows.len()),
                                theme::TEXT_SECONDARY,
                            ));
                        }
                    });
                });
            });
    }

    /// No project yet: open one or pick a recent one.
    pub(super) fn welcome(&mut self, ctx: &egui::Context) {
        let mut action = None;
        egui::CentralPanel::default()
            .frame(island_panel_frame())
            .show(ctx, |ui| {
                island(ui, Margin::same(24), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.18);
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                        paint_app_mark(ui, r);
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new("Open a project")
                                .font(theme::semibold_font(20.0))
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(
                                "A project is a folder with target.toml and e2e-playwright/catalog/journeys.json.",
                            )
                            .color(theme::TEXT_SECONDARY),
                        );
                        ui.add_space(14.0);
                        let browsing = self.folder_rx.is_some();
                        let button = egui::Button::new(
                            RichText::new("Open project...").color(Color32::WHITE),
                        )
                        .fill(theme::ACCENT)
                        .stroke(Stroke::NONE)
                        .min_size(vec2(150.0, 30.0));
                        if ui.add_enabled(!browsing, button).clicked() {
                            action = Some(ProjectAction::Browse);
                        }
                        if !self.recents.paths().is_empty() {
                            ui.add_space(20.0);
                            ui.label(
                                RichText::new("Recent projects")
                                    .color(theme::TEXT_SECONDARY)
                                    .size(12.0),
                            );
                            ui.add_space(4.0);
                            ui.allocate_ui(vec2(420.0, 400.0), |ui| {
                                for path in self.recents.paths() {
                                    if let Some(a) = recent_row(ui, path, false) {
                                        action = Some(a);
                                    }
                                }
                            });
                        }
                    });
                });
            });
        match action {
            Some(ProjectAction::Open(path)) => self.open_project(ctx, path, true),
            Some(ProjectAction::Remove(path)) => {
                self.recents.remove(&path);
            }
            Some(ProjectAction::Browse) => self.browse_for_project(ctx),
            None => {}
        }
    }
}

pub(super) fn exit_color(exit: crate::runner::Exit) -> Color32 {
    match exit {
        crate::runner::Exit::Code(0) => theme::VERIFIED,
        crate::runner::Exit::Cancelled => theme::AT_RISK,
        _ => theme::BROKEN,
    }
}

fn outcome_color(outcome: crate::results::Outcome) -> Color32 {
    match outcome {
        crate::results::Outcome::Passed => theme::VERIFIED,
        crate::results::Outcome::Flaky => theme::AT_RISK,
        crate::results::Outcome::Failed => theme::BROKEN,
        crate::results::Outcome::Skipped => theme::TEXT_SECONDARY,
    }
}

/// The app mark: a rounded accent square with a check.
fn paint_app_mark(ui: &Ui, rect: egui::Rect) {
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same((rect.height() * 0.24) as u8),
        theme::ACCENT,
    );
    let at = |x: f32, y: f32| {
        pos2(
            rect.left() + x * rect.width(),
            rect.top() + y * rect.height(),
        )
    };
    p.add(egui::Shape::line(
        vec![at(0.27, 0.52), at(0.44, 0.68), at(0.74, 0.34)],
        Stroke::new(rect.height() * 0.11, Color32::WHITE),
    ));
}

fn app_icon(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
    paint_app_mark(ui, rect);
}

fn toolbar_divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 20.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::ZERO, theme::BORDER);
}

/// A menu row with an icon.
fn menu_item(ui: &mut Ui, icon: Icon, text: &str, enabled: bool) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), sense);
    let p = ui.painter();
    if enabled && response.hovered() {
        p.rect_filled(rect, CornerRadius::same(4), theme::SELECTION);
    }
    let color = if enabled {
        theme::TEXT
    } else {
        theme::TEXT_DISABLED
    };
    widgets::paint_icon(
        p,
        egui::Rect::from_center_size(pos2(rect.left() + 14.0, rect.center().y), Vec2::splat(14.0)),
        icon,
        color,
    );
    widgets::text_at(
        p,
        pos2(rect.left() + 30.0, rect.center().y),
        text,
        theme::font(13.0),
        color,
    );
    response
}

/// One recent project: badge, name, path, and a remove button on hover.
fn recent_row(ui: &mut Ui, path: &std::path::Path, current: bool) -> Option<ProjectAction> {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let remove_rect = egui::Rect::from_center_size(
        pos2(rect.right() - 16.0, rect.center().y),
        Vec2::splat(22.0),
    );
    let remove = ui.interact(remove_rect, response.id.with("remove"), Sense::click());
    let name = projects::display_name(path);
    let hovered = response.hovered() || remove.hovered();
    {
        let p = ui.painter();
        if hovered {
            p.rect_filled(rect, CornerRadius::same(5), theme::SELECTION);
        }
        let badge = egui::Rect::from_min_size(
            pos2(rect.left() + 8.0, rect.center().y - 12.0),
            Vec2::splat(24.0),
        );
        paint_badge(ui, badge, &name);
        let p = ui.painter_at(rect.with_max_x(remove_rect.left() - 4.0));
        widgets::text_at(
            &p,
            pos2(rect.left() + 42.0, rect.top() + 13.0),
            &name,
            theme::semibold_font(13.0),
            theme::TEXT,
        );
        widgets::text_at(
            &p,
            pos2(rect.left() + 42.0, rect.top() + 28.0),
            &path.display().to_string(),
            theme::font(11.5),
            theme::TEXT_SECONDARY,
        );
        if current {
            let x = rect.left()
                + 46.0
                + p.layout_no_wrap(name.clone(), theme::semibold_font(13.0), theme::TEXT)
                    .size()
                    .x;
            widgets::text_at(
                &p,
                pos2(x, rect.top() + 13.0),
                "current",
                theme::font(11.5),
                theme::FOCUS,
            );
        }
        if hovered {
            let p = ui.painter();
            if remove.hovered() {
                p.rect_filled(remove_rect, CornerRadius::same(4), theme::CONTROL_ACTIVE);
            }
            widgets::paint_icon(
                p,
                remove_rect.shrink(5.0),
                Icon::Close,
                theme::TEXT_SECONDARY,
            );
        }
    }
    if remove.on_hover_text("Remove from recent").clicked() {
        return Some(ProjectAction::Remove(path.to_path_buf()));
    }
    if response.on_hover_text(path.display().to_string()).clicked() {
        return Some(ProjectAction::Open(path.to_path_buf()));
    }
    None
}
