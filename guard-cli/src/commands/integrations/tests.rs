use super::*;
use std::time::Instant;

fn facts() -> Facts {
    Facts {
        tracker: None,
        jira_base_url: None,
        jira_email: None,
        jira_token: None,
        server_url: None,
        thresholds: Ok(None),
        jev_key: false,
        jev_fake: false,
        agents: None,
        gateway_override: None,
        database_url: None,
        agents_md: false,
        code_host: None,
        ci: Vec::new(),
        app_url: "http://127.0.0.1:3000/".into(),
        repo: "acme/shop".into(),
    }
}

fn jira_facts() -> Facts {
    Facts {
        tracker: Some(Some("SHOP".into())),
        jira_base_url: Some("https://acme.atlassian.net".into()),
        jira_email: Some("qa@acme.test".into()),
        jira_token: Some("s3cr3t-token".into()),
        ..facts()
    }
}

fn find<'a>(all: &'a [Integration], id: &str) -> &'a Integration {
    all.iter().find(|i| i.id == id).expect(id)
}

#[test]
fn every_flow_node_with_an_integration_is_reported_in_a_stable_order() {
    let all = assemble(&facts(), &Probed::default());
    let ids: Vec<&str> = all.iter().map(|i| i.id).collect();
    assert_eq!(
        ids,
        [
            "jira",
            "server",
            "jev",
            "gatekeeper",
            "agents",
            "gateway",
            "skills",
            "local",
            "code_host",
            "ci"
        ]
    );
    assert_eq!(find(&all, "agents").nodes, ["qa", "seal", "coder"]);
    assert_eq!(find(&all, "code_host").nodes, ["direct", "devpr"]);
    assert_eq!(find(&all, "ci").nodes, ["guard"]);
}

#[test]
fn nothing_configured_says_so_with_a_hint_and_probes_only_the_app() {
    let f = facts();
    // Only the local app's own readiness URL is probed.
    assert_eq!(plan(&f), vec![ProbeId::App]);
    let all = assemble(&f, &Probed::default());
    for id in [
        "jira",
        "server",
        "jev",
        "agents",
        "gateway",
        "code_host",
        "ci",
    ] {
        let i = find(&all, id);
        assert_eq!(i.state, State::NotConfigured, "{id}: {}", i.detail);
        assert!(i.configure.is_some(), "{id} has no hint");
    }
    assert!(
        find(&all, "jira")
            .configure
            .as_deref()
            .unwrap()
            .contains("guard connect jira")
    );
}

#[test]
fn jira_names_what_is_missing_by_variable_name_only() {
    let f = Facts {
        tracker: Some(Some("SHOP".into())),
        jira_base_url: Some("https://acme.atlassian.net".into()),
        ..facts()
    };
    let jira = find(&assemble(&f, &Probed::default()), "jira").clone();
    assert_eq!(jira.state, State::NotConfigured);
    assert!(jira.detail.contains("JIRA_EMAIL"), "{}", jira.detail);
    assert!(jira.detail.contains("JIRA_API_TOKEN"), "{}", jira.detail);
}

#[test]
fn jira_maps_each_probe_outcome_and_never_shows_the_token() {
    let f = jira_facts();
    assert_eq!(plan(&f), vec![ProbeId::Jira, ProbeId::App]);
    let cases = [
        (Probe::Ok("Ada QA".into()), State::Connected),
        (Probe::Rejected(401), State::Error),
        (Probe::Http(404), State::Error),
        (
            Probe::Unreachable("connection refused".into()),
            State::Disconnected,
        ),
        (Probe::TimedOut, State::Disconnected),
    ];
    for (probe, state) in cases {
        let probed = Probed {
            jira: Some(probe.clone()),
            ..Probed::default()
        };
        let jira = find(&assemble(&f, &probed), "jira").clone();
        assert_eq!(jira.state, state, "{probe:?}: {}", jira.detail);
        let json = serde_json::to_string(&jira).unwrap();
        assert!(!json.contains("s3cr3t"), "{json}");
    }
    let ok = Probed {
        jira: Some(Probe::Ok("Ada QA".into())),
        ..Probed::default()
    };
    let detail = find(&assemble(&f, &ok), "jira").detail.clone();
    assert!(
        detail.contains("Ada QA") && detail.contains("SHOP"),
        "{detail}"
    );
}

#[test]
fn a_jira_site_over_plain_http_is_never_probed_with_the_token() {
    let f = Facts {
        jira_base_url: Some("http://acme.atlassian.net".into()),
        ..jira_facts()
    };
    assert_eq!(plan(&f), vec![ProbeId::App]);
    let jira = find(&assemble(&f, &Probed::default()), "jira").clone();
    assert_eq!(jira.state, State::Error, "{}", jira.detail);
    assert!(jira.detail.contains("https"), "{}", jira.detail);
    // Plain http to this machine (a local stub) is still probed.
    let local = Facts {
        jira_base_url: Some("http://127.0.0.1:8080".into()),
        ..jira_facts()
    };
    assert_eq!(plan(&local), vec![ProbeId::Jira, ProbeId::App]);
}

#[test]
fn only_tasks_with_a_live_lease_count_as_running() {
    // The queue's own definition of a live task (pg_queue.rs): a crashed
    // worker's expired lease is reclaimable, not running.
    let sql = RUNNING_TASKS_SQL
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        sql.contains("state = 'running' AND locked_until > clock_timestamp()"),
        "{sql}"
    );
}

#[test]
fn jira_without_a_project_key_is_configured_not_connected() {
    let f = Facts {
        tracker: Some(None),
        ..jira_facts()
    };
    let probed = Probed {
        jira: Some(Probe::Ok("Ada QA".into())),
        ..Probed::default()
    };
    let jira = find(&assemble(&f, &probed), "jira").clone();
    assert_eq!(jira.state, State::Configured);
    assert!(jira.detail.contains("[tracker].project"), "{}", jira.detail);
}

#[test]
fn the_gatekeeper_and_triage_follow_guard_serve() {
    let f = Facts {
        server_url: Some("http://127.0.0.1:8080".into()),
        thresholds: Ok(Some(("thresholds.toml".into(), Mode::Active))),
        ..facts()
    };
    assert!(plan(&f).contains(&ProbeId::Server));
    let up = Probed {
        server: Some(Probe::Ok(
            r#"{"status":"ok","version":"0.1.0","checks":{"database":"ok"}}"#.into(),
        )),
        ..Probed::default()
    };
    let all = assemble(&f, &up);
    assert_eq!(find(&all, "server").state, State::Connected);
    assert!(find(&all, "server").detail.contains("0.1.0"));
    let gate = find(&all, "gatekeeper");
    assert_eq!(gate.state, State::Connected);
    assert!(gate.detail.contains("active"), "{}", gate.detail);
    assert_eq!(find(&all, "jev").state, State::Configured);

    let down = Probed {
        server: Some(Probe::Unreachable("connection refused".into())),
        ..Probed::default()
    };
    let all = assemble(&f, &down);
    assert_eq!(find(&all, "server").state, State::Disconnected);
    assert_eq!(find(&all, "gatekeeper").state, State::Disconnected);

    // Without [server] the gatekeeper is not running anywhere.
    let none = assemble(&facts(), &Probed::default());
    let gate = find(&none, "gatekeeper");
    assert_eq!(gate.state, State::NotConfigured);
    assert!(gate.detail.contains("Shadow"), "{}", gate.detail);
}

#[test]
fn an_invalid_thresholds_file_is_an_error() {
    let f = Facts {
        thresholds: Err("version must be 1".into()),
        ..facts()
    };
    let gate = find(&assemble(&f, &Probed::default()), "gatekeeper").clone();
    assert_eq!(gate.state, State::Error);
}

#[test]
fn a_local_jev_key_or_the_fake_engine_counts_as_configured() {
    for f in [
        Facts {
            jev_key: true,
            ..facts()
        },
        Facts {
            jev_fake: true,
            ..facts()
        },
    ] {
        assert_eq!(
            find(&assemble(&f, &Probed::default()), "jev").state,
            State::Configured
        );
    }
}

#[test]
fn the_agent_lane_is_working_while_a_task_runs() {
    let f = Facts {
        agents: Some(Ok("http://127.0.0.1:4000".into())),
        database_url: Some("postgres://u:pw@db/guard".into()),
        ..facts()
    };
    assert!(plan(&f).contains(&ProbeId::Queue));
    assert!(plan(&f).contains(&ProbeId::Gateway));
    let task = AgentTask {
        ticket: "SHOP-42".into(),
        role: "qa".into(),
        branch: "agent/SHOP-42".into(),
        attempts: 1,
    };
    let busy = Probed {
        queue: Some(Ok(vec![task.clone()])),
        ..Probed::default()
    };
    let agents = find(&assemble(&f, &busy), "agents").clone();
    assert_eq!(agents.state, State::Working);
    assert_eq!(agents.tasks, vec![task]);
    assert!(agents.detail.contains("SHOP-42"), "{}", agents.detail);
    let json = serde_json::to_string(&agents).unwrap();
    assert!(!json.contains("pw@"), "{json}");

    let idle = Probed {
        queue: Some(Ok(Vec::new())),
        ..Probed::default()
    };
    assert_eq!(find(&assemble(&f, &idle), "agents").state, State::Connected);
    let broken = Probed {
        queue: Some(Err("timed out".into())),
        ..Probed::default()
    };
    assert_eq!(
        find(&assemble(&f, &broken), "agents").state,
        State::Disconnected
    );
}

#[test]
fn the_agent_lane_without_a_postgres_queue_is_not_configured() {
    let f = Facts {
        agents: Some(Ok("http://127.0.0.1:4000".into())),
        database_url: Some("sqlite://x".into()),
        ..facts()
    };
    assert!(!plan(&f).contains(&ProbeId::Queue));
    let agents = find(&assemble(&f, &Probed::default()), "agents").clone();
    assert_eq!(agents.state, State::NotConfigured);
    assert!(agents.detail.contains("DATABASE_URL"), "{}", agents.detail);
    let bad = Facts {
        agents: Some(Err("agents.toml: no agents".into())),
        ..facts()
    };
    assert_eq!(
        find(&assemble(&bad, &Probed::default()), "agents").state,
        State::Error
    );
}

#[test]
fn the_gateway_override_wins_and_is_probed() {
    let f = Facts {
        agents: Some(Ok("http://litellm:4000".into())),
        gateway_override: Some("http://127.0.0.1:4001".into()),
        ..facts()
    };
    assert_eq!(gateway_url(&f).as_deref(), Some("http://127.0.0.1:4001"));
    let up = Probed {
        gateway: Some(Probe::Ok("I'm alive!".into())),
        ..Probed::default()
    };
    assert_eq!(find(&assemble(&f, &up), "gateway").state, State::Connected);
    let down = Probed {
        gateway: Some(Probe::TimedOut),
        ..Probed::default()
    };
    assert_eq!(
        find(&assemble(&f, &down), "gateway").state,
        State::Disconnected
    );
}

#[test]
fn ci_is_configured_only_with_a_workflow_that_runs_guard_gate() {
    let f = Facts {
        ci: vec![".github/workflows/guard.yml".into()],
        ..facts()
    };
    let ci = find(&assemble(&f, &Probed::default()), "ci").clone();
    assert_eq!(ci.state, State::Configured);
    assert!(ci.detail.contains("guard.yml"));
    assert!(runs_guard_gate("run: guard gate plan --shards 4"));
    assert!(runs_guard_gate(
        "include:\n  - component: $CI_SERVER_FQDN/qa/guard-ci/gate@1.0"
    ));
    assert!(!runs_guard_gate("run: cargo test"));
}

#[test]
fn ci_files_scan_github_and_gitlab_without_reading_anything_else() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
    std::fs::write(root.join(".github/workflows/ci.yml"), "run: cargo test").unwrap();
    std::fs::write(
        root.join(".github/workflows/guard.yaml"),
        "run: guard gate finalize",
    )
    .unwrap();
    std::fs::write(root.join(".gitlab-ci.yml"), "script: guard gate plan").unwrap();
    std::fs::create_dir_all(root.join("api")).unwrap();
    std::fs::write(root.join("api/.gitlab-ci.yml"), "script: guard gate shard").unwrap();
    assert_eq!(
        ci_files(root, &["api".to_string(), "missing".to_string()]),
        [
            ".github/workflows/guard.yaml",
            ".gitlab-ci.yml",
            "api/.gitlab-ci.yml"
        ]
    );
}

#[test]
fn code_host_maps_its_token_check() {
    let f = Facts {
        code_host: Some(CodeHostFacts {
            kind: CodeHostKind::Github,
            api_url: "https://api.github.com".into(),
            token: None,
        }),
        ..facts()
    };
    let host = find(&assemble(&f, &Probed::default()), "code_host").clone();
    assert_eq!(host.state, State::NotConfigured);
    assert!(host.detail.contains("GITHUB_TOKEN"));
    let f = Facts {
        code_host: Some(CodeHostFacts {
            kind: CodeHostKind::Github,
            api_url: "https://api.github.com".into(),
            token: Some("ghp_x".into()),
        }),
        ..facts()
    };
    let ok = Probed {
        code_host: Some(Probe::Ok("octo".into())),
        ..Probed::default()
    };
    assert_eq!(
        find(&assemble(&f, &ok), "code_host").state,
        State::Connected
    );
}

#[test]
fn the_local_app_reports_readiness() {
    let up = Probed {
        app: Some(Probe::Ok(String::new())),
        ..Probed::default()
    };
    assert_eq!(
        find(&assemble(&facts(), &up), "local").state,
        State::Connected
    );
    let down = Probed {
        app: Some(Probe::Unreachable("connection refused".into())),
        ..Probed::default()
    };
    let local = find(&assemble(&facts(), &down), "local").clone();
    assert_eq!(local.state, State::Disconnected);
    assert_eq!(local.configure.as_deref(), Some("guard up"));
}

#[test]
fn the_text_view_lists_one_line_per_integration() {
    let text = render(&Report {
        integrations: assemble(&facts(), &Probed::default()),
    });
    assert!(text.starts_with("INTEGRATION"), "{text}");
    assert!(text.contains("jira"), "{text}");
    assert!(text.contains("not configured"), "{text}");
}

/// Probes that answer after a delay, to check the timeout and parallelism.
struct Slow {
    delay: Duration,
}

#[async_trait::async_trait]
impl Prober for Slow {
    async fn jira(&self, _: &str, _: &str, _: &str) -> Probe {
        tokio::time::sleep(self.delay).await;
        Probe::Ok("Ada".into())
    }
    async fn get(&self, _: &str) -> Probe {
        tokio::time::sleep(self.delay).await;
        Probe::Ok("{}".into())
    }
    async fn code_host(&self, _: CodeHostKind, _: &str, _: &str) -> Probe {
        tokio::time::sleep(self.delay).await;
        Probe::Ok("octo".into())
    }
    async fn queue(&self, _: &str, _: &str) -> Result<Vec<AgentTask>, String> {
        tokio::time::sleep(self.delay).await;
        Ok(Vec::new())
    }
}

fn everything() -> Facts {
    Facts {
        server_url: Some("http://127.0.0.1:9".into()),
        agents: Some(Ok("http://127.0.0.1:9".into())),
        database_url: Some("postgres://db/guard".into()),
        code_host: Some(CodeHostFacts {
            kind: CodeHostKind::Gitlab,
            api_url: "https://gitlab.com/api/v4".into(),
            token: Some("glpat".into()),
        }),
        ..jira_facts()
    }
}

#[tokio::test]
async fn probes_run_in_parallel() {
    let started = Instant::now();
    let probed = collect(
        &everything(),
        &Slow {
            delay: Duration::from_millis(300),
        },
        Duration::from_secs(5),
    )
    .await;
    // Six probes of 300 ms each: well under their 1.8 s sum.
    assert!(started.elapsed() < Duration::from_millis(1200));
    assert_eq!(probed.jira, Some(Probe::Ok("Ada".into())));
    assert_eq!(probed.queue, Some(Ok(Vec::new())));
}

#[tokio::test]
async fn a_slow_probe_times_out_instead_of_blocking() {
    let started = Instant::now();
    let probed = collect(
        &everything(),
        &Slow {
            delay: Duration::from_secs(10),
        },
        Duration::from_millis(100),
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(probed.jira, Some(Probe::TimedOut));
    assert_eq!(probed.server, Some(Probe::TimedOut));
    assert!(matches!(probed.queue, Some(Err(_))));
}

mod network {
    //! The real prober against local stubs only.
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn get_returns_the_body_and_maps_statuses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/healthz"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"status":"ok"}"#))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/locked"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let net = Network::new(Duration::from_secs(2));
        assert_eq!(
            net.get(&format!("{}/healthz", server.uri())).await,
            Probe::Ok(r#"{"status":"ok"}"#.into())
        );
        assert_eq!(
            net.get(&format!("{}/locked", server.uri())).await,
            Probe::Rejected(401)
        );
        assert_eq!(
            net.get(&format!("{}/missing", server.uri())).await,
            Probe::Http(404)
        );
    }

    #[tokio::test]
    async fn an_unreachable_host_is_reported_without_its_url() {
        let net = Network::new(Duration::from_secs(2));
        // Port 9 (discard) is closed on a test machine.
        match net.get("http://127.0.0.1:9/healthz").await {
            Probe::Unreachable(reason) => assert!(!reason.contains("127.0.0.1"), "{reason}"),
            other => assert_eq!(other, Probe::TimedOut),
        }
    }

    #[tokio::test]
    async fn jira_checks_myself_with_basic_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/myself"))
            .and(header("authorization", "Basic cWFAYWNtZS50ZXN0OnRvaw=="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accountId": "1", "displayName": "Ada QA", "emailAddress": "qa@acme.test"
            })))
            .mount(&server)
            .await;
        let net = Network::new(Duration::from_secs(2));
        assert_eq!(
            net.jira(&server.uri(), "qa@acme.test", "tok").await,
            Probe::Ok("Ada QA".into())
        );
        assert_eq!(
            net.jira(&server.uri(), "qa@acme.test", "wrong").await,
            Probe::Http(404)
        );
    }

    #[tokio::test]
    async fn a_queue_url_that_is_not_postgres_never_connects() {
        let net = Network::new(Duration::from_secs(2));
        let error = net
            .queue("mysql://u:pw@db/x", "acme/shop")
            .await
            .unwrap_err();
        assert!(!error.contains("pw"), "{error}");
    }
}
