use super::*;

fn argv(cmd: &GuardCommand) -> Vec<String> {
    cmd.args()
}

#[test]
fn status_and_affected_ask_for_json() {
    assert_eq!(argv(&GuardCommand::Status), ["status", "--json"]);
    assert_eq!(argv(&GuardCommand::Affected), ["affected", "--json"]);
    assert_eq!(
        argv(&GuardCommand::Integrations),
        ["status", "--integrations", "--json"]
    );
}

#[test]
fn up_passes_the_timeout_in_seconds_and_down_has_no_flags() {
    assert_eq!(
        argv(&GuardCommand::Up { timeout_secs: 90 }),
        ["up", "--timeout", "90"]
    );
    assert_eq!(argv(&GuardCommand::Down), ["down"]);
}

#[test]
fn test_selections_use_exactly_one_selection_flag() {
    assert_eq!(
        argv(&GuardCommand::TestAffected(RunMode::Headless)),
        ["test", "--affected"]
    );
    assert_eq!(
        argv(&GuardCommand::TestAll(RunMode::Headless)),
        ["test", "--all"]
    );
}

#[test]
fn journeys_repeat_the_journey_flag() {
    let cmd = GuardCommand::test_journeys(["a.b", "c.d"], RunMode::Headless).unwrap();
    assert_eq!(argv(&cmd), ["test", "--journey", "a.b", "--journey", "c.d"]);
}

#[test]
fn an_empty_journey_list_builds_no_command() {
    assert_eq!(
        GuardCommand::test_journeys(Vec::<String>::new(), RunMode::Headed),
        None
    );
}

#[test]
fn report_has_no_flags() {
    assert_eq!(argv(&GuardCommand::Report), ["report"]);
}

#[test]
fn explain_always_asks_for_json_and_adds_a_journey_when_set() {
    assert_eq!(
        argv(&GuardCommand::Explain { journey: None }),
        ["explain", "--json"]
    );
    assert_eq!(
        argv(&GuardCommand::Explain {
            journey: Some("checkout.split-payment".into())
        }),
        ["explain", "--json", "--journey", "checkout.split-payment"]
    );
}

#[test]
fn the_display_line_quotes_the_whole_command() {
    let cmd = GuardCommand::test_journeys(["a.b"], RunMode::Headless).unwrap();
    assert_eq!(cmd.display(), "guard test --journey a.b");
}

#[test]
fn suspects_asks_for_json() {
    assert_eq!(argv(&GuardCommand::Suspects), ["suspects", "--json"]);
}

#[test]
fn headless_adds_no_flag_and_the_other_modes_append_theirs() {
    for (mode, extra) in [
        (RunMode::Headless, None),
        (RunMode::Headed, Some("--headed")),
        (RunMode::Ui, Some("--ui")),
    ] {
        let tail: Vec<&str> = extra.into_iter().collect();
        let with = |base: &[&str]| -> Vec<String> {
            base.iter()
                .chain(tail.iter())
                .map(|w| (*w).to_string())
                .collect()
        };
        assert_eq!(argv(&GuardCommand::TestAll(mode)), with(&["test", "--all"]));
        assert_eq!(
            argv(&GuardCommand::TestAffected(mode)),
            with(&["test", "--affected"])
        );
        let journeys = GuardCommand::test_journeys(["a.b", "c.d"], mode).unwrap();
        assert_eq!(
            argv(&journeys),
            with(&["test", "--journey", "a.b", "--journey", "c.d"])
        );
    }
}

#[test]
fn the_run_mode_defaults_to_headless_and_only_ui_is_interactive() {
    assert_eq!(RunMode::default(), RunMode::Headless);
    assert_eq!(RunMode::ALL.len(), 3);
    assert!(RunMode::Ui.interactive());
    assert!(!RunMode::Headed.interactive());
    assert!(!RunMode::Headless.interactive());
}

#[test]
fn every_run_mode_names_its_flag_in_the_selector() {
    for mode in RunMode::ALL {
        let flag = mode.flag().unwrap_or("headless");
        assert!(
            mode.label().contains(flag) || mode.label().to_lowercase().contains(flag),
            "{mode:?}"
        );
        assert!(!mode.short_label().is_empty());
    }
    assert_eq!(RunMode::Headed.short_label(), "Headed");
}

#[test]
fn only_test_commands_carry_a_run_mode() {
    assert_eq!(
        GuardCommand::TestAll(RunMode::Ui).run_mode(),
        Some(RunMode::Ui)
    );
    assert!(GuardCommand::TestAffected(RunMode::Headed).is_test());
    assert_eq!(GuardCommand::Report.run_mode(), None);
    assert!(!GuardCommand::Report.is_test());
}

#[test]
fn the_run_mode_round_trips_through_serde() {
    for mode in RunMode::ALL {
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(serde_json::from_str::<RunMode>(&json).unwrap(), mode);
    }
}

#[test]
fn explain_export_asks_for_the_format_and_the_output_file() {
    let out = std::path::PathBuf::from("repo/.guard/explain/explain-20261105-101500.md");
    let cmd = GuardCommand::ExplainExport {
        journey: None,
        format: ExportFormat::Markdown,
        out: out.clone(),
    };
    assert_eq!(
        argv(&cmd),
        [
            "explain",
            "--format",
            "md",
            "--out",
            &out.display().to_string()
        ]
    );
    let cmd = GuardCommand::ExplainExport {
        journey: Some("checkout.guest".into()),
        format: ExportFormat::Excel,
        out: out.with_extension("xlsx"),
    };
    assert_eq!(
        argv(&cmd),
        [
            "explain",
            "--format",
            "xlsx",
            "--out",
            &out.with_extension("xlsx").display().to_string(),
            "--journey",
            "checkout.guest"
        ]
    );
}
