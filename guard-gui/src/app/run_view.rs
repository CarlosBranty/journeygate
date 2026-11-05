//! The Run view: services (guard status/up/down) and the current or last run.
//! The live log itself is the Run tab of the bottom tool window.

use super::shell::{exit_color, island, island_panel_frame};
use super::widgets::{self, Icon};
use super::{BottomTab, GuardApp, ProbeKind};
use crate::commands::{GuardCommand, RunMode};
use crate::status::yes_no;
use crate::theme;
use eframe::egui::{self, Align, CornerRadius, Layout, Margin, RichText, Sense, Ui, Vec2};
use egui_extras::{Column, TableBuilder};

impl GuardApp {
    pub(super) fn run_screen(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(island_panel_frame())
            .show(ctx, |ui| {
                island(ui, Margin::same(16), |ui| {
                    self.services(ctx, ui);
                    ui.add_space(18.0);
                    ui.separator();
                    ui.add_space(10.0);
                    self.run_summary(ctx, ui);
                });
            });
    }

    fn services(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let color = match &self.status {
                Some(Ok(s)) if s.ready => theme::VERIFIED,
                Some(_) => theme::BROKEN,
                None => theme::NEUTRAL,
            };
            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
            widgets::dot(ui.painter(), r.center(), 4.0, color);
            widgets::section_title(ui, "Services");
            if let Some(Ok(status)) = &self.status {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(&status.base_url)
                        .monospace()
                        .color(theme::TEXT_SECONDARY),
                );
                if status.ready {
                    widgets::chip(ui, "ready", theme::VERIFIED);
                } else {
                    widgets::chip(ui, "not ready", theme::BROKEN);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let idle = !self.running();
                if ui
                    .add_enabled(idle, egui::Button::new("Stop services"))
                    .on_hover_text("guard down")
                    .clicked()
                {
                    self.start(ctx, GuardCommand::Down);
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Start services"))
                    .on_hover_text("guard up --timeout <n>")
                    .clicked()
                {
                    let timeout_secs = self.up_timeout;
                    self.start(ctx, GuardCommand::Up { timeout_secs });
                }
                ui.add(
                    egui::DragValue::new(&mut self.up_timeout)
                        .range(1..=3600)
                        .suffix(" s"),
                )
                .on_hover_text("guard up --timeout");
                ui.label(RichText::new("Timeout").color(theme::TEXT_SECONDARY));
                ui.add_space(8.0);
                let probing = self.probe.is_some();
                if widgets::icon_button(
                    ui,
                    Icon::Refresh,
                    theme::TEXT,
                    !probing,
                    "Refresh: guard status --json",
                )
                .clicked()
                {
                    self.start_probe(ctx, ProbeKind::Status);
                }
                if probing {
                    // A static label: egui's Spinner repaints every frame.
                    ui.label(RichText::new("checking...").color(theme::TEXT_SECONDARY));
                }
            });
        });
        ui.add_space(8.0);
        let status = match &self.status {
            None => {
                ui.label(
                    RichText::new("Status not loaded yet: press Refresh.")
                        .color(theme::TEXT_SECONDARY),
                );
                return;
            }
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(theme::BROKEN));
                return;
            }
            Some(Ok(status)) => status.clone(),
        };
        let row_height = 28.0;
        egui::ScrollArea::horizontal()
            .id_salt("services-scroll")
            .show(ui, |ui| {
                TableBuilder::new(ui)
                    .id_salt("services-table")
                    .striped(false)
                    .resizable(false)
                    .vscroll(false)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::initial(170.0).at_least(110.0))
                    .column(Column::auto().at_least(70.0))
                    .column(Column::auto().at_least(60.0))
                    .column(Column::auto().at_least(60.0))
                    .column(Column::auto().at_least(80.0))
                    .column(Column::initial(240.0).at_least(100.0).clip(true))
                    .column(Column::remainder().at_least(120.0).clip(true))
                    .header(row_height, |mut header| {
                        for title in ["Name", "Kind", "Pid", "Alive", "Ready", "URL", "Log"] {
                            header.col(|ui| {
                                ui.label(
                                    RichText::new(title).color(theme::TEXT_SECONDARY).size(12.0),
                                );
                            });
                        }
                    })
                    .body(|mut body| {
                        for entry in &status.entries {
                            body.row(row_height, |mut row| {
                                let color = match entry.ready {
                                    Some(true) => theme::VERIFIED,
                                    Some(false) => theme::BROKEN,
                                    None => theme::NEUTRAL,
                                };
                                row.col(|ui| {
                                    let (r, _) =
                                        ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                                    widgets::dot(ui.painter(), r.center(), 3.5, color);
                                    ui.label(RichText::new(&entry.name).strong());
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(&entry.kind).color(theme::TEXT_SECONDARY),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(entry.pid.map_or("-".into(), |p| p.to_string()));
                                });
                                row.col(|ui| {
                                    ui.label(yes_no(entry.alive));
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(yes_no(entry.ready)).color(color));
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(entry.ready_url.as_deref().unwrap_or("-"))
                                            .monospace(),
                                    );
                                });
                                row.col(|ui| {
                                    ui.label(
                                        RichText::new(&entry.log)
                                            .monospace()
                                            .color(theme::TEXT_SECONDARY),
                                    )
                                    .on_hover_text(&entry.log);
                                });
                            });
                        }
                    });
            });
    }

    fn run_summary(&mut self, ctx: &egui::Context, ui: &mut Ui) {
        widgets::section_title(ui, "Run");
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(theme::GROUND.gamma_multiply(0.6))
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    match (&self.run, &self.last_exit) {
                        (Some(active), _) => {
                            widgets::chip(ui, "Running", theme::FOCUS);
                            ui.label(RichText::new(active.command.display()).monospace());
                        }
                        (None, Some((command, exit))) => {
                            widgets::chip(ui, &exit.label(), exit_color(*exit));
                            ui.label(RichText::new(command).monospace());
                        }
                        (None, None) => {
                            widgets::chip(ui, "Idle", theme::NEUTRAL);
                            ui.label(
                                RichText::new("No run yet in this project.")
                                    .color(theme::TEXT_SECONDARY),
                            );
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add_enabled(self.running(), egui::Button::new("Cancel"))
                            .on_hover_text("Kills the whole process tree")
                            .clicked()
                            && let Some(active) = &self.run
                        {
                            active.run.cancel();
                        }
                        if ui
                            .add_enabled(!self.running(), egui::Button::new("Open HTML report"))
                            .on_hover_text("guard report: serves the report until you press Cancel")
                            .clicked()
                        {
                            self.start(ctx, GuardCommand::Report);
                        }
                        if widgets::flat_button(ui, "Show log", true).clicked() {
                            self.bottom_open = true;
                            self.bottom_tab = BottomTab::Log;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Run mode").color(theme::TEXT_SECONDARY));
                    self.run_mode_selector(ui, !self.running());
                    ui.label(
                        RichText::new("Headed and UI runs still stream their output to the log.")
                            .color(theme::TEXT_DISABLED)
                            .size(12.0),
                    );
                });
                if self
                    .run
                    .as_ref()
                    .is_some_and(|a| a.command.run_mode().is_some_and(RunMode::interactive))
                {
                    ui.label(
                        RichText::new(
                            "Playwright UI mode is interactive: the progress counters may not update until you close it.",
                        )
                        .color(theme::AT_RISK)
                        .size(12.0),
                    );
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{} log lines kept (the last {} at most). Only one run happens at a time.",
                        self.log.len(),
                        crate::log_buffer::DEFAULT_CAP
                    ))
                    .color(theme::TEXT_SECONDARY)
                    .size(12.0),
                );
            });
    }
}
