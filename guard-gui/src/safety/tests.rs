use super::*;

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn safe_journeys_run_without_asking() {
    let gate = check(&[("a.b", Safety::Safe), ("c.d", Safety::Safe)]);
    assert_eq!(gate, Gate::Allowed);
}

#[test]
fn contained_and_unclassified_journeys_need_confirmation_naming_them() {
    let gate = check(&[
        ("a.b", Safety::Safe),
        ("c.d", Safety::Contained),
        ("e.f", Safety::Unclassified),
    ]);
    assert_eq!(gate, Gate::Confirm(ids(&["c.d", "e.f"])));
}

#[test]
fn any_dangerous_journey_blocks_the_whole_run() {
    let gate = check(&[("a.b", Safety::Contained), ("x.y", Safety::Dangerous)]);
    assert_eq!(gate, Gate::Blocked(ids(&["x.y"])));
}

#[test]
fn an_empty_selection_is_refused() {
    assert_eq!(check(&[]), Gate::Empty);
}

#[test]
fn only_dangerous_journeys_are_not_runnable() {
    assert!(Safety::Safe.runnable());
    assert!(Safety::Contained.runnable());
    assert!(Safety::Unclassified.runnable());
    assert!(!Safety::Dangerous.runnable());
}

#[test]
fn labels_are_lowercase_words() {
    assert_eq!(Safety::Unclassified.label(), "unclassified");
    assert_eq!(Safety::Dangerous.label(), "dangerous");
}

fn cand(id: &str, smoke: bool, quarantined: bool, safety: Safety) -> Candidate<'_> {
    Candidate {
        id,
        smoke,
        quarantined,
        safety,
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    ids(v)
}

#[test]
fn run_all_covers_active_journeys_and_quarantined_smoke_ones() {
    let catalog = [
        cand("a.active", false, false, Safety::Safe),
        cand("b.quarantined", false, true, Safety::Dangerous),
        cand("c.smoke-quarantined", true, true, Safety::Dangerous),
    ];
    let got: Vec<&str> = suite_candidates(Suite::All, &catalog)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(got, ["a.active", "c.smoke-quarantined"]);
    assert_eq!(
        suite_gate(Suite::All, &catalog),
        Gate::Blocked(ids(&["c.smoke-quarantined"]))
    );
}

#[test]
fn affected_adds_every_smoke_journey_to_the_preview() {
    let catalog = [
        cand("a.path", false, false, Safety::Safe),
        cand("b.other", false, false, Safety::Dangerous),
        cand("c.smoke", true, true, Safety::Dangerous),
    ];
    let preview_ids = strings(&["a.path"]);
    let tags = strings(&["@smoke", "@journey:a.path"]);
    let suite = Suite::Affected(Some(AffectedPreview {
        ids: &preview_ids,
        tags: &tags,
    }));
    assert_eq!(
        suite_gate(suite, &catalog),
        Gate::Blocked(ids(&["c.smoke"]))
    );
}

#[test]
fn a_ticket_tag_or_a_failed_preview_makes_every_journey_a_candidate() {
    let catalog = [
        cand("a.path", false, false, Safety::Safe),
        cand("b.ticketed", false, true, Safety::Dangerous),
    ];
    let preview_ids = strings(&["a.path"]);
    let tags = strings(&["@smoke", "@ticket:SHOP-1", "@journey:a.path"]);
    let with_ticket = Suite::Affected(Some(AffectedPreview {
        ids: &preview_ids,
        tags: &tags,
    }));
    assert_eq!(
        suite_gate(with_ticket, &catalog),
        Gate::Blocked(ids(&["b.ticketed"]))
    );
    assert_eq!(
        suite_gate(Suite::Affected(None), &catalog),
        Gate::Blocked(ids(&["b.ticketed"]))
    );
}

#[test]
fn preview_ids_missing_from_the_catalog_are_unclassified() {
    let preview_ids = strings(&["new.journey"]);
    let tags = strings(&["@smoke"]);
    let suite = Suite::Affected(Some(AffectedPreview {
        ids: &preview_ids,
        tags: &tags,
    }));
    assert_eq!(suite_gate(suite, &[]), Gate::Confirm(ids(&["new.journey"])));
}

#[test]
fn suite_runs_always_ask_even_when_every_candidate_is_safe() {
    let catalog = [cand("a.safe", true, false, Safety::Safe)];
    assert_eq!(suite_gate(Suite::All, &catalog), Gate::Confirm(Vec::new()));
    let none: Vec<String> = Vec::new();
    let suite = Suite::Affected(Some(AffectedPreview {
        ids: &none,
        tags: &none,
    }));
    assert_eq!(suite_gate(suite, &[]), Gate::Confirm(Vec::new()));
    assert_eq!(suite_gate(Suite::All, &[]), Gate::Empty);
}
