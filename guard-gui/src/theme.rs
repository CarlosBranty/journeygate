//! The look: JetBrains Rider "New UI" dark (islands layout), applied through
//! egui's `Style`/`Visuals`, plus the status palette of the impact-graph
//! spike for test states. Pure parts (state mapping, font lookup) are tested.

use crate::results::Outcome;
use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow,
    Stroke, TextStyle, Vec2,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
mod tests;

// Surfaces.
/// Window ground: main toolbar, tool-window stripes, status bar and the gaps
/// between islands.
pub const GROUND: Color32 = Color32::from_rgb(0x2b, 0x2d, 0x30);
/// Island fill: editor-like content (the main view, tool windows).
pub const ISLAND: Color32 = Color32::from_rgb(0x1e, 0x1f, 0x22);
/// Popups, menus and dialogs.
pub const POPUP: Color32 = Color32::from_rgb(0x2b, 0x2d, 0x30);
pub const BORDER: Color32 = Color32::from_rgb(0x39, 0x3b, 0x40);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x4e, 0x51, 0x57);
/// Row hover on an island.
pub const HOVER: Color32 = Color32::from_rgb(0x2a, 0x2c, 0x30);
/// Selection without keyboard focus, and the selected stripe button.
pub const INACTIVE_SELECTION: Color32 = Color32::from_rgb(0x39, 0x3b, 0x40);
pub const SELECTION: Color32 = Color32::from_rgb(0x2e, 0x43, 0x6e);
pub const CONTROL: Color32 = Color32::from_rgb(0x2b, 0x2d, 0x30);
pub const CONTROL_HOVER: Color32 = Color32::from_rgb(0x39, 0x3b, 0x40);
pub const CONTROL_ACTIVE: Color32 = Color32::from_rgb(0x43, 0x45, 0x4a);

// Text.
pub const TEXT: Color32 = Color32::from_rgb(0xdf, 0xe1, 0xe5);
pub const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x86, 0x8a, 0x91);
pub const TEXT_DISABLED: Color32 = Color32::from_rgb(0x5a, 0x5d, 0x63);

// Accents.
pub const ACCENT: Color32 = Color32::from_rgb(0x35, 0x74, 0xf0);
pub const RUN_GREEN: Color32 = Color32::from_rgb(0x57, 0x96, 0x5c);
pub const RUN_GREEN_HOVER: Color32 = Color32::from_rgb(0x61, 0xa6, 0x66);
pub const STOP_RED: Color32 = Color32::from_rgb(0xdb, 0x5c, 0x5c);

// Status palette (impact-graph egui spike).
pub const VERIFIED: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
pub const AT_RISK: Color32 = Color32::from_rgb(0xf5, 0x9e, 0x0b);
pub const BROKEN: Color32 = Color32::from_rgb(0xe5, 0x48, 0x4d);
pub const CONFLICT: Color32 = Color32::from_rgb(0xb4, 0x5c, 0xff);
pub const FOCUS: Color32 = Color32::from_rgb(0x59, 0xa7, 0xff);
pub const NEUTRAL: Color32 = Color32::from_rgb(0x6f, 0x73, 0x7a);

/// Project badge colours, picked by [`crate::projects::badge_index`].
pub const BADGES: [Color32; 6] = [
    Color32::from_rgb(0x9b, 0x59, 0xd0),
    Color32::from_rgb(0x35, 0x74, 0xf0),
    Color32::from_rgb(0x2e, 0x9e, 0x78),
    Color32::from_rgb(0xc9, 0x7a, 0x2e),
    Color32::from_rgb(0xc9, 0x4f, 0x6d),
    Color32::from_rgb(0x3f, 0x9b, 0xb5),
];

/// Island corner radius.
pub const ISLAND_RADIUS: u8 = 8;
/// Gap between islands.
pub const GAP: i8 = 5;

/// The state a journey row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestState {
    Passed,
    Failed,
    Flaky,
    Skipped,
    /// No result yet, and the last affected preview selects it.
    Affected,
    /// No result yet, and the catalog quarantines it.
    Quarantined,
    NotRun,
}

impl TestState {
    pub fn color(self) -> Color32 {
        match self {
            Self::Passed => VERIFIED,
            Self::Failed => BROKEN,
            Self::Flaky => AT_RISK,
            Self::Quarantined => CONFLICT,
            Self::Affected => FOCUS,
            Self::Skipped | Self::NotRun => NEUTRAL,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Flaky => "flaky",
            Self::Skipped => "skipped",
            Self::Affected => "affected",
            Self::Quarantined => "quarantined",
            Self::NotRun => "not run",
        }
    }

    /// Worse states win when an area sums up its journeys.
    fn rank(self) -> u8 {
        match self {
            Self::NotRun => 0,
            Self::Skipped => 1,
            Self::Quarantined => 2,
            Self::Passed => 3,
            Self::Affected => 4,
            Self::Flaky => 5,
            Self::Failed => 6,
        }
    }
}

/// A result decides first; without one, quarantine, then the affected preview.
pub fn journey_state(outcome: Option<Outcome>, affected: bool, quarantined: bool) -> TestState {
    match outcome {
        Some(Outcome::Passed) => TestState::Passed,
        Some(Outcome::Failed) => TestState::Failed,
        Some(Outcome::Flaky) => TestState::Flaky,
        Some(Outcome::Skipped) => TestState::Skipped,
        None if quarantined => TestState::Quarantined,
        None if affected => TestState::Affected,
        None => TestState::NotRun,
    }
}

/// The worst state of an area's journeys (`NotRun` for none).
pub fn area_state(states: impl IntoIterator<Item = TestState>) -> TestState {
    states
        .into_iter()
        .max_by_key(|s| s.rank())
        .unwrap_or(TestState::NotRun)
}

/// System font files to try, best first. No font ships with the binary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontCandidates {
    pub ui: Vec<PathBuf>,
    pub semibold: Vec<PathBuf>,
    pub mono: Vec<PathBuf>,
}

pub fn font_candidates(
    windows: bool,
    windir: Option<&Path>,
    local_appdata: Option<&Path>,
) -> FontCandidates {
    if windows {
        let mut fonts = FontCandidates::default();
        let user = local_appdata.map(|d| d.join("Microsoft").join("Windows").join("Fonts"));
        let system = windir.map(|d| d.join("Fonts"));
        let mono_names = [
            "JetBrainsMono-Regular.ttf",
            "CascadiaMono.ttf",
            "consola.ttf",
        ];
        if let Some(system) = &system {
            fonts.ui.push(system.join("segoeui.ttf"));
            fonts.semibold.push(system.join("seguisb.ttf"));
            fonts.semibold.push(system.join("segoeuib.ttf"));
        }
        for name in mono_names {
            for dir in [&user, &system].into_iter().flatten() {
                fonts.mono.push(dir.join(name));
            }
        }
        return fonts;
    }
    let ui = [
        "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
        "/usr/share/fonts/opentype/inter/Inter-Regular.otf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    ];
    let semibold = [
        "/usr/share/fonts/truetype/inter/Inter-SemiBold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    ];
    let mono = [
        "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    ];
    let paths = |list: &[&str]| list.iter().map(PathBuf::from).collect();
    FontCandidates {
        ui: paths(&ui),
        semibold: paths(&semibold),
        mono: paths(&mono),
    }
}

/// The semibold family for titles; falls back to the body font.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, semibold())
}

fn first_readable(paths: &[PathBuf]) -> Option<Vec<u8>> {
    paths.iter().find_map(|p| std::fs::read(p).ok())
}

/// Installs the system UI and mono fonts in front of egui's defaults (which
/// stay as fallbacks for symbols).
pub fn install_fonts(ctx: &egui::Context) {
    let candidates = font_candidates(
        cfg!(windows),
        std::env::var_os("WINDIR").map(PathBuf::from).as_deref(),
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .as_deref(),
    );
    let mut fonts = FontDefinitions::default();
    let front = |fonts: &mut FontDefinitions, name: &str, family: FontFamily, data: Vec<u8>| {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_owned(data)));
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.into());
    };
    let ui = first_readable(&candidates.ui);
    if let Some(data) = ui {
        front(&mut fonts, "ui", FontFamily::Proportional, data);
    }
    if let Some(data) = first_readable(&candidates.mono) {
        front(&mut fonts, "mono", FontFamily::Monospace, data);
    }
    let proportional = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mut bold = proportional;
    if let Some(data) = first_readable(&candidates.semibold) {
        fonts
            .font_data
            .insert("semibold".into(), Arc::new(FontData::from_owned(data)));
        bold.insert(0, "semibold".into());
    }
    fonts.families.insert(semibold(), bold);
    ctx.set_fonts(fonts);
}

/// Applies the palette, sizes and spacing to the dark theme and selects it.
pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        style.text_styles = [
            (TextStyle::Small, font(12.0)),
            (TextStyle::Body, font(13.0)),
            (TextStyle::Button, font(13.0)),
            (TextStyle::Heading, semibold_font(16.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
        style.animation_time = 0.08;

        let s = &mut style.spacing;
        s.item_spacing = Vec2::new(6.0, 4.0);
        s.button_padding = Vec2::new(8.0, 3.0);
        s.interact_size = Vec2::new(24.0, 24.0);
        s.window_margin = Margin::same(12);
        s.menu_margin = Margin::same(5);
        s.indent = 16.0;
        s.icon_width = 14.0;
        s.icon_width_inner = 8.0;
        s.icon_spacing = 6.0;
        s.combo_height = 300.0;
        s.scroll = egui::style::ScrollStyle::thin();
        s.scroll.floating = true;

        let v = &mut style.visuals;
        v.dark_mode = true;
        v.override_text_color = None;
        v.panel_fill = GROUND;
        v.window_fill = POPUP;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(8);
        v.menu_corner_radius = CornerRadius::same(6);
        v.window_shadow = Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(90),
        };
        v.popup_shadow = Shadow {
            offset: [0, 3],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(80),
        };
        v.extreme_bg_color = ISLAND;
        v.text_edit_bg_color = Some(ISLAND);
        v.faint_bg_color = Color32::from_rgb(0x23, 0x24, 0x27);
        v.code_bg_color = Color32::from_rgb(0x26, 0x28, 0x2c);
        v.hyperlink_color = Color32::from_rgb(0x54, 0x8a, 0xf7);
        v.warn_fg_color = AT_RISK;
        v.error_fg_color = BROKEN;
        v.weak_text_color = Some(TEXT_SECONDARY);
        v.selection.bg_fill = SELECTION;
        v.selection.stroke = Stroke::new(1.0, TEXT);
        v.striped = false;
        v.indent_has_left_vline = false;
        v.collapsing_header_frame = false;
        v.button_frame = true;
        v.resize_corner_size = 10.0;
        v.clip_rect_margin = 3.0;
        v.handle_shape = egui::style::HandleShape::Circle;
        v.interact_cursor = Some(egui::CursorIcon::PointingHand);

        let radius = CornerRadius::same(4);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = GROUND;
        w.noninteractive.weak_bg_fill = GROUND;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = radius;
        for (state, fill, border) in [
            (&mut w.inactive, CONTROL, BORDER_STRONG),
            (&mut w.hovered, CONTROL_HOVER, BORDER_STRONG),
            (&mut w.active, CONTROL_ACTIVE, ACCENT),
            (&mut w.open, CONTROL_HOVER, BORDER_STRONG),
        ] {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::new(1.0, border);
            state.fg_stroke = Stroke::new(1.0, TEXT);
            state.corner_radius = radius;
            state.expansion = 0.0;
        }
        w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x43, 0x45, 0x4a));
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    });
}

/// An island: the rounded content surface on the ground.
pub fn island() -> egui::Frame {
    egui::Frame::new()
        .fill(ISLAND)
        .corner_radius(CornerRadius::same(ISLAND_RADIUS))
}

/// A thin rounded chip (counts, tags).
pub fn chip(color: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(5, 1))
}
