use super::*;
use crate::results::Outcome;
use std::path::Path;

#[test]
fn a_result_decides_the_state_first() {
    assert_eq!(
        journey_state(Some(Outcome::Failed), true, true),
        TestState::Failed
    );
    assert_eq!(
        journey_state(Some(Outcome::Passed), false, false),
        TestState::Passed
    );
    assert_eq!(
        journey_state(Some(Outcome::Flaky), false, false),
        TestState::Flaky
    );
    assert_eq!(
        journey_state(Some(Outcome::Skipped), false, false),
        TestState::Skipped
    );
}

#[test]
fn without_a_result_quarantine_then_affected_decide() {
    assert_eq!(journey_state(None, true, true), TestState::Quarantined);
    assert_eq!(journey_state(None, true, false), TestState::Affected);
    assert_eq!(journey_state(None, false, false), TestState::NotRun);
}

#[test]
fn the_status_palette_is_the_impact_graph_one() {
    assert_eq!(TestState::Passed.color(), VERIFIED);
    assert_eq!(TestState::Failed.color(), BROKEN);
    assert_eq!(TestState::Flaky.color(), AT_RISK);
    assert_eq!(TestState::Quarantined.color(), CONFLICT);
    assert_eq!(TestState::Affected.color(), FOCUS);
    assert_eq!(VERIFIED, Color32::from_rgb(0x22, 0xc5, 0x5e));
    assert_eq!(BROKEN, Color32::from_rgb(0xe5, 0x48, 0x4d));
}

#[test]
fn an_area_shows_its_worst_journey() {
    use TestState::*;
    assert_eq!(area_state([Passed, Failed, Flaky]), Failed);
    assert_eq!(area_state([Passed, NotRun]), Passed);
    assert_eq!(area_state([Passed, Affected]), Affected);
    assert_eq!(area_state([NotRun, NotRun]), NotRun);
    assert_eq!(area_state([]), NotRun);
}

#[test]
fn windows_fonts_prefer_segoe_ui_and_jetbrains_mono() {
    // Expected paths are built with join, like the code, so the test also
    // holds on Linux CI where a backslash is not a separator.
    let windir = Path::new(r"C:\Windows");
    let local = Path::new(r"C:\Users\me\AppData\Local");
    let fonts = font_candidates(true, Some(windir), Some(local));
    let system = windir.join("Fonts");
    let user = local.join("Microsoft").join("Windows").join("Fonts");
    assert_eq!(fonts.ui[0], system.join("segoeui.ttf"));
    assert_eq!(fonts.mono[0], user.join("JetBrainsMono-Regular.ttf"));
    assert!(fonts.mono.contains(&system.join("consola.ttf")));
}

#[test]
fn other_systems_without_dirs_still_get_candidates_or_none() {
    let fonts = font_candidates(true, None, None);
    assert!(fonts.ui.is_empty() && fonts.mono.is_empty());
    let fonts = font_candidates(false, None, None);
    assert!(
        fonts
            .ui
            .iter()
            .all(|p| p.is_absolute() || p.starts_with("/"))
    );
}
