//! The `guard init` plan: how it is printed and how the user confirms or
//! changes it, one plain line prompt per item (cli.md "guard init", Flow).

use super::prompt::{Answer, PlanLine, Prompter};
use crate::cli::RunnerMode;
use guard_core::config::parse_target;
use guard_core::detect::{
    Confidence, DetectionReport, InitPlan, render_self_target_toml, render_target_toml,
};
use std::io;

fn confidence(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "medium",
        Confidence::Low => "low",
    }
}

/// The detected plan, one line per fact, evidence and note.
pub fn plan_lines(report: &DetectionReport, plan: &InitPlan, io_notes: &[String]) -> Vec<PlanLine> {
    let mut lines = Vec::new();
    let fact = |lines: &mut Vec<PlanLine>, text: String| lines.push(PlanLine::Fact(text));
    let evidence =
        |lines: &mut Vec<PlanLine>, text: &String| lines.push(PlanLine::Evidence(text.clone()));
    if let Some(compose) = &report.compose
        && let Some(start) = &compose.start
    {
        fact(
            &mut lines,
            format!(
                "infra      {}   ({})",
                start.value,
                confidence(start.confidence)
            ),
        );
        for line in &start.evidence {
            evidence(&mut lines, line);
        }
    }
    for service in &report.services {
        let pm = service
            .package_manager
            .as_ref()
            .map(|g| {
                format!(
                    "/{}",
                    serde_json::to_value(g.value)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        fact(
            &mut lines,
            format!(
                "service    {}  cwd={}  {}{pm}  start=\"{}\"  ready={}  ({})",
                service.name,
                service.root,
                service.framework.value.as_str(),
                service.start.value,
                service.ready_url,
                confidence(service.confidence)
            ),
        );
        let all = service
            .framework
            .evidence
            .iter()
            .chain(
                service
                    .package_manager
                    .iter()
                    .flat_map(|g| g.evidence.iter()),
            )
            .chain(&service.start.evidence)
            .chain(&service.port.evidence)
            .chain(&service.ready_path.evidence);
        for line in all {
            evidence(&mut lines, line);
        }
    }
    let infra_shown = report.compose.as_ref().is_some_and(|c| c.start.is_some());
    if report.services.is_empty()
        && !infra_shown
        && let Some(start) = &plan.start
    {
        fact(&mut lines, format!("start      {start}"));
    }
    fact(
        &mut lines,
        format!(
            "base_url   {}   ready_path {}   ({})",
            plan.base_url,
            plan.ready_path,
            report.base_url.evidence.join("; ")
        ),
    );
    let languages = if plan.languages.is_empty() {
        "none".to_string()
    } else {
        plan.languages.join(", ")
    };
    fact(&mut lines, format!("languages  {languages}"));
    fact(
        &mut lines,
        format!(
            "code host  {}",
            plan.code_host.map_or("none", |c| c.as_str())
        ),
    );
    if let Some(project) = &plan.ticket_project {
        fact(&mut lines, format!("ticket     {project}-\\d+"));
    }
    let areas: Vec<&str> = plan.areas.iter().map(|a| a.key.as_str()).collect();
    fact(
        &mut lines,
        format!("areas      {} (edit their descriptions)", areas.join(", ")),
    );

    let mut note = |text: String| lines.push(PlanLine::Note(text));
    for ignored in &report.ignored_compose_files {
        note(format!("ignored {} ({})", ignored.file, ignored.reason));
    }
    for service in &report.services {
        if let Some(pm) = &service.package_manager
            && pm.confidence == Confidence::Medium
        {
            for line in &pm.evidence {
                note(line.clone());
            }
        }
        if let Some(compose) = &service.also_in_compose {
            note(format!(
                "{} is also available in compose ({compose}); using the native dev command",
                service.name
            ));
        }
    }
    for skipped in &report.skipped {
        note(format!("skipped {}: {}", skipped.root, skipped.reason));
    }
    for extra in report.notes.iter().chain(io_notes) {
        note(extra.clone());
    }
    lines
}

/// One confirmable value.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    Start,
    Stop,
    ServiceStart(String),
    ServiceReady(String),
    /// Self mode: a service's `ready_url`, kept in the commented block.
    ServiceUrl(String),
    BaseUrl,
    ReadyPath,
    Ticket,
}

impl Item {
    /// `base_url` and `ready_path` cannot be dropped.
    fn optional(&self) -> bool {
        !matches!(self, Item::BaseUrl | Item::ReadyPath)
    }

    fn label(&self) -> String {
        match self {
            Item::Start => "start command".into(),
            Item::Stop => "stop command".into(),
            Item::ServiceStart(name) => format!("service {name} start"),
            Item::ServiceReady(name) => format!("service {name} ready_url"),
            Item::ServiceUrl(name) => format!("service {name} url"),
            Item::BaseUrl => "base_url".into(),
            Item::ReadyPath => "ready_path".into(),
            Item::Ticket => "ticket project".into(),
        }
    }

    /// The current value; `None` when the item no longer exists.
    fn current(&self, plan: &InitPlan) -> Option<Option<String>> {
        let service = |name: &str| plan.services.iter().find(|s| s.name == name);
        Some(match self {
            Item::Start => plan.start.clone(),
            // Gone once the start command was dropped.
            Item::Stop => Some(plan.stop.clone()?),
            Item::ServiceStart(name) => Some(service(name)?.start.clone()),
            Item::ServiceReady(name) | Item::ServiceUrl(name) => service(name)?.ready_url.clone(),
            Item::BaseUrl => Some(plan.base_url.clone()),
            Item::ReadyPath => Some(plan.ready_path.clone()),
            Item::Ticket => plan.ticket_project.clone(),
        })
    }

    /// Sets (`Some`) or drops (`None`) the value. Errors when it cannot be dropped.
    fn apply(&self, plan: &mut InitPlan, value: Option<String>) -> Result<(), String> {
        let required = || format!("{} is required", self.label());
        match (self, value) {
            (Item::Start, None) => {
                plan.start = None;
                plan.stop = None;
            }
            (Item::Start, Some(v)) => plan.start = Some(v),
            (Item::Stop, v) => plan.stop = v,
            (Item::ServiceStart(name), None) => plan.services.retain(|s| &s.name != name),
            (Item::ServiceStart(name), Some(v)) => {
                if let Some(s) = plan.services.iter_mut().find(|s| &s.name == name) {
                    s.start = v;
                }
            }
            (Item::ServiceReady(name) | Item::ServiceUrl(name), v) => {
                if let Some(s) = plan.services.iter_mut().find(|s| &s.name == name) {
                    s.ready_url = v;
                }
            }
            (Item::BaseUrl, Some(v)) => plan.base_url = v,
            (Item::ReadyPath, Some(v)) => plan.ready_path = v,
            (Item::BaseUrl | Item::ReadyPath, None) => return Err(required()),
            (Item::Ticket, None) => plan.ticket_project = None,
            (Item::Ticket, Some(v)) => plan.ticket_project = Some(v),
        }
        Ok(())
    }
}

fn items(plan: &InitPlan, runner: RunnerMode) -> Vec<Item> {
    if runner == RunnerMode::SelfRun {
        let mut items = vec![Item::BaseUrl, Item::ReadyPath];
        items.extend(
            plan.services
                .iter()
                .map(|s| Item::ServiceUrl(s.name.clone())),
        );
        return items;
    }
    let mut items = vec![Item::Start];
    if plan.stop.is_some() {
        items.push(Item::Stop);
    }
    for service in &plan.services {
        items.push(Item::ServiceStart(service.name.clone()));
        items.push(Item::ServiceReady(service.name.clone()));
    }
    items.extend([Item::BaseUrl, Item::ReadyPath]);
    if plan.ticket_project.is_some() {
        items.push(Item::Ticket);
    }
    items
}

/// The `target.toml` that `init` writes for this runner.
pub fn render(plan: &InitPlan, runner: RunnerMode) -> String {
    match runner {
        RunnerMode::Guard => render_target_toml(plan),
        RunnerMode::SelfRun => render_self_target_toml(plan),
    }
}

/// The first reason the rendered `target.toml` would not validate. In self
/// mode the guard shape must validate too, so the commented block still does
/// once it is uncommented.
pub fn invalid(plan: &InitPlan, runner: RunnerMode) -> Option<String> {
    let mut texts = vec![render(plan, runner)];
    if runner == RunnerMode::SelfRun {
        texts.push(render_target_toml(plan));
    }
    texts.iter().find_map(|text| {
        parse_target(text)
            .err()
            .map(|e| e.to_string().lines().next().unwrap_or_default().to_string())
    })
}

/// Asks for each item: Enter keeps it, text replaces it, `-` drops an optional
/// one. An answer that would not validate is refused and asked again. EOF
/// accepts everything left.
pub fn confirm_with(
    plan: &mut InitPlan,
    runner: RunnerMode,
    prompter: &mut dyn Prompter,
) -> io::Result<()> {
    for item in items(plan, runner) {
        loop {
            let Some(current) = item.current(plan) else {
                break;
            };
            let value = match prompter.input(&item.label(), current.as_deref(), item.optional())? {
                Answer::Eof => return Ok(()),
                Answer::Keep => break,
                Answer::Drop => None,
                Answer::Set(v) => Some(v),
            };
            let mut candidate = plan.clone();
            if let Err(message) = item.apply(&mut candidate, value) {
                prompter.error(&message)?;
                continue;
            }
            if let Some(message) = invalid(&candidate, runner) {
                prompter.error(&message)?;
                continue;
            }
            *plan = candidate;
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::prompt::Plain;
    use super::*;
    use crate::cli::RunnerMode;
    use guard_core::config::Service;
    use guard_core::detect::ProposedArea;
    use std::io::{BufRead, Write};

    /// Asks for each item through plain line prompts (see [`confirm_with`]).
    fn confirm(
        plan: &mut InitPlan,
        runner: RunnerMode,
        input: &mut dyn BufRead,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        let mut prompter = Plain {
            input,
            out,
            interactive: true,
        };
        confirm_with(plan, runner, &mut prompter)
    }

    /// `true` for a `y`/`yes` answer; EOF and anything else is no.
    fn ask_yes_no(
        question: &str,
        input: &mut dyn BufRead,
        out: &mut dyn Write,
    ) -> io::Result<bool> {
        Plain {
            input,
            out,
            interactive: true,
        }
        .confirm(question, false)
    }

    fn plan() -> InitPlan {
        InitPlan {
            name: "saas".into(),
            languages: vec!["typescript".into()],
            start: Some("docker compose -f docker-compose.yml up -d --wait db".into()),
            stop: Some("docker compose -f docker-compose.yml down".into()),
            code_host: None,
            ticket_project: Some("SHOP".into()),
            base_url: "http://localhost:3000".into(),
            ready_path: "/".into(),
            services: vec![
                Service {
                    name: "api".into(),
                    cwd: "api".into(),
                    start: "bun run dev".into(),
                    stop: None,
                    ready_url: Some("http://localhost:3001/health".into()),
                },
                Service {
                    name: "web".into(),
                    cwd: "web".into(),
                    start: "bun run dev".into(),
                    stop: None,
                    ready_url: Some("http://localhost:3000/".into()),
                },
            ],
            areas: vec![ProposedArea {
                key: "app".into(),
                dir: String::new(),
            }],
        }
    }

    fn run(answers: &str) -> (InitPlan, String) {
        run_as(RunnerMode::Guard, answers)
    }

    fn run_as(runner: RunnerMode, answers: &str) -> (InitPlan, String) {
        let mut p = plan();
        let mut out = Vec::new();
        confirm(&mut p, runner, &mut answers.as_bytes(), &mut out).unwrap();
        (p, String::from_utf8(out).unwrap())
    }

    #[test]
    fn self_mode_asks_only_the_urls() {
        let (p, out) = run_as(RunnerMode::SelfRun, "\n\n\n\n\n\n");
        assert_eq!(p, plan());
        assert_eq!(
            out,
            "base_url [http://localhost:3000]: ready_path [/]: \
             service api url [http://localhost:3001/health]: \
             service web url [http://localhost:3000/]: "
        );
    }

    #[test]
    fn self_mode_answers_change_the_urls_and_a_dash_drops_a_service_url() {
        let (p, _) = run_as(
            RunnerMode::SelfRun,
            "http://localhost:4000\n/health\n-\nhttp://localhost:4000/\n",
        );
        assert_eq!(p.base_url, "http://localhost:4000");
        assert_eq!(p.ready_path, "/health");
        assert_eq!(p.services[0].ready_url, None);
        assert_eq!(
            p.services[1].ready_url.as_deref(),
            Some("http://localhost:4000/")
        );
        // Commands and the ticket pattern are kept as detected.
        assert_eq!(p.start, plan().start);
        assert_eq!(p.ticket_project, plan().ticket_project);
    }

    #[test]
    fn self_mode_refuses_a_service_url_that_would_not_validate_once_uncommented() {
        let (p, out) = run_as(
            RunnerMode::SelfRun,
            "\n\nnot-a-url\nhttp://localhost:3002/health\n\n",
        );
        assert_eq!(
            p.services[0].ready_url.as_deref(),
            Some("http://localhost:3002/health")
        );
        assert_eq!(out.matches("error: ").count(), 1, "{out}");
    }

    #[test]
    fn render_follows_the_runner() {
        let p = plan();
        assert!(render(&p, RunnerMode::Guard).contains("\nstart = "));
        let own = render(&p, RunnerMode::SelfRun);
        assert!(!own.contains("\nstart = "), "{own}");
        assert!(own.contains("\n# start = "), "{own}");
        assert_eq!(invalid(&p, RunnerMode::SelfRun), None);
    }

    #[test]
    fn enter_accepts_every_value() {
        let (p, out) = run("\n\n\n\n\n\n\n\n\n");
        assert_eq!(p, plan());
        assert!(out.starts_with(
            "start command [docker compose -f docker-compose.yml up -d --wait db]: stop command [docker compose -f docker-compose.yml down]: service api start [bun run dev]: "
        ), "{out}");
        assert!(out.ends_with("ticket project [SHOP]: "), "{out}");
    }

    #[test]
    fn typed_text_replaces_and_a_dash_drops_optional_items() {
        // start, stop, api start, api ready, web start(-), base_url, ready_path, ticket(-)
        let (p, _) = run("\n\nbun run start\n-\n-\nhttp://localhost:3100\n/healthz\n-\n");
        assert_eq!(p.services.len(), 1);
        assert_eq!(p.services[0].start, "bun run start");
        assert_eq!(p.services[0].ready_url, None);
        assert_eq!(p.base_url, "http://localhost:3100");
        assert_eq!(p.ready_path, "/healthz");
        assert_eq!(p.ticket_project, None);
    }

    #[test]
    fn dropping_the_start_also_drops_the_stop_and_skips_its_prompt() {
        let (p, out) = run("-\n");
        assert_eq!((p.start, p.stop), (None, None));
        assert!(out.contains("service api start"), "{out}");
        assert!(!out.contains("stop command"), "{out}");
    }

    #[test]
    fn invalid_answers_are_refused_and_asked_again() {
        let mut answers = String::from("\n\n\n\n\n\n");
        answers.push_str("localhost\n-\nhttp://localhost:4000\n");
        answers.push_str("no-slash\n\n");
        answers.push_str("lower-case\n\n");
        let (p, out) = run(&answers);
        assert_eq!(p.base_url, "http://localhost:4000");
        assert_eq!(p.ready_path, "/");
        assert_eq!(p.ticket_project.as_deref(), Some("SHOP"));
        assert_eq!(out.matches("error: ").count(), 4, "{out}");
        assert!(out.contains("error: base_url is required"), "{out}");
    }

    #[test]
    fn eof_accepts_the_rest() {
        let (p, _) = run("echo hi\n");
        assert_eq!(p.start.as_deref(), Some("echo hi"));
        assert_eq!(p.services, plan().services);
    }

    #[test]
    fn yes_no_defaults_to_no() {
        let mut out = Vec::new();
        assert!(ask_yes_no("Install?", &mut "y\n".as_bytes(), &mut out).unwrap());
        assert!(ask_yes_no("Install?", &mut "YES\n".as_bytes(), &mut out).unwrap());
        assert!(!ask_yes_no("Install?", &mut "\n".as_bytes(), &mut out).unwrap());
        assert!(!ask_yes_no("Install?", &mut "".as_bytes(), &mut out).unwrap());
        assert!(
            String::from_utf8(out)
                .unwrap()
                .starts_with("Install? [y/N]: ")
        );
    }
}
