use super::*;
use std::path::PathBuf;

fn parse(args: &[&str]) -> Result<Options, ArgsError> {
    parse_args(args.iter().map(|a| a.to_string()))
}

#[test]
fn no_repo_by_default() {
    let options = parse(&[]).unwrap();
    assert_eq!(options.repo, None);
    assert_eq!(options.screenshot, None);
}

#[test]
fn reads_repo_as_separate_or_joined_value() {
    assert_eq!(
        parse(&["--repo", "E:/app"]).unwrap().repo,
        Some(PathBuf::from("E:/app"))
    );
    assert_eq!(
        parse(&["--repo=E:/app"]).unwrap().repo,
        Some(PathBuf::from("E:/app"))
    );
}

#[test]
fn rejects_a_missing_value_and_unknown_flags() {
    assert_eq!(parse(&["--repo"]), Err(ArgsError::MissingValue("--repo")));
    assert_eq!(parse(&["--repo="]), Err(ArgsError::MissingValue("--repo")));
    assert_eq!(parse(&["--nope"]), Err(ArgsError::Unknown("--nope".into())));
}

#[test]
fn help_is_its_own_outcome() {
    assert_eq!(parse(&["-h"]), Err(ArgsError::Help));
    assert_eq!(parse(&["--help"]), Err(ArgsError::Help));
}

#[test]
fn screenshot_defaults_to_the_tests_view_at_1600_by_1000() {
    let options = parse(&["--screenshot", "out.png"]).unwrap();
    assert_eq!(
        options.screenshot,
        Some(Screenshot {
            path: PathBuf::from("out.png"),
            view: ShotView::Tests,
            size: [1600, 1000],
            flow: crate::flow::ShotSpec::default(),
        })
    );
}

#[test]
fn screenshot_takes_a_view_and_a_size() {
    let options = parse(&[
        "--screenshot",
        "run.png",
        "--view",
        "run",
        "--size",
        "1280x800",
        "--repo",
        "E:/app",
    ])
    .unwrap();
    let shot = options.screenshot.unwrap();
    assert_eq!(shot.view, ShotView::Run);
    assert_eq!(shot.size, [1280, 800]);
    assert_eq!(options.repo, Some(PathBuf::from("E:/app")));
    let flow = parse(&["--screenshot=f.png", "--view=flow"]).unwrap();
    assert_eq!(flow.screenshot.unwrap().view, ShotView::Flow);
}

#[test]
fn screenshot_views_cover_the_ai_settings_and_the_problems_summary() {
    let ai = parse(&["--screenshot", "a.png", "--view", "ai-settings"]).unwrap();
    assert_eq!(ai.screenshot.unwrap().view, ShotView::AiSettings);
    let problems = parse(&["--screenshot", "p.png", "--view", "problems"]).unwrap();
    assert_eq!(problems.screenshot.unwrap().view, ShotView::Problems);
}

#[test]
fn view_and_size_need_a_screenshot_and_valid_values() {
    assert_eq!(
        parse(&["--view", "run"]),
        Err(ArgsError::NeedsScreenshot("--view"))
    );
    assert_eq!(
        parse(&["--screenshot", "a.png", "--view", "nope"]),
        Err(ArgsError::Invalid("--view", "nope".into()))
    );
    assert_eq!(
        parse(&["--screenshot", "a.png", "--size", "big"]),
        Err(ArgsError::Invalid("--size", "big".into()))
    );
}

#[test]
fn parse_size_reads_width_by_height() {
    assert_eq!(parse_size("1600x1000"), Some([1600, 1000]));
    assert_eq!(parse_size("800X600"), Some([800, 600]));
    assert_eq!(parse_size("0x600"), None);
    assert_eq!(parse_size("1600"), None);
    assert_eq!(parse_size("ax1"), None);
}

#[test]
fn flow_state_is_parsed_and_needs_a_screenshot() {
    let options = parse(&["--screenshot", "f.png", "--flow", "mode=agent,select=qa"]).unwrap();
    let shot = options.screenshot.unwrap();
    assert_eq!(shot.flow.mode, crate::flow::Mode::Agent);
    assert_eq!(shot.flow.select, Some("qa"));
    assert_eq!(
        parse(&["--screenshot", "f.png"])
            .unwrap()
            .screenshot
            .unwrap()
            .flow,
        crate::flow::ShotSpec::default()
    );
    assert_eq!(
        parse(&["--flow", "mode=agent"]),
        Err(ArgsError::NeedsScreenshot("--flow"))
    );
    assert_eq!(
        parse(&["--screenshot", "f.png", "--flow", "mode=x"]),
        Err(ArgsError::Invalid("--flow", "mode=x".into()))
    );
}
