use super::*;
use crate::ports::evidence_store::MAX_PRESIGN_EXPIRY;
use crate::thresholds::{Mode, Thresholds};
use std::time::Duration;

fn answers() -> TriageAnswers {
    TriageAnswers {
        model: "jev-1.13.0".into(),
        area: Classified {
            value: Area::new("catalog"),
            confidence: 0.9,
        },
        kind: Classified {
            value: Kind::Bugfix,
            confidence: 0.9,
        },
        risk: Classified {
            value: 0.4,
            confidence: 0.9,
        },
        clear_e2e_criteria: 0.9,
        touches_money_or_security: 0.01,
        agent_suitable: 0.95,
        changes_existing_flow: 0.05,
        raw: serde_json::json!({}),
    }
}

fn request(key: &str) -> TriageRequest {
    TriageRequest {
        ticket_key: key.into(),
        state: TriageState {
            summary: "Fix stock badge".into(),
            description: String::new(),
            acceptance_criteria: vec![],
            mentioned_modules: vec![],
            blast_radius: BlastRadius::Isolated,
        },
        areas: vec![],
        permutation_seed: None,
    }
}

fn decision(key: &str, at: &str) -> TriageDecision {
    TriageDecision {
        ticket_key: key.into(),
        decided_at: at.into(),
        mode: Mode::Shadow,
        state: request(key).state,
        answers: Some(answers()),
        engine_error: None,
        facts: ImpactFacts::default(),
        thresholds: Thresholds::default(),
        permutation_seed: Some(7),
        decided: Lane::Agent,
        effective: Lane::Human(vec![HumanReason::ShadowMode]),
    }
}

fn label(key: &str, at: &str) -> HumanLabel {
    HumanLabel {
        ticket_key: key.into(),
        labeled_at: at.into(),
        area: Some(Area::new("catalog")),
        kind: Some(Kind::Bugfix),
        lane: Some(LaneKind::Agent),
        flow_change: None,
        labels: vec!["area:catalog".into(), "type:bugfix".into()],
        source: LabelSource::JiraWebhook,
    }
}

fn pr(number: u64) -> PrContext {
    PrContext {
        provider: CodeHostProvider::Github,
        repo: "acme/shop".into(),
        number,
        title: "SHOP-1 fix cart".into(),
        url: format!("https://github.com/acme/shop/pull/{number}"),
        author: "dev".into(),
        ticket_key: Some("SHOP-1".into()),
        branch: "SHOP-1-fix-cart".into(),
        base_branch: "main".into(),
        base_sha: "b".repeat(40),
        head_sha: "a".repeat(40),
        changed_files: vec!["web/src/cart/index.ts".into()],
    }
}

fn gate_report(run_id: &str, number: u64) -> GateReport {
    GateReport {
        version: 2,
        run_id: run_id.into(),
        generated_at: "2026-10-31T12:00:00Z".into(),
        provider: Some(CodeHostProvider::Github),
        repo: Some("acme/shop".into()),
        pr: Some(pr(number)),
        ticket_key: Some("SHOP-1".into()),
        ticket_is_flow_change: false,
        head_sha: "a".repeat(40),
        base_sha: Some("b".repeat(40)),
        verdict: GateVerdict::Green,
        counts: GateCounts::default(),
        tests: vec![],
        journey_changes: vec![],
        change_flags: vec![],
        report_url: None,
        ci_url: None,
    }
}

// ------------------------------------------------------------ decision engine

#[tokio::test]
async fn engine_returns_scripted_triage_answers_in_order_and_records_requests() {
    let engine = FakeDecisionEngine::new()
        .with_triage(Ok(answers()))
        .with_triage(Err(EngineError::Timeout));
    assert_eq!(engine.model(), "jev-1.13.0");
    assert_eq!(engine.triage(&request("SHOP-1")).await, Ok(answers()));
    assert_eq!(
        engine.triage(&request("SHOP-2")).await,
        Err(EngineError::Timeout)
    );
    let keys: Vec<_> = engine
        .triage_requests()
        .into_iter()
        .map(|r| r.ticket_key)
        .collect();
    assert_eq!(keys, ["SHOP-1", "SHOP-2"]);
}

#[tokio::test]
async fn engine_without_a_scripted_answer_is_unconfigured() {
    let engine = FakeDecisionEngine::new();
    assert!(matches!(
        engine.triage(&request("SHOP-1")).await,
        Err(EngineError::Unconfigured(_))
    ));
}

#[tokio::test]
async fn engine_impact_answers_every_journey_defaulting_to_zero() {
    let engine = FakeDecisionEngine::new().with_impact("checkout.card", 0.9);
    let journeys = [
        JourneyQuestion {
            id: "checkout.card".into(),
            impact_question: "q".into(),
        },
        JourneyQuestion {
            id: "catalog.list".into(),
            impact_question: "q".into(),
        },
    ];
    let out = engine
        .impact(&DiffSummary::default(), &journeys)
        .await
        .unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out["checkout.card"], 0.9);
    assert_eq!(out["catalog.list"], 0.0);
    assert_eq!(engine.impact_calls(), 1);
}

#[tokio::test]
async fn engine_impact_can_fail_and_empty_input_skips_the_call() {
    let engine = FakeDecisionEngine::new().failing_impact(EngineError::Retryable(529));
    assert_eq!(
        engine.impact(&DiffSummary::default(), &[]).await,
        Ok(Default::default())
    );
    assert_eq!(engine.impact_calls(), 0);
    let journeys = [JourneyQuestion {
        id: "a.b".into(),
        impact_question: "q".into(),
    }];
    assert_eq!(
        engine.impact(&DiffSummary::default(), &journeys).await,
        Err(EngineError::Retryable(529))
    );
}

// ------------------------------------------------------------ issue tracker

#[tokio::test]
async fn tracker_creates_bugs_finds_them_by_label_newest_first() {
    let tracker = InMemoryTracker::new("SHOP");
    let bug = |summary: &str| NewBug {
        project: "SHOP".into(),
        summary: summary.into(),
        description_adf: serde_json::json!({"type": "doc", "version": 1, "content": []}),
        labels: vec!["guard".into(), "guard-fp-1a2b3c4d".into()],
        priority: None,
        linked_issue: Some("SHOP-1".into()),
    };
    let first = tracker.create_bug(&bug("one")).await.unwrap();
    let second = tracker.create_bug(&bug("two")).await.unwrap();
    assert_eq!(first.key, "SHOP-1001");
    assert_eq!(second.key, "SHOP-1002");
    assert_eq!(first.status_category, StatusCategory::ToDo);
    let found = tracker.find_by_label("guard-fp-1a2b3c4d").await.unwrap();
    let keys: Vec<_> = found.into_iter().map(|i| i.key).collect();
    assert_eq!(keys, ["SHOP-1002", "SHOP-1001"]);
    assert!(tracker.find_by_label("nope").await.unwrap().is_empty());
    assert_eq!(tracker.created_bugs().len(), 2);
}

#[tokio::test]
async fn tracker_comments_attaches_and_labels_existing_issues() {
    let tracker = InMemoryTracker::new("SHOP");
    tracker.insert_issue(Issue {
        key: "SHOP-1".into(),
        url: "https://jira.example/browse/SHOP-1".into(),
        summary: "Fix cart".into(),
        description: "text".into(),
        issue_type: "Story".into(),
        status: "To Do".into(),
        status_category: StatusCategory::ToDo,
        labels: vec!["area:catalog".into()],
    });
    let body = serde_json::json!({"type": "doc", "version": 1, "content": []});
    tracker.comment_adf("SHOP-1", &body).await.unwrap();
    let file = Attachment {
        file_name: "shot.png".into(),
        content_type: "image/png".into(),
        bytes: vec![1, 2, 3],
    };
    let att = tracker.attach_file("SHOP-1", &file).await.unwrap();
    assert_eq!(att.file_name, "shot.png");
    tracker
        .add_labels("SHOP-1", &["area:catalog".into(), "flow-change".into()])
        .await
        .unwrap();
    tracker
        .suggest_labels("SHOP-1", &["type:bugfix".into()])
        .await
        .unwrap();

    let issue = tracker.get_issue("SHOP-1").await.unwrap();
    assert_eq!(issue.labels, ["area:catalog", "flow-change"]);
    assert_eq!(tracker.comments("SHOP-1"), vec![body]);
    assert_eq!(tracker.attachments("SHOP-1"), vec![file]);
    assert_eq!(tracker.suggested_labels("SHOP-1"), ["type:bugfix"]);
}

#[tokio::test]
async fn tracker_unknown_issue_is_not_found() {
    let tracker = InMemoryTracker::new("SHOP");
    assert_eq!(
        tracker.get_issue("SHOP-9").await,
        Err(TrackerError::NotFound("SHOP-9".into()))
    );
    assert_eq!(
        tracker.add_labels("SHOP-9", &[]).await,
        Err(TrackerError::NotFound("SHOP-9".into()))
    );
}

#[test]
fn tracker_parses_jira_webhook_payloads() {
    let tracker = InMemoryTracker::new("SHOP");
    let req = |body: serde_json::Value| WebhookRequest {
        body: serde_json::to_vec(&body).unwrap(),
        ..WebhookRequest::default()
    };
    assert_eq!(
        tracker.parse_webhook(&req(serde_json::json!({
            "webhookEvent": "jira:issue_created",
            "issue": {"key": "SHOP-7", "fields": {"labels": []}}
        }))),
        Ok(TrackerEvent::IssueCreated {
            key: "SHOP-7".into()
        })
    );
    assert_eq!(
        tracker.parse_webhook(&req(serde_json::json!({
            "webhookEvent": "jira:issue_updated",
            "issue": {"key": "SHOP-7", "fields": {"labels": ["type:bugfix", "area:catalog"]}},
            "changelog": {"items": [{"field": "labels"}]}
        }))),
        Ok(TrackerEvent::LabelsChanged {
            key: "SHOP-7".into(),
            labels: vec!["area:catalog".into(), "type:bugfix".into()]
        })
    );
    assert!(matches!(
        tracker.parse_webhook(&req(serde_json::json!({"webhookEvent": "comment_created"}))),
        Ok(TrackerEvent::Ignored { .. })
    ));
    assert!(matches!(
        tracker.parse_webhook(&WebhookRequest {
            body: b"not json".to_vec(),
            ..WebhookRequest::default()
        }),
        Err(TrackerWebhookError::Malformed(_))
    ));
}

// ------------------------------------------------------------ code host

#[tokio::test]
async fn code_host_serves_pr_context_and_records_statuses() {
    let host = FakeCodeHost::new("s3cret").with_pr(pr(12));
    assert_eq!(host.provider(), CodeHostProvider::Github);
    assert_eq!(
        host.pr_context(PrRef { number: 12 }, None).await,
        Ok(pr(12))
    );
    assert_eq!(
        host.pr_context(PrRef { number: 13 }, None).await,
        Err(CodeHostError::NotFound(13))
    );
    let status = GateStatus {
        head_sha: "a".repeat(40),
        name: "guard".into(),
        state: CheckState::Success,
        title: "green".into(),
        summary: String::new(),
        details_url: None,
    };
    host.publish_status(&status).await.unwrap();
    assert_eq!(host.statuses(), vec![status]);
}

#[tokio::test]
async fn code_host_comment_pr_upserts_by_marker() {
    let host = FakeCodeHost::new("s3cret").with_pr(pr(12));
    let marker = "<!-- guard:gate-summary -->";
    host.comment_pr(PrRef { number: 12 }, &format!("{marker}\nfirst"))
        .await
        .unwrap();
    host.comment_pr(PrRef { number: 12 }, &format!("{marker}\nsecond"))
        .await
        .unwrap();
    host.comment_pr(PrRef { number: 12 }, "unrelated")
        .await
        .unwrap();
    assert_eq!(
        host.comments(12),
        vec![format!("{marker}\nsecond"), "unrelated".to_string()]
    );
}

#[test]
fn code_host_verifies_the_token_and_parses_json_events() {
    let host = FakeCodeHost::new("s3cret");
    let mut req = WebhookRequest {
        body: serde_json::to_vec(&CodeHostEvent::Ping).unwrap(),
        ..WebhookRequest::default()
    };
    assert_eq!(
        host.verify_webhook(&req),
        Err(WebhookError::MissingSignature(FAKE_WEBHOOK_HEADER))
    );
    req.headers
        .insert(FAKE_WEBHOOK_HEADER.into(), "wrong".into());
    assert_eq!(
        host.verify_webhook(&req),
        Err(WebhookError::InvalidSignature)
    );
    req.headers
        .insert(FAKE_WEBHOOK_HEADER.into(), "s3cret".into());
    assert_eq!(host.verify_webhook(&req), Ok(()));
    assert_eq!(host.parse_webhook(&req), Ok(CodeHostEvent::Ping));
}

// ------------------------------------------------------------ decision log

#[tokio::test]
async fn decision_log_pairs_latest_decision_with_latest_label() {
    let log = InMemoryStore::new();
    let first = log
        .record_decision(&decision("SHOP-1", "2026-10-01T00:00:00Z"))
        .await
        .unwrap();
    let second = log
        .record_decision(&decision("SHOP-1", "2026-10-02T00:00:00Z"))
        .await
        .unwrap();
    log.record_decision(&decision("SHOP-2", "2026-10-03T00:00:00Z"))
        .await
        .unwrap();
    assert!(second > first);
    log.record_human_label(&label("SHOP-1", "2026-10-05T00:00:00Z"))
        .await
        .unwrap();

    let all = log
        .list_for_calibration(&CalibrationQuery::default())
        .await
        .unwrap();
    let keys: Vec<_> = all.iter().map(|r| r.decision.ticket_key.as_str()).collect();
    assert_eq!(keys, ["SHOP-2", "SHOP-1"]);
    assert_eq!(all[1].decision_id, second);
    assert!(all[0].human.is_none());
    assert!(all[1].human.is_some());

    let labeled = log
        .list_for_calibration(&CalibrationQuery {
            labeled_only: true,
            ..CalibrationQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(labeled.len(), 1);

    let since = log
        .list_for_calibration(&CalibrationQuery {
            since: Some("2026-10-03T00:00:00Z".into()),
            limit: 1,
            ..CalibrationQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(since.len(), 1);
    assert_eq!(since[0].decision.ticket_key, "SHOP-2");
}

#[tokio::test]
async fn gate_history_is_idempotent_per_run_and_newest_first() {
    let store = InMemoryStore::new();
    let outcome = ReportOutcome::default();
    let a = store
        .record_gate_result(&gate_report("run-1", 12), &outcome)
        .await
        .unwrap();
    let again = store
        .record_gate_result(&gate_report("run-1", 12), &outcome)
        .await
        .unwrap();
    assert_eq!(a, again);
    store
        .record_gate_result(&gate_report("run-2", 13), &outcome)
        .await
        .unwrap();
    let recent = store.recent_gate_results(10).await.unwrap();
    let runs: Vec<_> = recent.iter().map(|r| r.report.run_id.as_str()).collect();
    assert_eq!(runs, ["run-2", "run-1"]);
    assert_eq!(store.recent_gate_results(1).await.unwrap().len(), 1);
    let latest = store.latest_for_pr("acme/shop", 12).await.unwrap().unwrap();
    assert_eq!(latest.report.run_id, "run-1");
    assert!(
        store
            .latest_for_pr("acme/shop", 99)
            .await
            .unwrap()
            .is_none()
    );
}

// ------------------------------------------------------------ evidence store

#[tokio::test]
async fn evidence_store_keeps_objects_and_presigns_fake_urls() {
    let store = InMemoryEvidenceStore::new();
    let obj = store
        .put_object("shop/run-1/a.png", vec![1, 2], "image/png")
        .await
        .unwrap();
    assert_eq!(obj.size, 2);
    assert_eq!(
        store.object("shop/run-1/a.png"),
        Some((vec![1, 2], "image/png".to_string()))
    );
    assert_eq!(store.get_object("shop/run-1/a.png").await, Ok(vec![1, 2]));
    assert_eq!(
        store.get_object("shop/run-1/missing.png").await,
        Err(EvidenceError::NotFound("shop/run-1/missing.png".into()))
    );
    assert_eq!(
        store
            .presign_get("shop/run-1/a.png", Duration::from_secs(60))
            .unwrap(),
        "memory://evidence/shop/run-1/a.png?expires=60"
    );
    assert!(matches!(
        store.presign_get("a.png", Duration::ZERO),
        Err(EvidenceError::InvalidExpiry(_))
    ));
    assert!(matches!(
        store.presign_get("a.png", MAX_PRESIGN_EXPIRY + Duration::from_secs(1)),
        Err(EvidenceError::InvalidExpiry(_))
    ));
    for bad in ["", "/abs", "a/../b"] {
        assert!(matches!(
            store.put_object(bad, vec![], "x").await,
            Err(EvidenceError::InvalidKey(_))
        ));
    }
}

// ------------------------------------------------------------ impact analyzer

#[test]
fn impact_analyzer_returns_scripted_facts() {
    let analyzer = FakeImpactAnalyzer::new()
        .with_fan_out("src/cart.ts", 5)
        .with_fan_out("src/util.ts", 20)
        .with_human_only("infra/main.tf")
        .with_area("src/cart", "checkout")
        .with_mention("CartService", "src/cart.ts");
    let facts = analyzer
        .fan_out(&[
            "src/cart.ts".into(),
            "infra/main.tf".into(),
            "new.ts".into(),
        ])
        .unwrap();
    assert_eq!(facts.max_fan_out, 5);
    assert_eq!(facts.fan_out["new.ts"], 0);
    assert!(facts.human_only_path_hit);
    assert_eq!(facts.human_only_paths, ["infra/main.tf"]);
    assert!(!facts.schema_migration);
    let migration = analyzer.fan_out(&["db/migrations/001.sql".into()]).unwrap();
    assert!(migration.schema_migration);
    assert_eq!(
        analyzer.areas_for(&["src/cart.ts".into(), "src/cart/x.ts".into()]),
        [Area::new("checkout")]
    );
    assert_eq!(
        analyzer.resolve_mentions(&["CartService".into(), "Nope".into()]),
        ["src/cart.ts"]
    );
}
