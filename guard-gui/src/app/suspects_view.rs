//! "Possibly related changes": the `guard suspects` rows of one journey
//! (commits on its paths, uncommitted changes, who last changed the failing
//! line), shared by the Tests detail panel and the Problems tab. A click on a
//! sha copies the full sha to the clipboard.

use super::GuardApp;
use crate::suspects::{Blame, JourneySuspects, Suspect, current, relative_age, shorten};
use crate::theme;
use eframe::egui::{self, RichText, Ui};
use std::time::{SystemTime, UNIX_EPOCH};

/// Commits shown per journey in the Problems tab.
const COMPACT_COMMITS: usize = 3;
const SUBJECT_CHARS: usize = 72;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl GuardApp {
    /// Whether `journey_id` has any suspect row to show.
    pub(super) fn has_suspects(&self, journey_id: &str) -> bool {
        matches!(current(&self.suspects, &self.report_stamp), Some(Ok(doc))
            if doc.for_journey(journey_id).is_some_and(JourneySuspects::has_any))
    }

    /// The suspects of `journey_id`, or why there are none. `compact` (the
    /// Problems tab) shows fewer commits and says nothing while loading.
    /// Rows read for an earlier report count as loading, never as current.
    pub(super) fn suspects_rows(&self, ui: &mut Ui, journey_id: &str, compact: bool) {
        let journey = match current(&self.suspects, &self.report_stamp) {
            Some(Ok(doc)) => doc.for_journey(journey_id),
            Some(Err(e)) => {
                if !compact {
                    muted(ui, &format!("Related changes unavailable: {e}"));
                }
                return;
            }
            None => {
                if !compact && self.suspects_job.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        muted(ui, "Looking for related changes...");
                    });
                }
                return;
            }
        };
        let Some(journey) = journey.filter(|j| j.has_any()) else {
            if !compact {
                muted(ui, "No commit touching this journey's paths was found.");
            }
            return;
        };
        journey_rows(ui, journey, compact);
    }
}

fn journey_rows(ui: &mut Ui, journey: &JourneySuspects, compact: bool) {
    let now = now();
    let limit = if compact {
        COMPACT_COMMITS
    } else {
        journey.commits.len()
    };
    ui.spacing_mut().item_spacing.y = 2.0;
    if journey.commits.iter().any(|c| c.recent) {
        muted(ui, "Recent commits on these paths (none on this branch):");
    }
    for commit in journey.commits.iter().take(limit) {
        commit_row(ui, commit, now);
    }
    if !journey.uncommitted.is_empty() {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Uncommitted changes")
                    .color(theme::AT_RISK)
                    .size(12.5),
            );
            let n = journey.uncommitted.len();
            ui.label(
                RichText::new(if n == 1 {
                    "1 file".to_string()
                } else {
                    format!("{n} files")
                })
                .color(theme::TEXT_SECONDARY)
                .size(12.5),
            )
            .on_hover_text(journey.uncommitted.join("\n"));
        });
    }
    for blame in &journey.blame {
        blame_row(ui, blame, now, compact);
    }
}

/// A short sha that copies the full one when clicked.
fn sha_button(ui: &mut Ui, short: &str, full: &str) {
    let response = ui
        .add(
            egui::Button::new(
                RichText::new(short)
                    .monospace()
                    .size(12.0)
                    .color(theme::FOCUS),
            )
            .frame(false),
        )
        .on_hover_text(format!("{full}\nClick to copy"));
    if response.clicked() {
        ui.ctx().copy_text(full.to_string());
    }
}

fn commit_row(ui: &mut Ui, commit: &Suspect, now: u64) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        sha_button(ui, &commit.short, &commit.sha);
        ui.label(
            RichText::new(shorten(&commit.subject, SUBJECT_CHARS))
                .color(theme::TEXT)
                .size(12.5),
        )
        .on_hover_text(commit.files.join("\n"));
        ui.label(
            RichText::new(&commit.author)
                .color(theme::TEXT_SECONDARY)
                .size(12.5),
        );
        ui.label(
            RichText::new(relative_age(now, commit.timestamp))
                .color(theme::TEXT_DISABLED)
                .size(12.0),
        );
        if let Some(repo) = &commit.repo {
            ui.label(
                RichText::new(repo)
                    .monospace()
                    .color(theme::TEXT_DISABLED)
                    .size(11.5),
            );
        }
    });
}

fn blame_row(ui: &mut Ui, blame: &Blame, now: u64, compact: bool) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new("Failing line")
                .color(theme::TEXT_SECONDARY)
                .size(12.5),
        );
        ui.label(
            RichText::new(format!("{}:{}", blame.file, blame.line))
                .monospace()
                .color(theme::TEXT_SECONDARY)
                .size(12.0),
        );
        match (&blame.short, &blame.sha) {
            (Some(short), Some(sha)) if !blame.uncommitted => {
                sha_button(ui, short, sha);
                ui.label(
                    RichText::new(blame.author.as_deref().unwrap_or("-"))
                        .color(theme::TEXT_SECONDARY)
                        .size(12.5),
                );
                if let Some(at) = blame.timestamp {
                    ui.label(
                        RichText::new(relative_age(now, at))
                            .color(theme::TEXT_DISABLED)
                            .size(12.0),
                    );
                }
                if !compact && let Some(subject) = &blame.subject {
                    ui.label(
                        RichText::new(shorten(subject, SUBJECT_CHARS))
                            .color(theme::TEXT)
                            .size(12.5),
                    );
                }
            }
            _ => {
                ui.label(
                    RichText::new("not committed yet")
                        .color(theme::AT_RISK)
                        .size(12.5),
                );
            }
        }
    });
    if !compact && !blame.content.is_empty() {
        ui.label(
            RichText::new(&blame.content)
                .monospace()
                .size(11.5)
                .color(theme::TEXT_DISABLED),
        );
    }
}

fn muted(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT_DISABLED).size(12.0)).wrap());
}
