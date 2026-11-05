//! Behavioral suite for `DecisionLog`, `GateHistory` and `FlakyHistory`.
//! Every function runs against `MemoryStore` and (when `DATABASE_URL` is set)
//! `PgStore`, so both implementations stay interchangeable.

use super::fixtures::*;
use guard_core::ports::*;
use guard_store::{FlakyHistory, FlakyQuery};

pub trait Store: DecisionLog + GateHistory + FlakyHistory {}
impl<T: DecisionLog + GateHistory + FlakyHistory> Store for T {}

fn q() -> CalibrationQuery {
    CalibrationQuery::default()
}

// ---------------------------------------------------------------- decisions

pub async fn decision_round_trips_through_calibration(s: &impl Store) {
    let d = decision("SHOP-1", "2026-11-01T09:00:00Z", Mode::Shadow);
    let id = s.record_decision(&d).await.unwrap();
    let rows = s.list_for_calibration(&q()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision_id, id);
    assert_eq!(rows[0].decision, d);
    assert_eq!(rows[0].human, None);
}

pub async fn decision_ids_increase(s: &impl Store) {
    let a = s
        .record_decision(&decision("SHOP-1", "2026-11-01T09:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    let b = s
        .record_decision(&decision("SHOP-2", "2026-11-01T09:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    assert!(b > a, "{b:?} > {a:?}");
}

pub async fn failed_engine_decision_is_recorded(s: &impl Store) {
    let d = failed_decision("SHOP-9", "2026-11-01T09:00:00Z");
    s.record_decision(&d).await.unwrap();
    let rows = s.list_for_calibration(&q()).await.unwrap();
    assert_eq!(rows[0].decision, d);
}

pub async fn decision_without_answers_nor_error_is_invalid(s: &impl Store) {
    let mut d = failed_decision("SHOP-9", "2026-11-01T09:00:00Z");
    d.engine_error = None;
    let err = s.record_decision(&d).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
    assert!(s.list_for_calibration(&q()).await.unwrap().is_empty());
}

pub async fn decision_with_bad_timestamp_is_invalid(s: &impl Store) {
    let d = decision("SHOP-1", "yesterday", Mode::Shadow);
    let err = s.record_decision(&d).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
}

pub async fn calibration_keeps_latest_decision_per_ticket(s: &impl Store) {
    // Recorded out of order: the latest is by decided_at, not by insertion.
    let late = decision("SHOP-1", "2026-11-01T12:00:00Z", Mode::Active);
    let early = decision("SHOP-1", "2026-11-01T08:00:00Z", Mode::Shadow);
    let late_id = s.record_decision(&late).await.unwrap();
    s.record_decision(&early).await.unwrap();
    // Same decided_at: the higher id wins.
    let other_a = decision("SHOP-2", "2026-11-01T10:00:00Z", Mode::Shadow);
    let mut other_b = other_a.clone();
    other_b.permutation_seed = Some(1);
    s.record_decision(&other_a).await.unwrap();
    let other_b_id = s.record_decision(&other_b).await.unwrap();

    let rows = s.list_for_calibration(&q()).await.unwrap();
    assert_eq!(rows.len(), 2);
    // Newest decision first.
    assert_eq!(rows[0].decision_id, late_id);
    assert_eq!(rows[0].decision, late);
    assert_eq!(rows[1].decision_id, other_b_id);
    assert_eq!(rows[1].decision.permutation_seed, Some(1));
}

pub async fn calibration_joins_latest_human_label(s: &impl Store) {
    s.record_decision(&decision("SHOP-1", "2026-11-01T09:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    s.record_human_label(&label(
        "SHOP-1",
        "2026-11-02T09:00:00Z",
        &["type:refactor", "area:checkout"],
    ))
    .await
    .unwrap();
    s.record_human_label(&label("SHOP-1", "2026-11-01T10:00:00Z", &["old"]))
        .await
        .unwrap();
    s.record_human_label(&label("OTHER-1", "2026-11-03T09:00:00Z", &["x"]))
        .await
        .unwrap();

    let rows = s.list_for_calibration(&q()).await.unwrap();
    assert_eq!(rows.len(), 1);
    let human = rows[0].human.clone().expect("a label");
    // Labels come back sorted.
    let expected = label(
        "SHOP-1",
        "2026-11-02T09:00:00Z",
        &["area:checkout", "type:refactor"],
    );
    assert_eq!(human, expected);
}

pub async fn human_label_with_empty_optionals_round_trips(s: &impl Store) {
    s.record_decision(&decision("SHOP-1", "2026-11-01T09:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    let l = HumanLabel {
        area: None,
        kind: None,
        lane: None,
        flow_change: None,
        labels: vec![],
        source: LabelSource::Manual,
        ..label("SHOP-1", "2026-11-02T09:00:00Z", &[])
    };
    s.record_human_label(&l).await.unwrap();
    let rows = s.list_for_calibration(&q()).await.unwrap();
    assert_eq!(rows[0].human, Some(l));
}

pub async fn calibration_filters(s: &impl Store) {
    s.record_decision(&decision("A-1", "2026-10-01T00:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    s.record_decision(&decision("A-2", "2026-11-01T00:00:00Z", Mode::Active))
        .await
        .unwrap();
    s.record_decision(&decision("A-3", "2026-11-02T00:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    s.record_human_label(&label("A-2", "2026-11-03T00:00:00Z", &["lane:human"]))
        .await
        .unwrap();

    let keys = |rows: Vec<CalibrationRow>| -> Vec<String> {
        rows.into_iter().map(|r| r.decision.ticket_key).collect()
    };
    let all = s.list_for_calibration(&q()).await.unwrap();
    assert_eq!(keys(all), ["A-3", "A-2", "A-1"]);

    let since = CalibrationQuery {
        since: Some("2026-11-01T00:00:00Z".into()),
        ..q()
    };
    assert_eq!(
        keys(s.list_for_calibration(&since).await.unwrap()),
        ["A-3", "A-2"]
    );

    let shadow = CalibrationQuery {
        mode: Some(Mode::Shadow),
        ..q()
    };
    assert_eq!(
        keys(s.list_for_calibration(&shadow).await.unwrap()),
        ["A-3", "A-1"]
    );

    let labeled = CalibrationQuery {
        labeled_only: true,
        ..q()
    };
    assert_eq!(
        keys(s.list_for_calibration(&labeled).await.unwrap()),
        ["A-2"]
    );

    let limited = CalibrationQuery { limit: 2, ..q() };
    assert_eq!(
        keys(s.list_for_calibration(&limited).await.unwrap()),
        ["A-3", "A-2"]
    );
}

pub async fn calibration_mode_filter_applies_to_latest_decision(s: &impl Store) {
    // The latest decision of a ticket is active; an earlier shadow one must not show up.
    s.record_decision(&decision("A-1", "2026-11-01T00:00:00Z", Mode::Shadow))
        .await
        .unwrap();
    s.record_decision(&decision("A-1", "2026-11-02T00:00:00Z", Mode::Active))
        .await
        .unwrap();
    let shadow = CalibrationQuery {
        mode: Some(Mode::Shadow),
        ..q()
    };
    assert!(s.list_for_calibration(&shadow).await.unwrap().is_empty());
}

pub async fn calibration_since_must_be_rfc3339(s: &impl Store) {
    let bad = CalibrationQuery {
        since: Some("last week".into()),
        ..q()
    };
    let err = s.list_for_calibration(&bad).await.unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "{err:?}");
}

// ---------------------------------------------------------------- gate results

pub async fn gate_result_round_trips(s: &impl Store) {
    let r = report(
        Some("acme/shop"),
        "run-1",
        Some(7),
        vec![test("p::f::ok", TestStatus::Passed)],
    );
    let o = outcome("tracker: 503");
    let id = s.record_gate_result(&r, &o).await.unwrap();
    let recent = s.recent_gate_results(10).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].id, id);
    assert_eq!(recent[0].report, r);
    assert_eq!(recent[0].outcome, o);
    let at = &recent[0].received_at;
    assert!(
        at.len() >= 20 && at.ends_with('Z') && at.as_bytes()[10] == b'T',
        "RFC 3339 UTC: {at}"
    );
}

pub async fn gate_result_upsert_keeps_id_and_moves_to_front(s: &impl Store) {
    let first = report(Some("acme/shop"), "run-1", Some(7), vec![]);
    let id = s.record_gate_result(&first, &outcome("")).await.unwrap();
    let other = report(Some("acme/shop"), "run-2", Some(8), vec![]);
    let other_id = s.record_gate_result(&other, &outcome("")).await.unwrap();
    assert_ne!(id, other_id);

    let mut rerun = first.clone();
    rerun.verdict = GateVerdict::Red;
    let again = s
        .record_gate_result(&rerun, &outcome("rerun"))
        .await
        .unwrap();
    assert_eq!(again, id);

    let recent = s.recent_gate_results(10).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].id, id, "the re-run is the newest");
    assert_eq!(recent[0].report.verdict, GateVerdict::Red);
    assert_eq!(recent[0].outcome, outcome("rerun"));
    assert_eq!(recent[1].id, other_id);
}

pub async fn gate_result_same_run_id_other_repo_is_distinct(s: &impl Store) {
    let a = s
        .record_gate_result(
            &report(Some("acme/shop"), "run-1", None, vec![]),
            &outcome(""),
        )
        .await
        .unwrap();
    let b = s
        .record_gate_result(
            &report(Some("acme/blog"), "run-1", None, vec![]),
            &outcome(""),
        )
        .await
        .unwrap();
    let c = s
        .record_gate_result(&report(None, "run-1", None, vec![]), &outcome(""))
        .await
        .unwrap();
    let c2 = s
        .record_gate_result(&report(None, "run-1", None, vec![]), &outcome("x"))
        .await
        .unwrap();
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_eq!(c, c2, "repo None upserts on the empty repo");
    assert_eq!(s.recent_gate_results(10).await.unwrap().len(), 3);
}

pub async fn recent_gate_results_limit(s: &impl Store) {
    for i in 0..5 {
        s.record_gate_result(
            &report(Some("acme/shop"), &format!("run-{i}"), None, vec![]),
            &outcome(""),
        )
        .await
        .unwrap();
    }
    let recent = s.recent_gate_results(3).await.unwrap();
    let runs: Vec<_> = recent.iter().map(|g| g.report.run_id.as_str()).collect();
    assert_eq!(runs, ["run-4", "run-3", "run-2"]);
    assert!(s.recent_gate_results(0).await.unwrap().is_empty());
}

pub async fn latest_for_pr_picks_newest_of_that_pr(s: &impl Store) {
    assert_eq!(s.latest_for_pr("acme/shop", 7).await.unwrap(), None);
    s.record_gate_result(
        &report(Some("acme/shop"), "run-1", Some(7), vec![]),
        &outcome(""),
    )
    .await
    .unwrap();
    let newest = s
        .record_gate_result(
            &report(Some("acme/shop"), "run-2", Some(7), vec![]),
            &outcome(""),
        )
        .await
        .unwrap();
    s.record_gate_result(
        &report(Some("acme/shop"), "run-3", Some(8), vec![]),
        &outcome(""),
    )
    .await
    .unwrap();
    s.record_gate_result(
        &report(Some("acme/blog"), "run-4", Some(7), vec![]),
        &outcome(""),
    )
    .await
    .unwrap();

    let got = s.latest_for_pr("acme/shop", 7).await.unwrap().unwrap();
    assert_eq!(got.id, newest);
    assert_eq!(got.report.run_id, "run-2");
    assert_eq!(s.latest_for_pr("acme/shop", 99).await.unwrap(), None);
}

// ---------------------------------------------------------------- flaky history

pub async fn flaky_stats_over_window(s: &impl Store) {
    let pay = "chromium::pay.spec.ts::pays";
    let cart = "chromium::cart.spec.ts::adds";
    let login = "chromium::login.spec.ts::logs in";
    // run-0 is oldest; with window 4 it falls out.
    let runs = [
        vec![test(pay, TestStatus::Flaky), test(cart, TestStatus::Flaky)],
        vec![
            test(pay, TestStatus::Flaky),
            test(cart, TestStatus::Passed),
            test(login, TestStatus::Passed),
        ],
        vec![
            test(pay, TestStatus::Passed),
            test(cart, TestStatus::Failed),
            test(login, TestStatus::Skipped),
        ],
        vec![
            test(pay, TestStatus::Passed),
            test(cart, TestStatus::Passed),
            test(login, TestStatus::Passed),
        ],
        vec![
            test(pay, TestStatus::Passed),
            test(cart, TestStatus::Flaky),
            test(login, TestStatus::Passed),
        ],
    ];
    for (i, tests) in runs.into_iter().enumerate() {
        s.record_gate_result(
            &report(Some("acme/shop"), &format!("run-{i}"), None, tests),
            &outcome(""),
        )
        .await
        .unwrap();
    }
    // Another repo never counts when filtering by repo.
    s.record_gate_result(
        &report(
            Some("acme/blog"),
            "run-x",
            None,
            vec![test(login, TestStatus::Flaky)],
        ),
        &outcome(""),
    )
    .await
    .unwrap();

    let stats = s
        .flaky_stats(&FlakyQuery {
            repo: Some("acme/shop".into()),
            window: 4,
        })
        .await
        .unwrap();
    // Window = run-1..run-4. pay: 1 flaky / 4 runs; cart: 1 flaky + 1 failed / 4 runs.
    // login never flaky in acme/shop: not listed. Skipped runs do not count.
    assert_eq!(stats.len(), 2, "{stats:?}");
    assert_eq!(stats[0].test_id, cart);
    assert_eq!((stats[0].runs, stats[0].flaky, stats[0].failed), (4, 1, 1));
    assert_eq!(stats[1].test_id, pay);
    assert_eq!((stats[1].runs, stats[1].flaky, stats[1].failed), (4, 1, 0));
    assert!((stats[1].flaky_rate - 0.25).abs() < 1e-9);
    assert_eq!(stats[0].journey.as_deref(), Some("checkout.pay"));

    // Ties on rate are ordered by test_id; window 0 means 50.
    let all = s
        .flaky_stats(&FlakyQuery {
            repo: None,
            window: 0,
        })
        .await
        .unwrap();
    let ids: Vec<_> = all.iter().map(|f| f.test_id.as_str()).collect();
    // login: 1 flaky of 4 counted runs (3 passed in shop + 1 flaky in blog) = 0.25.
    // cart: 2/5 = 0.4, pay: 2/5 = 0.4.
    assert_eq!(ids, [cart, pay, login]);
}

pub async fn flaky_stats_follow_upserted_runs(s: &impl Store) {
    let pay = "chromium::pay.spec.ts::pays";
    s.record_gate_result(
        &report(
            Some("acme/shop"),
            "run-1",
            None,
            vec![test(pay, TestStatus::Flaky)],
        ),
        &outcome(""),
    )
    .await
    .unwrap();
    // The finalize is re-run and now the test passes: the old row is replaced.
    s.record_gate_result(
        &report(
            Some("acme/shop"),
            "run-1",
            None,
            vec![test(pay, TestStatus::Passed)],
        ),
        &outcome(""),
    )
    .await
    .unwrap();
    let stats = s
        .flaky_stats(&FlakyQuery {
            repo: Some("acme/shop".into()),
            window: 50,
        })
        .await
        .unwrap();
    assert!(stats.is_empty(), "{stats:?}");
}
