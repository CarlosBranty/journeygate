use super::*;

const REPORT: &str = r#"{
  "integrations": [
    {"id": "jira", "nodes": ["jira"], "label": "Jira", "state": "connected",
     "detail": "authenticated as Ada at acme.atlassian.net; project SHOP"},
    {"id": "gatekeeper", "nodes": ["gate"], "label": "Gatekeeper", "state": "not_configured",
     "detail": "runs inside guard serve", "configure": "add [server] url"},
    {"id": "agents", "nodes": ["qa", "seal", "coder"], "label": "Agent lane", "state": "working",
     "detail": "1 agent task(s) running",
     "tasks": [{"ticket": "SHOP-42", "role": "qa", "branch": "agent/SHOP-42", "attempts": 1}]},
    {"id": "ci", "nodes": ["guard"], "label": "Remote guard (CI)", "state": "not_configured",
     "detail": "no CI workflow runs guard gate yet", "configure": "add a workflow"},
    {"id": "future", "nodes": ["x"], "label": "New", "state": "something_new", "detail": "?"}
  ]
}"#;

#[test]
fn the_report_parses_and_unknown_states_degrade_to_configured() {
    let report = parse(REPORT).unwrap();
    assert_eq!(report.integrations.len(), 5);
    assert_eq!(report.get("jira").unwrap().state, State::Connected);
    assert_eq!(report.get("future").unwrap().state, State::Unknown);
    assert!(parse("not json").is_err());
}

#[test]
fn nodes_find_their_integration() {
    let report = parse(REPORT).unwrap();
    assert_eq!(report.for_node("seal").unwrap().id, "agents");
    assert_eq!(report.for_node("gate").unwrap().id, "gatekeeper");
    assert!(report.for_node("dev").is_none());
}

#[test]
fn the_agent_lane_counts_its_running_tasks() {
    let report = parse(REPORT).unwrap();
    assert_eq!(report.running_agents(), 1);
    assert_eq!(
        report.get("agents").unwrap().tasks[0].branch,
        "agent/SHOP-42"
    );
    let idle = parse(r#"{"integrations": [{"id": "agents", "nodes": [], "label": "", "state": "connected", "detail": ""}]}"#).unwrap();
    assert_eq!(idle.running_agents(), 0);
}

#[test]
fn states_have_spanish_labels_and_only_working_animates() {
    let labels: Vec<&str> = [
        State::Connected,
        State::Configured,
        State::NotConfigured,
        State::Disconnected,
        State::Error,
        State::Working,
    ]
    .iter()
    .map(|s| s.label())
    .collect();
    assert_eq!(
        labels,
        [
            "Conectado",
            "Configurado",
            "Sin configurar",
            "Desconectado",
            "Error",
            "Trabajando"
        ]
    );
    assert!(State::Working.animates());
    assert!(!State::Connected.animates());
}

#[test]
fn polling_starts_at_once_then_every_30_s_only_while_visible() {
    let t0 = Instant::now();
    assert!(poll_due(None, t0, false));
    assert!(!poll_due(Some(t0), t0 + Duration::from_secs(29), true));
    assert!(poll_due(Some(t0), t0 + Duration::from_secs(30), true));
    assert!(!poll_due(Some(t0), t0 + Duration::from_secs(300), false));
    assert_eq!(
        next_poll_in(Some(t0), t0 + Duration::from_secs(10)),
        Duration::from_secs(20)
    );
    assert_eq!(next_poll_in(None, t0), Duration::ZERO);
}

#[test]
fn nodes_with_settings_the_gui_can_write_have_a_form() {
    assert_eq!(form("jira"), Some(Form::Jira));
    assert_eq!(form("ingest"), Some(Form::Server));
    assert_eq!(form("gate"), Some(Form::Server));
    assert_eq!(form("gateway"), Some(Form::Gateway));
    assert_eq!(form("qa"), None);
    // The roles the local run and the CI guard log in as.
    assert_eq!(form("local"), Some(Form::Accounts));
    assert_eq!(form("guard"), Some(Form::Accounts));
}

#[test]
fn child_env_sets_only_what_the_gui_holds_and_binds_the_token_to_its_host() {
    let empty = IntegrationSettings::default();
    assert!(child_env(&empty, None).is_empty());
    // A saved token without a base URL is never sent anywhere.
    assert!(child_env(&empty, Some("tok")).is_empty());
    let s = IntegrationSettings {
        jira_base_url: " https://acme.atlassian.net/ ".into(),
        jira_email: "qa@acme.test".into(),
        gateway_url: "http://127.0.0.1:4000".into(),
    };
    assert_eq!(
        child_env(&s, Some("tok")),
        vec![
            (
                "JIRA_BASE_URL".to_string(),
                "https://acme.atlassian.net".to_string()
            ),
            ("JIRA_EMAIL".to_string(), "qa@acme.test".to_string()),
            ("JIRA_API_TOKEN".to_string(), "tok".to_string()),
            (
                "GUARD_GATEWAY_ADMIN_URL".to_string(),
                "http://127.0.0.1:4000".to_string()
            ),
        ]
    );
    // The GUI's base URL without its own token blanks an inherited one, so a
    // token meant for another site is never sent to this one.
    let env = child_env(&s, None);
    assert!(env.contains(&("JIRA_API_TOKEN".to_string(), String::new())));
    assert_eq!(s.jira_host().as_deref(), Some("acme.atlassian.net"));
    assert_eq!(empty.jira_host(), None);
}

#[test]
fn urls_must_be_https_or_local_http_without_credentials() {
    assert_eq!(
        check_url("https://acme.atlassian.net/").unwrap(),
        "https://acme.atlassian.net"
    );
    assert!(check_url("http://localhost:8080").is_ok());
    assert!(check_url("http://127.0.0.1:4000").is_ok());
    assert!(check_url("http://guard.corp").is_err());
    assert!(check_url("https://user:pw@acme.atlassian.net").is_err());
    assert!(check_url("acme.atlassian.net").is_err());
    assert!(check_url("https://a b").is_err());
}

#[test]
fn project_keys_and_emails_are_checked() {
    assert!(check_project_key("SHOP").is_ok());
    assert!(check_project_key("TOTPR_2").is_ok());
    assert!(check_project_key("shop").is_err());
    assert!(check_project_key("1SHOP").is_err());
    assert!(check_project_key("").is_err());
    assert!(check_email("qa@acme.test").is_ok());
    assert!(check_email("qa acme").is_err());
}

const TARGET: &str = "version = 1\n\n[app]\nname = \"shop\"\nbase_url = \"http://127.0.0.1:3000\"\n\n[areas.web]\ndescription = \"Web\"\npaths = [\"web/**\"]\n";

#[test]
fn target_toml_edits_set_tracker_and_server_and_stay_valid() {
    let with_tracker = set_tracker_project(TARGET, "SHOP").unwrap();
    assert!(with_tracker.contains("[tracker]\nkind = \"jira\"\nproject = \"SHOP\""));
    assert!(with_tracker.starts_with("version = 1\n"));
    let with_server = set_server_url(&with_tracker, "https://guard.acme.test").unwrap();
    assert!(with_server.contains("[server]\nurl = \"https://guard.acme.test\""));
    let again = set_tracker_project(&with_server, "OPS").unwrap();
    assert!(again.contains("project = \"OPS\""));
    assert!(!again.contains("SHOP"));
    assert!(set_tracker_project(TARGET, "bad key").is_err());
    assert!(set_server_url(TARGET, "ftp://x").is_err());
    assert!(set_tracker_project("not = [toml", "SHOP").is_err());
}

#[test]
fn a_jira_site_that_fails_the_url_check_never_gets_the_token() {
    let s = IntegrationSettings {
        jira_base_url: "http://acme.atlassian.net".into(),
        jira_email: "qa@acme.test".into(),
        gateway_url: String::new(),
    };
    // The token saved for https://acme.atlassian.net shares the host.
    assert!(!jira_token_allowed(&s));
    let env = child_env(&s, Some("tok"));
    assert!(
        !env.iter()
            .any(|(k, _)| k == "JIRA_BASE_URL" || k == "JIRA_API_TOKEN"),
        "{env:?}"
    );
    let https = IntegrationSettings {
        jira_base_url: "https://acme.atlassian.net".into(),
        ..s.clone()
    };
    assert!(jira_token_allowed(&https));
    // Plain http, even to this machine, is no place for a saved token.
    let local = IntegrationSettings {
        jira_base_url: "http://localhost:8080".into(),
        ..s
    };
    assert!(!jira_token_allowed(&local));
    assert!(
        !child_env(&local, Some("tok"))
            .iter()
            .any(|(_, v)| v == "tok")
    );
}

#[test]
fn edits_reach_the_settings_only_once_they_pass_the_checks() {
    let mut s = IntegrationSettings {
        jira_base_url: "https://acme.atlassian.net".into(),
        jira_email: "qa@acme.test".into(),
        gateway_url: "http://127.0.0.1:4000".into(),
    };
    let before = s.clone();
    assert!(
        s.apply_jira("http://acme.atlassian.net", "qa@acme.test")
            .is_err()
    );
    assert!(
        s.apply_jira("https://ops.atlassian.net", "not an email")
            .is_err()
    );
    assert!(s.apply_gateway("http://litellm.corp").is_err());
    assert_eq!(s, before);
    s.apply_jira(" https://ops.atlassian.net/ ", " ops@acme.test ")
        .unwrap();
    assert_eq!(s.jira_base_url, "https://ops.atlassian.net");
    assert_eq!(s.jira_email, "ops@acme.test");
    s.apply_gateway("").unwrap();
    assert_eq!(s.gateway_url, "");
    // A blank site clears the GUI's Jira override (guard's own config applies).
    s.apply_jira("", "").unwrap();
    assert_eq!(s.jira_base_url, "");
}

fn lane(tasks: &str) -> Report {
    parse(&format!(
        r#"{{"integrations": [{{"id": "agents", "nodes": ["qa", "seal", "coder"], "label": "Agent lane",
            "state": "working", "detail": "", "tasks": [{tasks}]}}]}}"#
    ))
    .unwrap()
}

#[test]
fn each_agent_role_works_only_on_its_own_nodes() {
    let qa = lane(r#"{"ticket": "SHOP-1", "role": "qa", "branch": "agent/SHOP-1"}"#);
    assert_eq!(qa.node_state("qa"), Some(State::Working));
    assert_eq!(qa.node_state("seal"), Some(State::Working));
    assert_eq!(qa.node_state("coder"), Some(State::Connected));
    assert_eq!(qa.working_lane_nodes(), ["qa", "seal"].into());
    let coder = lane(r#"{"ticket": "SHOP-2", "role": "coder", "branch": "agent/SHOP-2"}"#);
    assert_eq!(coder.node_state("qa"), Some(State::Connected));
    assert_eq!(coder.node_state("coder"), Some(State::Working));
    assert_eq!(coder.working_lane_nodes(), ["coder"].into());
    // A role this GUI does not know still shows the lane at work.
    let other = lane(r#"{"ticket": "SHOP-3", "role": "reviewer", "branch": "agent/SHOP-3"}"#);
    assert_eq!(other.working_lane_nodes(), ["qa", "seal", "coder"].into());
    let report = parse(REPORT).unwrap();
    assert_eq!(report.node_state("gate"), Some(State::NotConfigured));
    assert_eq!(report.node_state("dev"), None);
}

#[test]
fn a_jira_probe_run_with_the_guis_own_settings_is_not_shown_as_connected() {
    let mut report = parse(REPORT).unwrap();
    mark_gui_jira_probe(&mut report);
    let jira = report.get("jira").unwrap();
    assert_eq!(jira.state, State::Configured);
    assert!(jira.gui_probe);
    assert!(!report.get("ci").unwrap().gui_probe);
    // A failure is still shown as it is.
    let mut failed = parse(
        r#"{"integrations": [{"id": "jira", "nodes": ["jira"], "state": "error", "detail": "x"}]}"#,
    )
    .unwrap();
    mark_gui_jira_probe(&mut failed);
    assert_eq!(failed.get("jira").unwrap().state, State::Error);
}

#[test]
fn integrations_read_in_spanish_and_only_commands_are_copyable() {
    let report = parse(REPORT).unwrap();
    let ci = report.get("ci").unwrap();
    assert_eq!(spanish_label(ci), "Guard remoto (CI)");
    assert_eq!(
        spanish_label(report.get("agents").unwrap()),
        "Carril de agentes"
    );
    // An integration a newer guard adds keeps guard's own label.
    assert_eq!(spanish_label(report.get("future").unwrap()), "New");
    let summary = summary(ci);
    assert!(summary.contains("CI"), "{summary}");
    assert!(!summary.contains("workflow runs"), "{summary}");
    assert_eq!(
        copy_command(
            "guard connect jira --base-url https://<site>.atlassian.net --email <you@company.com> --project <KEY>"
        ),
        Some(
            "guard connect jira --base-url https://<site>.atlassian.net --email <you@company.com> --project <KEY>"
        )
    );
    assert_eq!(copy_command("guard up"), Some("guard up"));
    assert_eq!(copy_command("guard agents sync"), Some("guard agents sync"));
    for prose in [
        "add a workflow that runs guard gate plan/shard/finalize (see .github/workflows/guard.yml)",
        "guard serve (on the server host)",
        "guard connect github | guard connect gitlab",
        "set DATABASE_URL=postgres://... (the queue guard serve and the workers share)",
    ] {
        assert_eq!(copy_command(prose), None, "{prose}");
    }
}
