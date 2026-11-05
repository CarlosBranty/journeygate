//! The Flow view: the E2E flow diagram of `docs/flujo-e2e.html`, painted on
//! the egui painter in the Rider look. Cards on the three-lane grid, curved
//! wires with arrowheads, the legend, the mode selector, the walkthrough
//! (play, pause, step) and the inspector. Pan with a drag, zoom with the
//! wheel. While a guard run is active the local test path lights up live.
//!
//! It repaints on its own only while the walkthrough plays or the camera
//! glides; idle it costs nothing.

use super::GuardApp;
use super::shell::{island, island_panel_frame};
use super::widgets::{self, Icon};
use crate::commands::GuardCommand;
use crate::flow::{self, Edge, LegendMark, Live, LivePhase, Mode, Node, ShotSpec, Sim, Tone, Walk};
use crate::integrations::State;
use crate::theme;
use eframe::egui::{
    self, Align, Color32, CornerRadius, CursorIcon, FontId, Galley, Layout, Margin, Painter, Pos2,
    Rect, Response, RichText, Sense, Shadow, Shape, Stroke, StrokeKind, Ui, Vec2,
    epaint::TextShape, pos2, vec2,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

const INSPECTOR_W: f32 = 340.0;
/// Card fill: a step above the island, like Rider's popups.
const CARD: Color32 = Color32::from_rgb(0x27, 0x29, 0x2d);
const CANVAS: Color32 = Color32::from_rgb(0x1b, 0x1c, 0x1f);
const CODE: Color32 = Color32::from_rgb(0x19, 0x1a, 0x1d);
const FRAME_TIME: Duration = Duration::from_millis(16);
/// The ambient animation (a working lane, a local run) is cheaper: ~30 fps.
const AMBIENT_FRAME_TIME: Duration = Duration::from_millis(33);
/// The agent lane's wires while a task runs: each one moves while the node
/// it leads to works.
const LANE_EDGES: [&str; 5] = [
    "gate-qa",
    "qa-seal",
    "seal-coder",
    "skills-qa",
    "gateway-coder",
];
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 2.0;

/// Everything the Flow view remembers between frames.
pub(super) struct FlowState {
    mode: Mode,
    sim: Sim,
    selected: Option<&'static str>,
    walk: Option<Walk>,
    playing: bool,
    zoom: f32,
    /// Screen offset of the canvas origin from the canvas rect.
    pan: Vec2,
    camera_set: bool,
    /// How much wider than their base the columns are (set on fit).
    spread: f32,
    /// Fit the whole diagram next frame.
    fit_pending: bool,
    /// Where the camera glides to (following the walk).
    target: Option<Vec2>,
    /// Bring the current or selected node into view next frame.
    follow: bool,
    /// Jump instead of gliding (the screenshot harness).
    snap: bool,
    /// Seconds of animation, for the flowing dashes and the pulse.
    clock: f32,
    /// The harness's stand-in for a real run.
    live_override: Option<LivePhase>,
    /// The user hid the live overlay; a new run shows it again.
    pub(super) live_hidden: bool,
    /// The live phase last shown, to pan to its node when it changes.
    live_seen: Option<LivePhase>,
}

impl Default for FlowState {
    fn default() -> Self {
        Self {
            mode: Mode::Full,
            sim: Sim::default(),
            selected: None,
            walk: None,
            playing: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            camera_set: false,
            spread: 1.0,
            fit_pending: false,
            target: None,
            follow: false,
            snap: false,
            clock: 0.0,
            live_override: None,
            live_hidden: false,
            live_seen: None,
        }
    }
}

impl FlowState {
    /// The state `--flow` asks for: a mode, a selection, a paused step.
    pub(super) fn from_shot(spec: &ShotSpec) -> Self {
        let mut state = Self {
            mode: spec.mode,
            sim: spec.sim,
            selected: spec.select,
            live_override: spec.live,
            follow: spec.select.is_some() || spec.step.is_some(),
            snap: true,
            ..Self::default()
        };
        if let Some(step) = spec.step {
            let mut walk = Walk::start(spec.mode, spec.sim);
            for _ in 1..step {
                walk.advance(spec.sim);
            }
            // Catch the comet halfway along its wire.
            walk.tick(flow::COMET_SECS * 0.5, spec.sim);
            state.walk = Some(walk).filter(|w| !w.finished());
            state.selected = None;
        }
        state
    }

    fn stop_walk(&mut self) {
        self.walk = None;
        self.playing = false;
        self.target = None;
    }

    fn set_mode(&mut self, mode: Mode) {
        self.stop_walk();
        self.mode = mode;
        self.selected = None;
        self.live_hidden = true;
    }

    fn set_sim(&mut self, sim: Sim) {
        self.stop_walk();
        self.sim = sim;
    }

    fn toggle_play(&mut self) {
        match &self.walk {
            Some(walk) if !walk.finished() => self.playing = !self.playing,
            _ => {
                self.walk = Some(Walk::start(self.mode, self.sim));
                self.playing = true;
                self.selected = None;
                self.follow = true;
            }
        }
    }

    fn step(&mut self) {
        self.playing = false;
        match &mut self.walk {
            Some(walk) if !walk.finished() => {
                if !walk.advance(self.sim) {
                    self.stop_walk();
                }
            }
            _ => {
                self.walk = Some(Walk::start(self.mode, self.sim));
                self.selected = None;
            }
        }
        self.follow = true;
    }

    fn select(&mut self, id: Option<&'static str>) {
        if self.walk.is_some() {
            self.stop_walk();
        }
        self.selected = if self.selected == id { None } else { id };
    }

    /// Advances the walk and the clock by `dt` seconds; `ambient` keeps the
    /// clock (the flowing dashes and the pulse) going without a walk.
    fn update(&mut self, dt: f32, ambient: bool) {
        if ambient && !self.playing {
            self.clock += dt;
            return;
        }
        if !self.playing {
            return;
        }
        self.clock += dt;
        let sim = self.sim;
        if let Some(walk) = &mut self.walk
            && walk.tick(dt, sim)
        {
            if walk.finished() {
                self.stop_walk();
            } else {
                self.follow = true;
            }
        }
    }

    fn animating(&self) -> bool {
        self.playing || self.target.is_some()
    }
}

/// What the diagram shows this frame.
struct Scene {
    nodes: BTreeSet<&'static str>,
    edges: Vec<&'static Edge>,
    focus: Option<BTreeSet<&'static str>>,
    current: Option<&'static str>,
    live: Option<(LivePhase, Live)>,
    /// Nodes with work going on now (a running agent task, a local run).
    busy: BTreeSet<&'static str>,
}

/// What the integrations and the runner add to the diagram this frame.
#[derive(Default)]
struct Ambient {
    /// The integration state badged on each node.
    badges: BTreeMap<&'static str, State>,
    /// The agent lane's nodes whose role has a running task.
    lane: BTreeSet<&'static str>,
    /// A local guard up / affected / test is running.
    local_running: bool,
    /// The running test's progress: fraction and summary.
    progress: Option<(Option<f32>, String)>,
    /// Animate (false with reduced motion).
    motion: bool,
}

impl Scene {
    fn build(state: &FlowState, live: Option<LivePhase>, ambient: &Ambient) -> Self {
        let live = live
            .filter(|_| state.walk.is_none() && !state.live_hidden)
            .map(|phase| (phase, flow::live(phase)));
        let (nodes, edges, current) = if let Some(walk) = &state.walk {
            let nodes = flow::active_nodes(walk.mode, state.sim, Some(walk));
            let edges = flow::active_edges(&nodes, state.sim, Some(walk));
            let current = Some(walk.current()).filter(|c| !c.is_empty());
            (nodes, edges, current)
        } else if let Some((_, live)) = &live {
            let nodes: BTreeSet<&'static str> = live.lit.iter().copied().collect();
            let edges = flow::EDGES
                .iter()
                .filter(|e| nodes.contains(e.from) && nodes.contains(e.to) && !e.feedback && !e.sat)
                .collect();
            (nodes, edges, live.current)
        } else {
            let nodes = flow::active_nodes(state.mode, state.sim, None);
            let edges = flow::active_edges(&nodes, state.sim, None);
            (nodes, edges, None)
        };
        let (mut nodes, mut edges) = (nodes, edges);
        let mut busy = BTreeSet::new();
        if state.walk.is_none() {
            nodes.extend(ambient.lane.iter().copied());
            busy.extend(ambient.lane.iter().copied());
            for id in LANE_EDGES {
                let Some(e) = flow::edge(id).filter(|e| ambient.lane.contains(e.to)) else {
                    continue;
                };
                // Skills feed the QA agent, the gateway the coder.
                if matches!(e.from, "skills" | "gateway") {
                    nodes.insert(e.from);
                }
                if nodes.contains(e.from) && !edges.iter().any(|x| x.id == id) {
                    edges.push(e);
                }
            }
            if ambient.local_running {
                busy.insert("local");
            }
        }
        let focus = state
            .selected
            .filter(|_| state.walk.is_none())
            .map(|id| flow::lineage(id, &edges));
        Self {
            nodes,
            edges,
            focus,
            current,
            live,
            busy,
        }
    }

    fn lit(&self, id: &str, selected: Option<&str>) -> bool {
        (self.nodes.contains(id) && self.focus.as_ref().is_none_or(|f| f.contains(id)))
            || selected == Some(id)
    }

    fn edge_shown(&self, e: &Edge) -> bool {
        self.focus
            .as_ref()
            .is_none_or(|f| f.contains(e.from) && f.contains(e.to))
    }
}

fn tone_color(tone: Tone) -> Option<Color32> {
    match tone {
        Tone::Plain => None,
        Tone::Ok => Some(theme::VERIFIED),
        Tone::Warn => Some(theme::AT_RISK),
        Tone::Err => Some(theme::BROKEN),
    }
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn white(alpha: f32) -> Color32 {
    Color32::from_white_alpha((alpha * 255.0).round() as u8)
}

fn with_alpha(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha * 255.0).round() as u8)
}

/// The text blocks of a card at zoom `z`.
struct CardText {
    stage: Arc<Galley>,
    title: Arc<Galley>,
    sub: Arc<Galley>,
    chips: Vec<Arc<Galley>>,
}

const CARD_PAD_X: f32 = 14.0;
const CARD_PAD_T: f32 = 12.0;
const CARD_PAD_B: f32 = 13.0;
const CARD_GAP: f32 = 4.0;
const CHIP_PAD: Vec2 = vec2(8.0, 5.0);

/// Lays out a card's text for a card `width` canvas units wide at zoom
/// `z`; text sizes keep a readable floor (see [`flow::text_px`]).
fn card_text(p: &Painter, node: &Node, width: f32, z: f32) -> CardText {
    let wrap = (width - 2.0 * CARD_PAD_X) * z;
    CardText {
        stage: p.layout_no_wrap(
            node.stage.to_uppercase(),
            FontId::monospace(flow::text_px(10.0, 10.0, z)),
            theme::TEXT_SECONDARY,
        ),
        title: p.layout(
            node.title.to_string(),
            theme::semibold_font(flow::text_px(14.0, 12.5, z)),
            theme::TEXT,
            wrap,
        ),
        sub: p.layout(
            node.sub.to_string(),
            theme::font(flow::text_px(12.0, 11.0, z)),
            theme::TEXT_SECONDARY,
            wrap,
        ),
        chips: node
            .chips
            .iter()
            .map(|c| {
                p.layout_no_wrap(
                    (*c).to_string(),
                    FontId::monospace(flow::text_px(10.5, 10.0, z)),
                    theme::TEXT_SECONDARY,
                )
            })
            .collect(),
    }
}

/// Chip padding follows the chip text, which keeps a size floor.
fn chip_z(z: f32) -> f32 {
    z.max(0.7)
}

fn card_height(text: &CardText, z: f32) -> f32 {
    let mut h = (CARD_PAD_T + CARD_PAD_B + 2.0 * CARD_GAP) * z
        + text.stage.size().y
        + text.title.size().y
        + text.sub.size().y;
    if let Some(chip) = text.chips.first() {
        h += 6.0 * z + chip.size().y + 2.0 * CHIP_PAD.y * chip_z(z);
    }
    h
}

/// The outline of a rounded rect, for dashed borders.
fn rounded_outline(r: Rect, radius: f32) -> Vec<Pos2> {
    let mut pts = Vec::with_capacity(4 * 7 + 1);
    let corners = [
        (pos2(r.right() - radius, r.top() + radius), -90.0f32),
        (pos2(r.right() - radius, r.bottom() - radius), 0.0),
        (pos2(r.left() + radius, r.bottom() - radius), 90.0),
        (pos2(r.left() + radius, r.top() + radius), 180.0),
    ];
    for (c, start) in corners {
        for i in 0..=6 {
            let a = (start + 15.0 * i as f32).to_radians();
            pts.push(c + vec2(a.cos(), a.sin()) * radius);
        }
    }
    pts.push(pts[0]);
    pts
}

fn arrowhead(p: &Painter, tip: Pos2, from: Pos2, size: f32, color: Color32) {
    let dir = (tip - from).normalized();
    if !dir.is_finite() {
        return;
    }
    let side = dir.rot90() * size * 0.5;
    let back = tip - dir * size;
    p.add(Shape::convex_polygon(
        vec![tip, back + side, back - side],
        color,
        Stroke::NONE,
    ));
}

/// A segmented control in Rider's look; returns the clicked option.
fn segmented<T: Copy + PartialEq>(ui: &mut Ui, options: &[(T, &str)], current: T) -> Option<T> {
    let mut picked = None;
    egui::Frame::new()
        .fill(theme::GROUND)
        .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
        .corner_radius(CornerRadius::same(7))
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (value, label) in options {
                    let on = *value == current;
                    let galley = ui.painter().layout_no_wrap(
                        (*label).to_string(),
                        theme::font(12.5),
                        theme::TEXT,
                    );
                    let size = vec2(galley.size().x + 20.0, 24.0);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    let p = ui.painter();
                    if on {
                        p.rect_filled(rect, CornerRadius::same(5), theme::CONTROL_ACTIVE);
                        p.rect_stroke(
                            rect,
                            CornerRadius::same(5),
                            Stroke::new(1.0, Color32::from_rgb(0x5a, 0x5d, 0x63)),
                            StrokeKind::Inside,
                        );
                    } else if response.hovered() {
                        p.rect_filled(rect, CornerRadius::same(5), theme::CONTROL_HOVER);
                    }
                    let color = if on || response.hovered() {
                        theme::TEXT
                    } else {
                        theme::TEXT_SECONDARY
                    };
                    p.galley_with_override_text_color(
                        rect.center() - galley.size() / 2.0,
                        galley,
                        color,
                    );
                    if response.clicked() {
                        picked = Some(*value);
                    }
                }
            });
        });
    picked
}

/// A small switch with its label; the track takes `tone` when on.
fn switch(ui: &mut Ui, label: &str, on: bool, tone: Color32) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), theme::font(12.5), theme::TEXT);
    let size = vec2(26.0 + 8.0 + galley.size().x + 16.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if response.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), theme::CONTROL_HOVER);
    }
    let track = Rect::from_min_size(
        pos2(rect.left() + 8.0, rect.center().y - 7.5),
        vec2(26.0, 15.0),
    );
    let fill = if on {
        tone.gamma_multiply(0.55)
    } else {
        theme::BORDER_STRONG
    };
    p.rect_filled(track, CornerRadius::same(8), fill);
    let knob_x = if on {
        track.right() - 7.5
    } else {
        track.left() + 7.5
    };
    let knob = if on {
        Color32::WHITE
    } else {
        theme::TEXT_SECONDARY
    };
    p.circle_filled(pos2(knob_x, track.center().y), 5.5, knob);
    let color = if on {
        theme::TEXT
    } else {
        theme::TEXT_SECONDARY
    };
    p.galley_with_override_text_color(
        pos2(track.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response
}

/// A flat icon-and-label button for the view's toolbar.
fn tool_button(ui: &mut Ui, icon: Icon, label: &str, color: Color32, tip: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), theme::font(12.5), theme::TEXT);
    let size = vec2(galley.size().x + 36.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if response.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), theme::CONTROL_HOVER);
    }
    widgets::paint_icon(
        p,
        Rect::from_center_size(pos2(rect.left() + 15.0, rect.center().y), Vec2::splat(14.0)),
        icon,
        color,
    );
    p.galley(
        pos2(rect.left() + 27.0, rect.center().y - galley.size().y / 2.0),
        galley,
        theme::TEXT,
    );
    response.on_hover_text(tip)
}

fn legend_mark(p: &Painter, a: Pos2, b: Pos2, mark: LegendMark) {
    let stroke = |c| Stroke::new(1.5, c);
    match mark {
        LegendMark::Flow => {
            p.line_segment([a, b], stroke(white(0.75)));
        }
        LegendMark::Err => {
            p.line_segment([a, b], stroke(with_alpha(theme::BROKEN, 0.85)));
        }
        LegendMark::Warn => {
            p.line_segment([a, b], stroke(with_alpha(theme::AT_RISK, 0.8)));
        }
        LegendMark::Sat => {
            p.extend(Shape::dashed_line(&[a, b], stroke(white(0.45)), 1.5, 3.5));
        }
    }
}

impl GuardApp {
    /// The live phase of the local test path, from the runner state.
    fn flow_live(&self) -> Option<LivePhase> {
        if let Some(phase) = self.flow.live_override {
            return Some(phase);
        }
        let running = self.run.as_ref().map(|active| match active.command {
            GuardCommand::Up { .. } => flow::RunKind::Up,
            ref command if command.is_test() => flow::RunKind::Test,
            _ => flow::RunKind::Other,
        });
        let selecting = self.gate_affected.is_some()
            || self
                .probe
                .as_ref()
                .is_some_and(|p| p.kind == super::ProbeKind::Affected);
        flow::live_phase(running, selecting, self.last_test)
    }

    /// Records the verdict of a finished test run (after its report reload).
    /// A stale report (`fresh` false) counts no failure: its results belong
    /// to an earlier run.
    pub(super) fn record_test_verdict(&mut self, exit: crate::runner::Exit, fresh: bool) {
        let (failed, flaky) = crate::results::run_counts(fresh, &self.results);
        self.last_test = Some(flow::verdict(
            exit.success(),
            exit == crate::runner::Exit::Cancelled,
            failed,
            flaky,
        ));
    }

    /// The integrations' badges and the local run, for this frame.
    fn flow_ambient(&self, live: Option<LivePhase>) -> Ambient {
        let report = self.integration_report();
        let mut badges: BTreeMap<&'static str, State> = flow::NODES
            .iter()
            .filter_map(|n| report?.node_state(n.id).map(|s| (n.id, s)))
            .collect();
        let local_running = matches!(
            live,
            Some(LivePhase::Up | LivePhase::Selecting | LivePhase::Testing)
        );
        if local_running {
            // The local run is the work going on there now.
            badges.insert("local", State::Working);
        }
        let progress = self
            .run_progress
            .summary()
            .map(|s| (self.run_progress.fraction(), s))
            .filter(|_| matches!(live, Some(LivePhase::Testing)));
        Ambient {
            badges,
            lane: report.map(|r| r.working_lane_nodes()).unwrap_or_default(),
            local_running,
            progress,
            motion: !self.reduced_motion,
        }
    }

    pub(super) fn flow_screen(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.unstable_dt).min(0.1);
        let live = self.flow_live();
        let ambient = self.flow_ambient(live);
        let moving = ambient.motion && (!ambient.lane.is_empty() || ambient.local_running);
        self.flow.update(dt, moving);
        if live != self.flow.live_seen {
            self.flow.live_seen = live;
            self.flow.follow = live.is_some();
        }
        egui::CentralPanel::default()
            .frame(island_panel_frame())
            .show(ctx, |ui| {
                island(ui, Margin::same(14), |ui| {
                    self.flow_header(ui);
                    ui.add_space(8.0);
                    let progress = ambient.progress.as_ref().map(|p| p.1.clone());
                    self.flow_legend(ui, live, progress.as_deref());
                    ui.add_space(10.0);
                    let scene = Scene::build(&self.flow, live, &ambient);
                    let avail = ui.available_rect_before_wrap();
                    let ins_w = INSPECTOR_W.min(avail.width() * 0.4);
                    let canvas = Rect::from_min_max(
                        avail.min,
                        pos2(avail.max.x - ins_w - 12.0, avail.max.y),
                    );
                    let inspector =
                        Rect::from_min_max(pos2(avail.max.x - ins_w, avail.min.y), avail.max);
                    self.flow_canvas(ui, canvas, &scene, &ambient, dt);
                    ui.scope_builder(egui::UiBuilder::new().max_rect(inspector), |ui| {
                        self.flow_inspector(ui, &scene, progress.as_deref());
                    });
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) && self.pending.is_none() {
            self.flow.stop_walk();
            self.flow.selected = None;
        }
        if self.flow.animating() {
            ctx.request_repaint_after(FRAME_TIME);
        } else if moving {
            ctx.request_repaint_after(AMBIENT_FRAME_TIME);
        }
    }

    fn flow_header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Flujo Guard E2E")
                    .font(theme::semibold_font(15.0))
                    .color(theme::TEXT),
            );
            ui.add_space(4.0);
            egui::Frame::new()
                .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::symmetric(8, 2))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Rust · Jev 1.13 · Playwright")
                            .font(FontId::monospace(11.0))
                            .color(theme::TEXT_SECONDARY),
                    );
                });
            ui.add_space(6.0);
            self.integrations_button(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let fit =
                    widgets::icon_button(ui, Icon::Fit, theme::TEXT, true, "Ajustar a la vista");
                if fit.clicked() {
                    self.flow.target = None;
                    self.flow.fit_pending = true;
                }
                ui.label(
                    RichText::new(format!("{:.0}%", self.flow.zoom * 100.0))
                        .font(FontId::monospace(11.5))
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(6.0);
                let walking = self.flow.walk.is_some();
                if walking
                    && tool_button(
                        ui,
                        Icon::Stop,
                        "Stop",
                        theme::STOP_RED,
                        "Terminar el recorrido (Esc)",
                    )
                    .clicked()
                {
                    self.flow.stop_walk();
                }
                if tool_button(
                    ui,
                    Icon::StepForward,
                    "Step",
                    theme::TEXT,
                    "Avanzar un paso",
                )
                .clicked()
                {
                    self.flow.step();
                }
                if self.flow.playing {
                    if tool_button(ui, Icon::Pause, "Pause", theme::TEXT, "Pausar el recorrido")
                        .clicked()
                    {
                        self.flow.toggle_play();
                    }
                } else {
                    let tip = if walking {
                        "Reanudar el recorrido"
                    } else {
                        "Recorrer paso a paso"
                    };
                    if widgets::run_button(ui, "Play", true)
                        .on_hover_text(tip)
                        .clicked()
                    {
                        self.flow.toggle_play();
                    }
                }
                ui.add_space(8.0);
                let sim = self.flow.sim;
                if switch(ui, "Simular inestable", sim.flaky, theme::AT_RISK).clicked() {
                    self.flow.set_sim(Sim {
                        flaky: !sim.flaky,
                        ..sim
                    });
                }
                if switch(ui, "Simular regresión", sim.reg, theme::BROKEN).clicked() {
                    self.flow.set_sim(Sim {
                        reg: !sim.reg,
                        ..sim
                    });
                }
                ui.add_space(8.0);
                let options: Vec<(Mode, &str)> =
                    Mode::ALL.iter().map(|m| (*m, m.label())).collect();
                // Right-to-left: lay the segments out in reading order.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if let Some(mode) = segmented(ui, &options, self.flow.mode) {
                        self.flow.set_mode(mode);
                    }
                });
            });
        });
    }

    fn flow_legend(&mut self, ui: &mut Ui, live: Option<LivePhase>, progress: Option<&str>) {
        ui.horizontal(|ui| {
            ui.set_min_height(24.0);
            for (label, mark) in flow::LEGEND {
                let (r, _) = ui.allocate_exact_size(vec2(22.0, 14.0), Sense::hover());
                legend_mark(
                    ui.painter(),
                    pos2(r.left(), r.center().y),
                    pos2(r.right(), r.center().y),
                    mark,
                );
                ui.label(
                    RichText::new(label)
                        .font(FontId::monospace(11.5))
                        .color(theme::TEXT_SECONDARY),
                );
                ui.add_space(14.0);
            }
            let Some(phase) = live else {
                return;
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if self.flow.live_hidden || self.flow.walk.is_some() {
                    if widgets::flat_button(ui, "Mostrar corrida en vivo", true).clicked() {
                        self.flow.stop_walk();
                        self.flow.live_hidden = false;
                    }
                    return;
                }
                if widgets::icon_button(
                    ui,
                    Icon::Close,
                    theme::TEXT_SECONDARY,
                    true,
                    "Ocultar corrida en vivo",
                )
                .clicked()
                {
                    self.flow.live_hidden = true;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    live_strip(ui, phase, progress);
                });
            });
        });
    }

    fn flow_canvas(&mut self, ui: &mut Ui, rect: Rect, scene: &Scene, ambient: &Ambient, dt: f32) {
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let p = ui.painter_at(rect);
        // Card sizes depend on the zoom (text floors) and the spread.
        let measure = |s: f32, z: f32| {
            flow::layout_spread(s, |n, w| card_height(&card_text(&p, n, w, z), z) / z)
        };
        let st = &mut self.flow;

        let fit = std::mem::take(&mut st.fit_pending);
        if fit || !st.camera_set {
            // Fit the whole diagram, outcome row included, and let the
            // cards widen into the spare width. Zoom, spread and card
            // heights depend on each other; a few rounds settle them.
            let fit_zoom = |l: &flow::Layout| {
                (rect.width() / l.size.x)
                    .min(rect.height() / l.size.y)
                    .clamp(MIN_ZOOM, 1.0)
            };
            let (mut s, mut z) = (1.0, 1.0);
            for _ in 0..3 {
                z = fit_zoom(&measure(s, z));
                s = flow::spread(rect.width() / z);
            }
            let l = measure(s, z);
            z = fit_zoom(&l);
            st.zoom = z;
            st.spread = s;
            st.pan = vec2(
                (rect.width() - l.size.x * z) / 2.0,
                ((rect.height() - l.size.y * z) / 2.0).max(0.0),
            );
            st.camera_set = true;
        }
        let layout = measure(st.spread, st.zoom);

        // Pan and zoom.
        if response.dragged() {
            st.pan += response.drag_delta();
            st.target = None;
        }
        if response.hovered() {
            let (scroll, pinch, pointer) =
                ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
            let factor = pinch * (scroll.y * 0.0015).exp();
            if (factor - 1.0).abs() > 1e-4
                && let Some(pointer) = pointer
            {
                let new_zoom = (st.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
                let anchor = (pointer - rect.min - st.pan) / st.zoom;
                st.pan = pointer - rect.min - anchor * new_zoom;
                st.zoom = new_zoom;
                st.target = None;
            }
        }

        // Follow the walk (or the harness's selection) into view.
        let focus_id = scene.current.or(st.selected);
        if std::mem::take(&mut st.follow)
            && let Some(node_rect) = focus_id.map(|id| layout.rects[id])
        {
            let screen = Rect::from_min_size(
                rect.min + st.pan + node_rect.min.to_vec2() * st.zoom,
                node_rect.size() * st.zoom,
            );
            let inner = rect.shrink(12.0);
            if !inner.contains_rect(screen) {
                let mut target = st.pan;
                target.y = rect.height() / 2.0 - node_rect.center().y * st.zoom;
                if screen.left() < inner.left() || screen.right() > inner.right() {
                    target.x = rect.width() / 2.0 - node_rect.center().x * st.zoom;
                }
                // Never pan past the diagram's edges into an empty band.
                let target = flow::clamp_pan(target, layout.size * st.zoom, rect.size());
                if std::mem::take(&mut st.snap) {
                    st.pan = target;
                } else {
                    st.target = Some(target);
                }
            }
        }
        if let Some(target) = st.target {
            let k = (dt * 7.0).min(1.0);
            st.pan += (target - st.pan) * k;
            if (target - st.pan).length() < 0.5 {
                st.pan = target;
                st.target = None;
            }
        }

        let z = st.zoom;
        let origin = rect.min + st.pan;
        let to_screen = |q: Pos2| origin + q.to_vec2() * z;

        // Ground: a darker well with the dot grid.
        p.rect_filled(rect, CornerRadius::same(8), CANVAS);
        let spacing = 22.0 * z;
        if spacing >= 9.0 {
            let dot = white(0.05);
            let start_x = origin.x + ((rect.left() - origin.x) / spacing).ceil() * spacing;
            let start_y = origin.y + ((rect.top() - origin.y) / spacing).ceil() * spacing;
            let mut y = start_y;
            while y < rect.bottom() {
                let mut x = start_x;
                while x < rect.right() {
                    p.circle_filled(pos2(x, y), 1.0, dot);
                    x += spacing;
                }
                y += spacing;
            }
        }

        // Hover and click.
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|_| response.hovered());
        let hit = |pos: Pos2| {
            flow::POS.iter().map(|(id, _, _)| *id).find(|id| {
                let r = layout.rects[id];
                Rect::from_min_size(to_screen(r.min), r.size() * z).contains(pos)
            })
        };
        let hovered = pointer.and_then(hit);
        if hovered.is_some() && !response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        } else if response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        let clicked = response
            .clicked()
            .then(|| response.interact_pointer_pos())
            .flatten();

        // Wires: faint base for every route, then the active flow.
        let width = 1.5 * z.max(0.7);
        let paths: Vec<(&Edge, Vec<Pos2>)> = flow::EDGES
            .iter()
            .map(|e| {
                let pts = flow::edge_points(e, &layout.rects);
                (e, pts.into_iter().map(to_screen).collect())
            })
            .collect();
        for (e, pts) in paths.iter().filter(|(e, _)| !e.feedback) {
            let stroke = Stroke::new(width, white(0.075));
            if e.sat {
                p.extend(Shape::dashed_line(pts, stroke, 2.0 * z, 5.0 * z));
            } else {
                p.add(Shape::line(pts.clone(), stroke));
            }
        }
        let dash = 12.0 * z;
        let offset = dash - (st.clock * 24.0 / 1.1 * z) % dash;
        for (e, pts) in paths
            .iter()
            .filter(|(e, _)| scene.edges.iter().any(|a| a.id == e.id) && scene.edge_shown(e))
        {
            let tone = tone_color(e.tone);
            let (line, glow, head) = match tone {
                Some(c) => (with_alpha(c, 0.7), with_alpha(c, 0.1), with_alpha(c, 0.85)),
                None if e.sat => (white(0.26), Color32::TRANSPARENT, white(0.4)),
                None => (white(0.4), white(0.06), white(0.55)),
            };
            if !e.sat {
                p.add(Shape::line(pts.clone(), Stroke::new(6.0 * z, glow)));
            }
            let (dashes, gaps, off) = if e.sat {
                (1.5 * z, 5.0 * z, offset * 0.4)
            } else {
                (5.0 * z, 7.0 * z, offset)
            };
            p.extend(Shape::dashed_line_with_offset(
                pts,
                Stroke::new(width, line),
                &[dashes],
                &[gaps],
                off,
            ));
            let n = pts.len();
            arrowhead(&p, pts[n - 1], pts[n - 2], 9.0 * z, head);
            if e.side {
                arrowhead(&p, pts[0], pts[1], 9.0 * z, head);
                let mid_y = (layout.rects[e.from].center().y + layout.rects[e.to].center().y) / 2.0;
                let galley = p.layout_no_wrap(
                    flow::SIDE_LABEL.to_string(),
                    FontId::monospace(flow::text_px(10.5, 10.0, z)),
                    theme::TEXT_DISABLED,
                );
                let at = to_screen(pos2(24.0, mid_y));
                let pos = at + vec2(-galley.size().y / 2.0, galley.size().x / 2.0);
                p.add(
                    TextShape::new(pos, galley, theme::TEXT_DISABLED)
                        .with_angle(-std::f32::consts::FRAC_PI_2),
                );
            }
        }

        // Cards.
        let pulse = if st.playing || (ambient.motion && !scene.busy.is_empty()) {
            0.5 + 0.5 * (st.clock * std::f32::consts::TAU / 1.8).sin()
        } else {
            0.5
        };
        for node in &flow::NODES {
            let r = layout.rects[node.id];
            let screen = Rect::from_min_size(to_screen(r.min), r.size() * z);
            if !rect.intersects(screen.expand(40.0 * z)) {
                continue;
            }
            let busy = scene.busy.contains(node.id);
            let look = CardLook {
                lit: scene.lit(node.id, st.selected) || busy,
                selected: st.selected == Some(node.id),
                current: scene.current == Some(node.id) || busy,
                hovered: hovered == Some(node.id),
                pulse,
            };
            paint_card(&p, node, screen, z, look);
            if node.id == "local"
                && let Some((fraction, summary)) = &ambient.progress
            {
                paint_progress(&p, screen, z, *fraction, summary, st.clock, ambient.motion);
            }
            if let Some(state) = ambient.badges.get(node.id) {
                paint_badge(&p, screen, z, *state, pulse);
            }
        }

        // The comet travelling into the current step.
        if let Some(walk) = &st.walk
            && let Some((edge_id, tone)) = walk.comet()
            && walk.comet_progress() < 1.0
            && let Some((_, pts)) = paths.iter().find(|(e, _)| e.id == edge_id)
        {
            let at = flow::point_at(pts, walk.comet_progress());
            let color = tone_color(tone).map_or(Color32::WHITE, |c| mix(c, Color32::WHITE, 0.6));
            p.circle_filled(at, 9.0 * z, with_alpha(color, 0.18));
            p.circle_filled(at, 4.0 * z.max(0.6), color);
        }

        // Step badge over the canvas while walking.
        if let Some(walk) = &st.walk {
            let (i, n) = walk.position();
            let text = format!("Paso {i} de {n}");
            let galley = p.layout_no_wrap(text, FontId::monospace(11.5), theme::TEXT);
            let badge = Rect::from_min_size(
                rect.min + vec2(12.0, 12.0),
                galley.size() + vec2(20.0, 10.0),
            );
            p.rect_filled(badge, CornerRadius::same(6), theme::POPUP);
            p.rect_stroke(
                badge,
                CornerRadius::same(6),
                Stroke::new(1.0, theme::BORDER_STRONG),
                StrokeKind::Inside,
            );
            p.galley(badge.min + vec2(10.0, 5.0), galley, theme::TEXT);
        }

        p.rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0, theme::BORDER),
            StrokeKind::Inside,
        );

        if let Some(pos) = clicked {
            match hit(pos) {
                Some(id) => st.select(Some(id)),
                None => st.selected = None,
            }
        }
    }

    /// The ↻ of the header: refreshes the integrations, with their age.
    fn integrations_button(&mut self, ui: &mut Ui) {
        let busy = self.integrations_job.is_some();
        let label = if busy {
            "Integraciones…".to_string()
        } else {
            match self.integrations_at {
                Some(at) => format!("Integraciones · hace {}", age(at.elapsed())),
                None => "Integraciones".to_string(),
            }
        };
        let color = match &self.integrations {
            Some(Err(_)) => theme::BROKEN,
            _ => theme::TEXT,
        };
        let tip = match &self.integrations {
            Some(Err(e)) => format!("No se pudo consultar: {e}"),
            _ => "Volver a consultar el estado de Jira, guard serve, agentes, gateway y CI (cada 30 s mientras esta vista está abierta)".to_string(),
        };
        if tool_button(ui, Icon::Refresh, &label, color, &tip).clicked() && !busy {
            self.refresh_integrations(ui.ctx());
        }
    }

    fn flow_inspector(&mut self, ui: &mut Ui, scene: &Scene, progress: Option<&str>) {
        let st = &self.flow;
        let walk_node = st
            .walk
            .as_ref()
            .filter(|w| !w.finished())
            .and_then(|w| flow::node(w.current()).map(|n| (n, w.position())));
        let shown = walk_node
            .map(|(n, _)| n)
            .or_else(|| st.selected.and_then(flow::node));
        // The integration panel belongs to a clicked node, not to the walk.
        let panel = shown.filter(|_| walk_node.is_none()).map(|n| n.id);
        let (mode, sim) = (st.mode, st.sim);
        let project = self.project.as_ref().map(|p| p.name.clone());
        let tone = match (shown, &scene.live) {
            (Some(n), _) => n.tone,
            (None, Some((_, live))) => live.tone,
            (None, None) if st.sim.reg => Tone::Err,
            _ => Tone::Plain,
        };
        let border = tone_color(tone).map_or(theme::BORDER, |c| with_alpha(c, 0.45));
        egui::Frame::new()
            .fill(CARD)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(16))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        if let Some(node) = shown {
                            if let Some(id) = panel {
                                self.integration_panel(ui, id);
                            }
                            let step = walk_node.map(|(_, (i, n))| format!(" · Paso {i} de {n}"));
                            node_details(ui, node, step.as_deref().unwrap_or(""));
                        } else if let Some((phase, live)) = &scene.live {
                            live_details(ui, *phase, live, project.as_deref(), progress);
                        } else {
                            mode_details(ui, mode, sim);
                        }
                        ui.add_space(14.0);
                        ui.label(
                            RichText::new(flow::FOOT)
                                .font(FontId::monospace(10.5))
                                .color(theme::TEXT_DISABLED),
                        );
                    });
            });
    }
}

#[derive(Clone, Copy)]
struct CardLook {
    lit: bool,
    selected: bool,
    current: bool,
    hovered: bool,
    pulse: f32,
}

fn paint_card(p: &Painter, node: &Node, r: Rect, z: f32, look: CardLook) {
    let radius = CornerRadius::same((10.0 * z).round().clamp(2.0, 20.0) as u8);
    let a: f32 = if look.lit { 1.0 } else { 0.26 };
    let tone = tone_color(node.tone).filter(|_| look.lit);

    // Glow first, under the card.
    let glow = if look.current {
        Some(white(0.10 + 0.08 * look.pulse))
    } else if look.selected {
        Some(with_alpha(theme::ACCENT, 0.28))
    } else {
        tone.map(|c| with_alpha(c, 0.16))
    };
    if let Some(color) = glow {
        let spread = if look.current {
            (2.0 + 3.0 * look.pulse) * z
        } else {
            0.0
        };
        p.add(
            Shadow {
                offset: [0, 0],
                blur: (26.0 * z).round().clamp(4.0, 60.0) as u8,
                spread: spread.round() as u8,
                color,
            }
            .as_shape(r, radius),
        );
    }

    if !node.sat {
        let fill = match tone {
            Some(c) => mix(CARD, c, 0.07),
            None => CARD,
        };
        p.rect_filled(r, radius, fill.gamma_multiply(a.max(0.55)));
    }
    let border = if look.selected {
        theme::ACCENT
    } else if look.current {
        white(0.72)
    } else if let Some(c) = tone {
        with_alpha(c, 0.6)
    } else if look.hovered {
        Color32::from_rgb(0x6f, 0x73, 0x7a)
    } else if look.lit {
        Color32::from_rgb(0x52, 0x55, 0x5b)
    } else {
        theme::BORDER
    };
    let width = if look.selected || look.current {
        1.5
    } else {
        1.0
    };
    let stroke = Stroke::new(
        width,
        border.gamma_multiply(if look.lit { 1.0 } else { 0.8 }),
    );
    if node.sat {
        let outline = rounded_outline(r, f32::from(radius.nw));
        p.extend(Shape::dashed_line(&outline, stroke, 5.0 * z, 4.0 * z));
    } else {
        p.rect_stroke(r, radius, stroke, StrokeKind::Inside);
    }

    if z < 0.3 {
        return;
    }
    let text = card_text(p, node, r.width() / z, z);
    let fade = |c: Color32| c.gamma_multiply(a);
    let mut y = r.top() + CARD_PAD_T * z;
    let x = r.left() + CARD_PAD_X * z;
    let stage_color = tone.map_or(theme::TEXT_SECONDARY, |c| {
        mix(theme::TEXT_SECONDARY, c, 0.6)
    });
    let h = text.stage.size().y;
    p.galley_with_override_text_color(pos2(x, y), text.stage, fade(stage_color));
    y += h + CARD_GAP * z;
    let h = text.title.size().y;
    p.galley_with_override_text_color(pos2(x, y), text.title, fade(theme::TEXT));
    y += h + CARD_GAP * z;
    let h = text.sub.size().y;
    p.galley_with_override_text_color(pos2(x, y), text.sub, fade(theme::TEXT_SECONDARY));
    y += h + 6.0 * z;
    let mut cx = x;
    for chip in text.chips {
        let size = chip.size() + CHIP_PAD * 2.0 * chip_z(z);
        let pill = Rect::from_min_size(pos2(cx, y), size);
        p.rect_stroke(
            pill,
            CornerRadius::same((size.y / 2.0) as u8),
            Stroke::new(1.0, fade(theme::BORDER_STRONG)),
            StrokeKind::Inside,
        );
        p.galley_with_override_text_color(
            pill.min + CHIP_PAD * chip_z(z),
            chip,
            fade(theme::TEXT_SECONDARY),
        );
        cx += size.x + 6.0 * chip_z(z);
    }
}

fn kicker(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .font(FontId::monospace(10.5))
            .color(theme::TEXT_SECONDARY),
    );
}

fn title(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::semibold_font(18.0))
            .color(theme::TEXT),
    );
}

fn note(ui: &mut Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .stroke(Stroke::new(1.0, with_alpha(color, 0.45)))
        .fill(with_alpha(color, 0.06))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).size(12.5).color(theme::TEXT_SECONDARY));
        });
}

fn node_details(ui: &mut Ui, node: &Node, step: &str) {
    kicker(ui, &format!("{}{step}", node.stage));
    title(ui, node.title);
    ui.label(
        RichText::new(node.sub)
            .size(13.0)
            .color(theme::TEXT_SECONDARY),
    );
    ui.add_space(4.0);
    ui.label(RichText::new(node.body).size(13.5).color(theme::TEXT));
    if !node.bullets.is_empty() {
        ui.add_space(2.0);
        for bullet in node.bullets {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("•").color(theme::TEXT_DISABLED));
                ui.add(
                    egui::Label::new(
                        RichText::new(*bullet)
                            .size(13.0)
                            .color(theme::TEXT_SECONDARY),
                    )
                    .wrap(),
                );
            });
        }
    }
    if !node.cmds.is_empty() {
        ui.add_space(2.0);
        egui::Frame::new()
            .fill(CODE)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::ScrollArea::horizontal()
                    .id_salt("flow-cmds")
                    .show(ui, |ui| {
                        // One command per line: scroll sideways, never wrap.
                        ui.add(
                            egui::Label::new(
                                RichText::new(node.cmds.join("\n"))
                                    .font(FontId::monospace(12.0))
                                    .color(theme::TEXT),
                            )
                            .extend(),
                        );
                    });
            });
    }
    ui.add_space(4.0);
    ui.label(
        RichText::new(format!("{} del documento", node.refs))
            .font(FontId::monospace(11.0))
            .color(theme::TEXT_DISABLED),
    );
}

fn mode_details(ui: &mut Ui, mode: Mode, sim: Sim) {
    kicker(ui, "Recorrido");
    title(ui, mode.title());
    ui.label(RichText::new(mode.text()).size(13.5).color(theme::TEXT));
    if sim.flaky {
        note(ui, flow::SIM_FLAKY, theme::AT_RISK);
    }
    if sim.reg {
        note(ui, flow::SIM_REG, theme::BROKEN);
    }
    ui.label(
        RichText::new(flow::HINT)
            .size(12.0)
            .color(theme::TEXT_DISABLED),
    );
}

fn live_details(
    ui: &mut Ui,
    phase: LivePhase,
    live: &Live,
    project: Option<&str>,
    progress: Option<&str>,
) {
    let kicker_text = match project {
        Some(name) => format!("En vivo · {name}"),
        None => "En vivo".to_string(),
    };
    kicker(ui, &kicker_text);
    title(ui, phase.title());
    ui.label(RichText::new(phase.text()).size(13.5).color(theme::TEXT));
    ui.add_space(4.0);
    for (i, label) in flow::LIVE_STEPS.iter().enumerate() {
        let accent = tone_color(live.tone).unwrap_or(theme::FOCUS);
        let color = if i < live.step {
            theme::TEXT_SECONDARY
        } else if i == live.step {
            theme::TEXT
        } else {
            theme::TEXT_DISABLED
        };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
            let p = ui.painter();
            if i < live.step {
                widgets::paint_state(p, r.center(), 6.0, theme::TestState::Passed);
            } else if i == live.step {
                widgets::dot(p, r.center(), 4.0, accent);
            } else {
                p.circle_stroke(r.center(), 4.0, Stroke::new(1.2, color));
            }
            ui.label(RichText::new(*label).color(color));
        });
        if i == 2
            && i == live.step
            && let Some(progress) = progress
        {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(progress)
                            .font(FontId::monospace(11.5))
                            .color(theme::TEXT_SECONDARY),
                    )
                    .wrap(),
                );
            });
        }
    }
    if let Some(node) = live.current.and_then(flow::node) {
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        node_details(ui, node, "");
    }
}

/// The four live stages as a breadcrumb.
fn live_strip(ui: &mut Ui, phase: LivePhase, progress: Option<&str>) {
    let live = flow::live(phase);
    let accent = tone_color(live.tone).unwrap_or(theme::FOCUS);
    widgets::chip(ui, "EN VIVO", accent);
    ui.add_space(4.0);
    for (i, label) in flow::LIVE_STEPS.iter().enumerate() {
        let text = match (i, phase, progress) {
            (3, LivePhase::Done(v), _) => format!("{label}: {}", v.label()),
            (2, LivePhase::Testing, Some(progress)) => format!("{label} · {progress}"),
            _ => (*label).to_string(),
        };
        let color = if i == live.step {
            accent
        } else if i < live.step {
            theme::TEXT
        } else {
            theme::TEXT_DISABLED
        };
        let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        if i == live.step {
            widgets::dot(ui.painter(), r.center(), 3.5, color);
        } else if i < live.step {
            widgets::paint_state(ui.painter(), r.center(), 5.5, theme::TestState::Passed);
        } else {
            ui.painter()
                .circle_stroke(r.center(), 3.5, Stroke::new(1.2, color));
        }
        ui.label(RichText::new(text).size(12.5).color(color));
        if i + 1 < flow::LIVE_STEPS.len() {
            ui.label(RichText::new("›").color(theme::TEXT_DISABLED));
        }
    }
}

/// "12 s", "3 min", "2 h".
fn age(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        _ => format!("{} h", secs / 3600),
    }
}

/// The integration state as a pill in the card's top-right corner; a
/// working lane's dot pulses.
fn paint_badge(p: &Painter, card: Rect, z: f32, state: State, pulse: f32) {
    if z < 0.3 {
        return;
    }
    let color = super::integrations_view::state_color(state);
    let galley = p.layout_no_wrap(
        state.label().to_string(),
        FontId::proportional(flow::text_px(10.0, 9.5, z)),
        theme::TEXT,
    );
    let pad = vec2(7.0, 3.0) * chip_z(z);
    let dot_w = 10.0 * chip_z(z);
    let size = vec2(
        galley.size().x + dot_w + 2.0 * pad.x,
        galley.size().y + 2.0 * pad.y,
    );
    let at = pos2(card.right() - 8.0 * z - size.x, card.top() + 8.0 * z);
    let pill = Rect::from_min_size(at, size);
    let radius = CornerRadius::same((size.y / 2.0).round() as u8);
    p.rect_filled(pill, radius, mix(CARD, color, 0.16));
    p.rect_stroke(
        pill,
        radius,
        Stroke::new(1.0, with_alpha(color, 0.55)),
        StrokeKind::Inside,
    );
    let c = pos2(pill.left() + pad.x + 3.0 * chip_z(z), pill.center().y);
    if state.animates() {
        p.circle_filled(c, (3.0 + 3.0 * pulse) * chip_z(z), with_alpha(color, 0.25));
    }
    p.circle_filled(c, 3.0 * chip_z(z), color);
    p.galley(
        pos2(pill.left() + pad.x + dot_w, pill.top() + pad.y),
        galley,
        theme::TEXT,
    );
}

/// The running test's progress along the bottom of the "Prueba local"
/// card: a bar (indeterminate stripe until the total is known) and the
/// count over it.
fn paint_progress(
    p: &Painter,
    card: Rect,
    z: f32,
    fraction: Option<f32>,
    summary: &str,
    clock: f32,
    motion: bool,
) {
    let inset = 12.0 * z;
    let track = Rect::from_min_max(
        pos2(card.left() + inset, card.bottom() - 8.0 * z),
        pos2(
            card.right() - inset,
            card.bottom() - 8.0 * z + (4.0 * z).max(2.5),
        ),
    );
    let radius = CornerRadius::same(2);
    p.rect_filled(track, radius, white(0.08));
    match fraction {
        Some(f) => {
            let fill = Rect::from_min_size(track.min, vec2(track.width() * f, track.height()));
            p.rect_filled(fill, radius, theme::FOCUS);
        }
        None => {
            let w = track.width() * 0.25;
            let t = if motion { (clock * 0.6).fract() } else { 0.0 };
            let x = track.left() + (track.width() - w) * t;
            let fill = Rect::from_min_size(pos2(x, track.top()), vec2(w, track.height()));
            p.rect_filled(fill, radius, theme::FOCUS);
        }
    }
    if z >= 0.45 {
        let galley = p.layout_no_wrap(
            summary.to_string(),
            FontId::monospace(flow::text_px(10.0, 9.5, z)),
            theme::TEXT_SECONDARY,
        );
        let pos = pos2(
            track.right() - galley.size().x,
            track.top() - galley.size().y - 2.0 * z,
        );
        if galley.size().x < track.width() {
            p.galley(pos, galley, theme::TEXT_SECONDARY);
        }
    }
}
