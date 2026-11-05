//! Painter-drawn icons and the small Rider-like controls (stripe buttons,
//! toolbar icon buttons, the green run button, tabs, tree checkboxes). No
//! image assets: everything is a few shapes.

use crate::theme::{self, TestState};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Painter, Pos2, Rect, Response, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, pos2, vec2,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Tests,
    Flow,
    RunView,
    Log,
    Problems,
    Play,
    PlayAll,
    Stop,
    Gear,
    Branch,
    Folder,
    ChevronDown,
    ChevronRight,
    Refresh,
    Close,
    Minimize,
    Search,
    Target,
    Pause,
    StepForward,
    Fit,
}

/// Draws `icon` centred in `rect`, designed on a 16x16 grid.
pub fn paint_icon(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let u = rect.width().min(rect.height()) / 16.0;
    let c = rect.center();
    let at = |x: f32, y: f32| pos2(c.x + x * u, c.y + y * u);
    let thin = Stroke::new(1.3 * u.max(1.0), color);
    match icon {
        Icon::Play => {
            p.add(Shape::convex_polygon(
                vec![at(-4.0, -5.5), at(5.5, 0.0), at(-4.0, 5.5)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::PlayAll => {
            for dx in [-5.5, 0.5] {
                p.add(Shape::convex_polygon(
                    vec![at(dx, -5.0), at(dx + 5.5, 0.0), at(dx, 5.0)],
                    color,
                    Stroke::NONE,
                ));
            }
        }
        Icon::Stop => {
            p.rect_filled(
                Rect::from_center_size(c, Vec2::splat(10.0 * u)),
                CornerRadius::same(2),
                color,
            );
        }
        Icon::Tests => {
            // A flask: Rider's test tool window glyph.
            p.add(Shape::closed_line(
                vec![
                    at(-2.5, -6.0),
                    at(2.5, -6.0),
                    at(2.5, -1.5),
                    at(6.0, 5.0),
                    at(5.0, 6.5),
                    at(-5.0, 6.5),
                    at(-6.0, 5.0),
                    at(-2.5, -1.5),
                ],
                thin,
            ));
            p.line_segment([at(-4.0, 2.0), at(4.0, 2.0)], thin);
        }
        Icon::Flow => {
            let a = at(0.0, -4.5);
            let b = at(-5.0, 4.5);
            let d = at(5.0, 4.5);
            p.line_segment([a, b], thin);
            p.line_segment([a, d], thin);
            p.line_segment([b, d], thin);
            for n in [a, b, d] {
                p.circle_filled(n, 2.6 * u, theme::GROUND);
                p.circle_stroke(n, 2.4 * u, thin);
            }
        }
        Icon::RunView => {
            p.add(Shape::closed_line(
                vec![at(-4.0, -6.0), at(6.0, 0.0), at(-4.0, 6.0)],
                thin,
            ));
        }
        Icon::Log => {
            p.rect_stroke(
                Rect::from_min_max(at(-6.5, -5.5), at(6.5, 5.5)),
                CornerRadius::same(2),
                thin,
                StrokeKind::Middle,
            );
            p.add(Shape::line(
                vec![at(-3.5, -2.0), at(-1.0, 0.5), at(-3.5, 3.0)],
                thin,
            ));
            p.line_segment([at(0.5, 3.0), at(4.0, 3.0)], thin);
        }
        Icon::Problems => {
            p.circle_stroke(c, 6.0 * u, thin);
            p.line_segment([at(0.0, -3.5), at(0.0, 1.0)], thin);
            p.circle_filled(at(0.0, 3.3), 0.9 * u, color);
        }
        Icon::Gear => {
            p.circle_stroke(c, 4.6 * u, thin);
            p.circle_stroke(c, 1.8 * u, thin);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4;
                let dir = vec2(a.cos(), a.sin());
                p.line_segment(
                    [c + dir * 4.6 * u, c + dir * 6.6 * u],
                    Stroke::new(2.2 * u, color),
                );
            }
        }
        Icon::Branch => {
            let top = at(-3.0, -5.0);
            let bottom = at(-3.0, 5.0);
            let side = at(3.5, -3.0);
            p.line_segment([top, bottom], thin);
            p.add(Shape::line(
                vec![at(3.5, -1.2), at(3.5, 0.5), at(-3.0, 3.0)],
                thin,
            ));
            for n in [top, bottom, side] {
                p.circle_filled(n, 2.0 * u, theme::GROUND);
                p.circle_stroke(n, 1.8 * u, thin);
            }
        }
        Icon::Folder => {
            p.add(Shape::closed_line(
                vec![
                    at(-6.5, -5.0),
                    at(-2.0, -5.0),
                    at(-0.5, -3.5),
                    at(6.5, -3.5),
                    at(6.5, 5.0),
                    at(-6.5, 5.0),
                ],
                thin,
            ));
        }
        Icon::ChevronDown => {
            p.add(Shape::line(
                vec![at(-3.5, -1.5), at(0.0, 2.0), at(3.5, -1.5)],
                thin,
            ));
        }
        Icon::ChevronRight => {
            p.add(Shape::line(
                vec![at(-1.5, -3.5), at(2.0, 0.0), at(-1.5, 3.5)],
                thin,
            ));
        }
        Icon::Refresh => {
            let r = 5.0 * u;
            let points: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = -0.6 + i as f32 / 20.0 * 4.9;
                    c + vec2(a.cos(), a.sin()) * r
                })
                .collect();
            let end = points[0];
            p.add(Shape::line(points, thin));
            p.add(Shape::convex_polygon(
                vec![
                    end + vec2(-2.8, -1.2) * u,
                    end + vec2(1.8, -3.0) * u,
                    end + vec2(1.0, 1.8) * u,
                ],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Close => {
            p.line_segment([at(-4.0, -4.0), at(4.0, 4.0)], thin);
            p.line_segment([at(4.0, -4.0), at(-4.0, 4.0)], thin);
        }
        Icon::Minimize => {
            p.line_segment([at(-5.0, 0.0), at(5.0, 0.0)], thin);
        }
        Icon::Search => {
            p.circle_stroke(at(-1.0, -1.0), 4.5 * u, thin);
            p.line_segment([at(2.3, 2.3), at(6.0, 6.0)], thin);
        }
        Icon::Target => {
            p.circle_stroke(c, 6.0 * u, thin);
            p.circle_stroke(c, 3.0 * u, thin);
            p.circle_filled(c, 1.0 * u, color);
        }
        Icon::Pause => {
            for dx in [-3.0, 3.0] {
                p.rect_filled(
                    Rect::from_center_size(at(dx, 0.0), egui::vec2(3.0 * u, 11.0 * u)),
                    CornerRadius::same(1),
                    color,
                );
            }
        }
        Icon::StepForward => {
            p.add(Shape::convex_polygon(
                vec![at(-5.0, -5.5), at(3.0, 0.0), at(-5.0, 5.5)],
                color,
                Stroke::NONE,
            ));
            p.rect_filled(
                Rect::from_center_size(at(5.0, 0.0), egui::vec2(2.2 * u, 11.0 * u)),
                CornerRadius::same(1),
                color,
            );
        }
        Icon::Fit => {
            // Four corner brackets around the view.
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let corner = at(6.0 * sx, 6.0 * sy);
                p.line_segment([corner, at(6.0 * sx, 2.0 * sy)], thin);
                p.line_segment([corner, at(2.0 * sx, 6.0 * sy)], thin);
            }
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(5.0 * u)),
                CornerRadius::same(1),
                thin,
                StrokeKind::Middle,
            );
        }
    }
}

/// A round test-state badge of radius `r` (passed check, failed cross, ...).
pub fn paint_state(p: &Painter, c: Pos2, r: f32, state: TestState) {
    let color = state.color();
    let mark = Stroke::new((r * 0.28).max(1.2), theme::ISLAND);
    let at = |x: f32, y: f32| pos2(c.x + x * r, c.y + y * r);
    match state {
        TestState::Passed => {
            p.circle_filled(c, r, color);
            p.add(Shape::line(
                vec![at(-0.45, 0.02), at(-0.1, 0.38), at(0.48, -0.3)],
                mark,
            ));
        }
        TestState::Failed => {
            p.circle_filled(c, r, color);
            p.line_segment([at(-0.35, -0.35), at(0.35, 0.35)], mark);
            p.line_segment([at(0.35, -0.35), at(-0.35, 0.35)], mark);
        }
        TestState::Flaky => {
            p.circle_filled(c, r, color);
            p.line_segment([at(0.0, -0.5), at(0.0, 0.12)], mark);
            p.circle_filled(at(0.0, 0.45), r * 0.14, theme::ISLAND);
        }
        TestState::Skipped => {
            p.circle_stroke(c, r * 0.9, Stroke::new(1.2, color));
            p.line_segment([at(-0.4, 0.0), at(0.4, 0.0)], Stroke::new(1.2, color));
        }
        TestState::Affected => {
            p.circle_stroke(c, r * 0.9, Stroke::new(1.5, color));
            p.circle_filled(c, r * 0.38, color);
        }
        TestState::Quarantined => {
            p.circle_stroke(c, r * 0.9, Stroke::new(1.5, color));
            p.line_segment([at(-0.2, -0.38), at(-0.2, 0.38)], Stroke::new(1.4, color));
            p.line_segment([at(0.2, -0.38), at(0.2, 0.38)], Stroke::new(1.4, color));
        }
        TestState::NotRun => {
            p.circle_stroke(c, r * 0.9, Stroke::new(1.2, color));
        }
    }
}

/// A square icon button with a hover background (toolbar, tool windows).
pub fn icon_button(ui: &mut Ui, icon: Icon, color: Color32, enabled: bool, tip: &str) -> Response {
    let size = Vec2::splat(26.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if enabled && response.hovered() {
            p.rect_filled(rect, CornerRadius::same(5), theme::CONTROL_HOVER);
        }
        let color = if enabled { color } else { theme::TEXT_DISABLED };
        paint_icon(p, rect.shrink(5.0), icon, color);
    }
    response.on_hover_text(tip)
}

/// A tool-window stripe button. `active` paints it accent-filled, like the
/// focused tool window in Rider; `open` paints it with a quiet fill.
pub fn stripe_button(ui: &mut Ui, icon: Icon, active: bool, open: bool, tip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let fill = if active {
            Some(theme::ACCENT)
        } else if open {
            Some(theme::INACTIVE_SELECTION)
        } else if response.hovered() {
            Some(theme::CONTROL_HOVER)
        } else {
            None
        };
        if let Some(fill) = fill {
            p.rect_filled(rect, CornerRadius::same(6), fill);
        }
        let color = if active { Color32::WHITE } else { theme::TEXT };
        paint_icon(p, rect.shrink(7.0), icon, color);
    }
    response.on_hover_text(tip)
}

/// The green filled run button of the main toolbar.
pub fn run_button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let font = theme::semibold_font(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, Color32::WHITE);
    let size = vec2(galley.size().x + 40.0, 26.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let fill = if !enabled {
            theme::CONTROL_ACTIVE
        } else if response.hovered() {
            theme::RUN_GREEN_HOVER
        } else {
            theme::RUN_GREEN
        };
        p.rect_filled(rect, CornerRadius::same(5), fill);
        let fg = if enabled {
            Color32::WHITE
        } else {
            theme::TEXT_DISABLED
        };
        paint_icon(
            p,
            Rect::from_center_size(pos2(rect.left() + 16.0, rect.center().y), Vec2::splat(14.0)),
            Icon::Play,
            fg,
        );
        p.galley_with_override_text_color(
            pos2(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0),
            galley,
            fg,
        );
    }
    response
}

/// A flat text button with a hover fill (menus, toolbars, links).
pub fn flat_button(ui: &mut Ui, text: impl Into<egui::WidgetText>, enabled: bool) -> Response {
    ui.add_enabled(enabled, egui::Button::new(text).frame_when_inactive(false))
}

/// A tool-window tab; the selected one is outlined in accent like Rider's.
pub fn tab(ui: &mut Ui, label: &str, badge: Option<(String, Color32)>, selected: bool) -> Response {
    let font = theme::font(13.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font.clone(), theme::TEXT);
    let badge_galley = badge.as_ref().map(|(text, color)| {
        ui.painter()
            .layout_no_wrap(text.clone(), theme::font(11.5), *color)
    });
    let badge_w = badge_galley.as_ref().map_or(0.0, |g| g.size().x + 14.0);
    let size = vec2(galley.size().x + 20.0 + badge_w, 24.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if selected {
            p.rect_filled(
                rect,
                CornerRadius::same(5),
                Color32::from_rgb(0x23, 0x35, 0x58),
            );
            p.rect_stroke(
                rect,
                CornerRadius::same(5),
                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.85)),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            p.rect_filled(rect, CornerRadius::same(5), theme::CONTROL_HOVER);
        }
        let text_pos = pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0);
        let text_w = galley.size().x;
        p.galley(text_pos, galley, theme::TEXT);
        if let (Some(g), Some((_, color))) = (badge_galley, badge) {
            let r = Rect::from_min_size(
                pos2(text_pos.x + text_w + 6.0, rect.center().y - 8.0),
                vec2(g.size().x + 8.0, 16.0),
            );
            p.rect_filled(r, CornerRadius::same(8), color.gamma_multiply(0.18));
            p.galley(
                pos2(r.left() + 4.0, r.center().y - g.size().y / 2.0),
                g,
                color,
            );
        }
    }
    response
}

/// A small painted checkbox for tree rows; returns its click response.
pub fn tree_checkbox(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    checked: bool,
    enabled: bool,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect.expand(3.0), id, sense);
    let p = ui.painter();
    if checked {
        p.rect_filled(rect, CornerRadius::same(3), theme::ACCENT);
        p.add(Shape::line(
            vec![
                pos2(rect.left() + 3.0, rect.center().y),
                pos2(rect.left() + 6.0, rect.bottom() - 3.5),
                pos2(rect.right() - 3.0, rect.top() + 3.5),
            ],
            Stroke::new(1.6, Color32::WHITE),
        ));
    } else {
        let border = if !enabled {
            theme::BORDER
        } else if response.hovered() {
            theme::ACCENT
        } else {
            Color32::from_rgb(0x6f, 0x73, 0x7a)
        };
        p.rect_filled(rect, CornerRadius::same(3), theme::ISLAND);
        p.rect_stroke(
            rect,
            CornerRadius::same(3),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
    }
    response
}

/// A coloured status dot with an optional soft halo.
pub fn dot(p: &Painter, c: Pos2, r: f32, color: Color32) {
    p.circle_filled(c, r + 2.5, color.gamma_multiply(0.2));
    p.circle_filled(c, r, color);
}

/// Text in a tinted rounded chip.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    theme::chip(color)
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color).size(11.5));
        })
        .response
}

/// Paints `text` left-aligned at `pos` and returns its width.
pub fn text_at(p: &Painter, pos: Pos2, text: &str, font: egui::FontId, color: Color32) -> f32 {
    p.text(pos, Align2::LEFT_CENTER, text, font, color).width()
}

/// A section title inside an island.
pub fn section_title(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::semibold_font(13.0))
            .color(theme::TEXT),
    );
}
