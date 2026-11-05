use super::*;

fn ids(set: &BTreeSet<&'static str>) -> Vec<&'static str> {
    set.iter().copied().collect()
}

fn fixed_layout() -> Layout {
    layout(|n| if n.chips.is_empty() { 90.0 } else { 116.0 })
}

#[test]
fn every_node_has_a_position_and_every_edge_known_ends() {
    assert_eq!(NODES.len(), 18);
    for n in NODES {
        assert!(
            POS.iter().any(|(id, _, _)| *id == n.id),
            "{} has no cell",
            n.id
        );
    }
    for (id, _, _) in POS {
        assert!(node(id).is_some(), "{id} is placed but not defined");
    }
    for e in EDGES {
        assert!(node(e.from).is_some() && node(e.to).is_some(), "{}", e.id);
    }
    for mode in Mode::ALL {
        for id in mode.lane().iter().chain(mode.steps()) {
            assert!(node(id).is_some(), "{id} in {mode:?}");
        }
    }
}

#[test]
fn texts_are_kept_verbatim_from_the_html() {
    let jira = node("jira").unwrap();
    assert_eq!(jira.stage, "01 · Origen");
    assert_eq!(jira.sub, "Ticket nuevo o cambio de estado");
    assert_eq!(jira.refs, "Secciones 1 y 14");
    let local = node("local").unwrap();
    assert_eq!(local.cmds[0], "guard up");
    assert_eq!(local.cmds.len(), 4);
    assert_eq!(
        node("guard").unwrap().chips,
        ["plan", "shards por duración", "finalize"]
    );
    assert_eq!(node("red").unwrap().tone, Tone::Err);
    assert!(node("skills").unwrap().sat);
    assert_eq!(Mode::Direct.title(), "Entrada directa");
    assert_eq!(Mode::Full.label(), "Completo");
}

#[test]
fn idle_nodes_are_the_lane_plus_the_outcome() {
    let plain = active_nodes(Mode::Direct, Sim::default(), None);
    assert_eq!(
        ids(&plain),
        ["confirm", "direct", "green", "guard", "ingest"]
    );
    let sim = Sim {
        reg: true,
        flaky: true,
    };
    let both = active_nodes(Mode::Direct, sim, None);
    assert!(both.contains("red") && both.contains("flaky") && !both.contains("green"));
}

#[test]
fn edges_follow_the_simulation() {
    let sim = Sim::default();
    let nodes = active_nodes(Mode::Full, sim, None);
    let edges: Vec<&str> = active_edges(&nodes, sim, None)
        .iter()
        .map(|e| e.id)
        .collect();
    assert!(edges.contains(&"confirm-green"));
    assert!(!edges.contains(&"confirm-red") && !edges.contains(&"red-coder"));
    let sim = Sim {
        reg: true,
        flaky: false,
    };
    let nodes = active_nodes(Mode::Full, sim, None);
    let edges: Vec<&str> = active_edges(&nodes, sim, None)
        .iter()
        .map(|e| e.id)
        .collect();
    assert!(edges.contains(&"confirm-red") && edges.contains(&"red-coder"));
    assert!(edges.contains(&"red-dev") && !edges.contains(&"confirm-green"));
}

#[test]
fn lineage_walks_both_ways_but_skips_feedback_and_side_edges() {
    let sim = Sim::default();
    let nodes = active_nodes(Mode::Agent, sim, None);
    let edges = active_edges(&nodes, sim, None);
    let keep = lineage("seal", &edges);
    for id in [
        "jira", "ingest", "jev", "gate", "qa", "seal", "coder", "guard", "confirm", "green",
    ] {
        assert!(keep.contains(id), "{id}");
    }
    // skills -> qa is upstream of seal; guard -> ingest is a side edge.
    assert!(keep.contains("skills"));
    assert!(!keep.contains("dev"));
}

#[test]
fn steps_end_with_the_outcome_and_feed_back_on_a_regression() {
    let green: Vec<&str> = build_steps(Mode::Direct, Sim::default())
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(green, ["direct", "guard", "confirm", "green"]);
    let sim = Sim {
        reg: true,
        flaky: true,
    };
    let full = build_steps(Mode::Full, sim);
    let tail: Vec<(&str, Option<&str>)> = full[full.len() - 4..]
        .iter()
        .map(|s| (s.id, s.via))
        .collect();
    assert_eq!(
        tail,
        [
            ("flaky", None),
            ("red", None),
            ("coder", Some("red-coder")),
            ("dev", Some("red-dev"))
        ]
    );
    let human = build_steps(
        Mode::Human,
        Sim {
            reg: true,
            flaky: false,
        },
    );
    assert_eq!(human.last().unwrap().via, Some("red-dev"));
}

#[test]
fn a_walk_enters_steps_through_the_edge_from_the_previous_one() {
    let sim = Sim::default();
    let mut walk = Walk::start(Mode::Human, sim);
    assert_eq!(walk.current(), "jira");
    assert_eq!(walk.comet(), None);
    assert!(walk.advance(sim));
    assert_eq!(walk.current(), "ingest");
    assert_eq!(walk.comet(), Some(("jira-ingest", Tone::Plain)));
    while walk.current() != "dev" {
        assert!(walk.advance(sim));
    }
    assert_eq!(walk.comet().unwrap().0, "gate-dev");
    let active = active_nodes(Mode::Human, sim, Some(&walk));
    assert!(active.contains("skills") && !active.contains("gateway"));
    while walk.advance(sim) {}
    assert!(walk.finished());
    assert_eq!(walk.position(), (walk.len(), walk.len()));
}

#[test]
fn walk_feedback_edges_only_show_once_reached() {
    let sim = Sim {
        reg: true,
        flaky: false,
    };
    let mut walk = Walk::start(Mode::Agent, sim);
    let nodes = active_nodes(Mode::Agent, sim, Some(&walk));
    assert!(
        active_edges(&nodes, sim, Some(&walk))
            .iter()
            .all(|e| !e.feedback)
    );
    while walk.advance(sim) && walk.current() != "red" {}
    assert!(walk.advance(sim));
    assert_eq!(walk.comet(), Some(("red-coder", Tone::Err)));
    let nodes = active_nodes(Mode::Agent, sim, Some(&walk));
    let ids: Vec<&str> = active_edges(&nodes, sim, Some(&walk))
        .iter()
        .map(|e| e.id)
        .collect();
    assert!(ids.contains(&"red-coder"));
}

#[test]
fn walk_timing_advances_after_a_step_and_eases_the_comet() {
    let sim = Sim::default();
    let mut walk = Walk::start(Mode::Direct, sim);
    assert!(!walk.tick(STEP_SECS - 0.1, sim));
    assert_eq!(walk.current(), "direct");
    assert!(walk.tick(0.2, sim));
    assert_eq!(walk.current(), "guard");
    assert_eq!(walk.comet_progress(), 0.0);
    walk.tick(COMET_SECS / 2.0, sim);
    assert!((walk.comet_progress() - 0.5).abs() < 1e-4);
    walk.tick(COMET_SECS, sim);
    assert_eq!(walk.comet_progress(), 1.0);
}

#[test]
fn ease_in_out_is_symmetric() {
    assert_eq!(ease(0.0), 0.0);
    assert_eq!(ease(1.0), 1.0);
    assert!((ease(0.5) - 0.5).abs() < 1e-6);
    assert!((ease(0.25) + ease(0.75) - 1.0).abs() < 1e-6);
}

#[test]
fn layout_follows_the_three_column_grid() {
    let l = fixed_layout();
    let jira = l.rects["jira"];
    let qa = l.rects["qa"];
    let dev = l.rects["dev"];
    let guard = l.rects["guard"];
    assert_eq!(qa.width(), COL_W);
    assert_eq!(l.rects["skills"].width(), SAT_W);
    assert_eq!(guard.width(), WIDE_W);
    assert_eq!(qa.left(), PAD_X);
    assert_eq!(dev.right(), PAD_X + 3.0 * COL_W + 2.0 * COL_GAP);
    assert_eq!(jira.center().x, l.size.x / 2.0);
    assert_eq!(guard.center().x, l.size.x / 2.0);
    assert_eq!(jira.top(), PAD_T);
    assert_eq!(l.rects["ingest"].top(), PAD_T + 90.0 + ROW_GAP);
    // Row 8 is 116 tall (chips): its 90-tall neighbours never share it.
    assert_eq!(guard.height(), 116.0);
    assert_eq!(l.size.x, 2.0 * PAD_X + 3.0 * COL_W + 2.0 * COL_GAP);
    assert_eq!(
        l.size.y,
        PAD_T + 8.0 * 90.0 + 2.0 * 116.0 + 9.0 * ROW_GAP + PAD_B
    );
}

#[test]
fn edge_paths_start_and_end_on_the_node_borders() {
    let l = fixed_layout();
    let e = |id: &str| EDGES.iter().find(|e| e.id == id).unwrap();
    let pts = edge_points(e("jira-ingest"), &l.rects);
    assert_eq!(
        pts[0],
        pos2(l.rects["jira"].center().x, l.rects["jira"].bottom())
    );
    assert_eq!(
        *pts.last().unwrap(),
        pos2(l.rects["ingest"].center().x, l.rects["ingest"].top())
    );
    let pts = edge_points(e("coder-guard"), &l.rects);
    assert_eq!(pts.last().unwrap().x, l.rects["guard"].center().x - 170.0);
    let pts = edge_points(e("confirm-flaky"), &l.rects);
    assert_eq!(pts[0].x, l.rects["confirm"].right());
    assert_eq!(pts.last().unwrap().x, l.rects["flaky"].left());
    let pts = edge_points(e("guard-ingest"), &l.rects);
    assert!(pts.iter().any(|p| p.x == CTX_X));
    let pts = edge_points(e("red-coder"), &l.rects);
    let floor = l.rects["red"].bottom().max(l.rects["green"].bottom()) + 26.0;
    assert!(pts.iter().all(|p| p.y <= floor + 1e-3));
    assert_eq!(
        *pts.last().unwrap(),
        pos2(l.rects["coder"].left(), l.rects["coder"].center().y)
    );
}

#[test]
fn point_at_measures_arc_length() {
    let pts = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(10.0, 30.0)];
    assert_eq!(point_at(&pts, 0.0), pos2(0.0, 0.0));
    assert_eq!(point_at(&pts, 0.25), pos2(10.0, 0.0));
    assert_eq!(point_at(&pts, 0.5), pos2(10.0, 10.0));
    assert_eq!(point_at(&pts, 1.0), pos2(10.0, 30.0));
    assert_eq!(point_at(&[pos2(1.0, 1.0)], 0.5), pos2(1.0, 1.0));
}

#[test]
fn verdict_reads_the_exit_and_the_report() {
    assert_eq!(verdict(true, false, 0, 0), Verdict::Green);
    assert_eq!(verdict(true, false, 0, 2), Verdict::Flaky);
    assert_eq!(verdict(false, false, 1, 1), Verdict::Regression);
    // A failed exit without failed tests (guard could not start) is no regression.
    assert_eq!(verdict(false, false, 0, 0), Verdict::Stopped);
    assert_eq!(verdict(false, true, 3, 0), Verdict::Stopped);
}

#[test]
fn live_phase_prefers_what_is_running_now() {
    assert_eq!(
        live_phase(Some(RunKind::Up), false, None),
        Some(LivePhase::Up)
    );
    assert_eq!(
        live_phase(None, true, Some(Verdict::Green)),
        Some(LivePhase::Selecting)
    );
    assert_eq!(
        live_phase(Some(RunKind::Test), true, None),
        Some(LivePhase::Testing)
    );
    assert_eq!(live_phase(Some(RunKind::Other), false, None), None);
    assert_eq!(
        live_phase(None, false, Some(Verdict::Flaky)),
        Some(LivePhase::Done(Verdict::Flaky))
    );
    assert_eq!(live_phase(None, false, None), None);
}

#[test]
fn live_nodes_follow_the_local_test_path() {
    let up = live(LivePhase::Up);
    assert_eq!(up.current, Some("local"));
    assert_eq!(up.lit, ["dev", "local"]);
    assert_eq!(up.step, 0);
    assert_eq!(live(LivePhase::Selecting).step, 1);
    let testing = live(LivePhase::Testing);
    assert_eq!((testing.current, testing.step), (Some("confirm"), 2));
    let red = live(LivePhase::Done(Verdict::Regression));
    assert_eq!(red.current, Some("red"));
    assert_eq!(red.tone, Tone::Err);
    assert_eq!(red.step, 3);
    let flaky = live(LivePhase::Done(Verdict::Flaky));
    assert_eq!(flaky.current, Some("flaky"));
    assert!(flaky.lit.contains(&"green"));
    let green = live(LivePhase::Done(Verdict::Green));
    assert_eq!((green.current, green.tone), (Some("green"), Tone::Ok));
    let stopped = live(LivePhase::Done(Verdict::Stopped));
    assert_eq!(stopped.current, None);
    for phase in [
        LivePhase::Up,
        LivePhase::Testing,
        LivePhase::Done(Verdict::Flaky),
    ] {
        for id in live(phase).lit {
            assert!(node(id).is_some(), "{id}");
        }
    }
}

#[test]
fn shot_spec_parses_every_key() {
    let spec = ShotSpec::parse("mode=agent,select=qa,step=3,sim=both,live=red").unwrap();
    assert_eq!(spec.mode, Mode::Agent);
    assert_eq!(spec.select, Some("qa"));
    assert_eq!(spec.step, Some(3));
    assert_eq!(
        spec.sim,
        Sim {
            reg: true,
            flaky: true
        }
    );
    assert_eq!(spec.live, Some(LivePhase::Done(Verdict::Regression)));
    assert_eq!(ShotSpec::parse("").unwrap(), ShotSpec::default());
    assert_eq!(
        ShotSpec::parse("live=test").unwrap().live,
        Some(LivePhase::Testing)
    );
    assert!(ShotSpec::parse("select=nope").is_err());
    assert!(ShotSpec::parse("mode=x").is_err());
    assert!(ShotSpec::parse("bogus").is_err());
}

#[test]
fn text_px_keeps_small_zoom_text_readable() {
    // At or above full zoom the base size scales as usual.
    assert_eq!(text_px(10.0, 10.0, 1.0), 10.0);
    assert_eq!(text_px(10.0, 10.0, 1.5), 15.0);
    // A typical first look: the floor wins over 6px.
    assert_eq!(text_px(10.0, 10.0, 0.6), 10.0);
    assert_eq!(text_px(14.0, 12.5, 0.6), 12.5);
    // Far zoomed out the floor shrinks too, so cards stay bounded.
    assert_eq!(text_px(10.0, 10.0, FLOOR_ZOOM / 2.0), 5.0);
    for z in [0.1, 0.3, 0.5, 0.8, 1.0] {
        assert!(text_px(12.0, 11.0, z) >= 12.0 * z);
        // In canvas units a card never grows past twice its base text.
        assert!(text_px(12.0, 11.0, z) / z <= 2.0 * 12.0 + 1e-4);
    }
}

#[test]
fn spread_fills_spare_width_within_bounds() {
    let base = 2.0 * PAD_X + 3.0 * COL_W + 2.0 * COL_GAP;
    assert_eq!(spread(base * 0.5), 1.0);
    assert_eq!(spread(base), 1.0);
    assert_eq!(spread(base * 10.0), MAX_SPREAD);
    let s = spread(base + 3.0 * COL_W * 0.2);
    assert!((s - 1.2).abs() < 1e-4);
}

#[test]
fn layout_spread_widens_cards_and_measures_them_at_that_width() {
    let l = layout_spread(1.4, |n, w| {
        assert_eq!(w, node_width(n) * 1.4);
        90.0
    });
    assert_eq!(l.rects["qa"].width(), COL_W * 1.4);
    assert_eq!(l.rects["guard"].width(), WIDE_W * 1.4);
    assert_eq!(l.rects["qa"].left(), PAD_X);
    assert_eq!(l.size.x, 2.0 * PAD_X + 3.0 * COL_W * 1.4 + 2.0 * COL_GAP);
    assert_eq!(l.rects["guard"].center().x, l.size.x / 2.0);
    assert_eq!(layout_spread(1.0, |_, _| 90.0), layout(|_| 90.0));
}

#[test]
fn the_live_mode_speaks_spanish() {
    assert_eq!(
        LIVE_STEPS,
        [
            "guard up",
            "Journeys afectados",
            "Corrida Playwright",
            "Resultado"
        ]
    );
    assert_eq!(Verdict::Stopped.label(), "Detenida");
    let phases = [
        LivePhase::Up,
        LivePhase::Selecting,
        LivePhase::Testing,
        LivePhase::Done(Verdict::Green),
        LivePhase::Done(Verdict::Regression),
        LivePhase::Done(Verdict::Flaky),
        LivePhase::Done(Verdict::Stopped),
    ];
    let english = [
        "Starting",
        "Selecting",
        "Run stopped",
        "passed",
        "the run",
        "retry",
    ];
    for phase in phases {
        for text in [phase.title(), phase.text()] {
            assert!(!text.is_empty());
            for word in english {
                assert!(!text.contains(word), "{text:?} is not Spanish");
            }
        }
    }
    assert_eq!(LivePhase::Up.title(), "Levantando servicios");
    assert_eq!(
        LivePhase::Done(Verdict::Stopped).title(),
        "Corrida detenida"
    );
}

#[test]
fn the_hint_names_the_visible_controls() {
    assert!(HINT.contains("Play"));
    assert!(HINT.contains("Step"));
    assert!(!HINT.contains("Recorrer paso a paso"));
}

#[test]
fn pan_centres_content_that_fits() {
    let pan = clamp_pan(vec2(-500.0, 900.0), vec2(400.0, 300.0), vec2(1000.0, 800.0));
    assert_eq!(pan, vec2(300.0, 250.0));
}

#[test]
fn pan_never_leaves_an_empty_band_around_tall_content() {
    let content = vec2(2000.0, 3000.0);
    let view = vec2(1000.0, 800.0);
    // Top edge would sit below the canvas top.
    assert_eq!(clamp_pan(vec2(50.0, 120.0), content, view), vec2(0.0, 0.0));
    // Bottom edge would sit above the canvas bottom.
    assert_eq!(
        clamp_pan(vec2(-1500.0, -2600.0), content, view),
        vec2(-1000.0, -2200.0)
    );
    // Inside the range it is left alone.
    assert_eq!(
        clamp_pan(vec2(-400.0, -900.0), content, view),
        vec2(-400.0, -900.0)
    );
}
