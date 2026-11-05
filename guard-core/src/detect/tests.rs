use super::*;
use crate::config::{CodeHostKind, parse_target};

fn files(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn detects_languages_from_manifests_and_extensions() {
    let tracked = files(&[
        "package.json",
        "web/src/app.tsx",
        "services/api/main.go",
        "tools/x.py",
        "lib/a.rb",
        "app/Program.cs",
        "Api.csproj",
        "x.kt",
        "s/A.java",
        "p/composer.json",
        "README.md",
    ]);
    assert_eq!(
        detect_languages(&tracked),
        vec![
            "csharp",
            "go",
            "java",
            "kotlin",
            "php",
            "python",
            "ruby",
            "typescript"
        ]
    );
    assert_eq!(
        detect_languages(&files(&["a.mjs", "b.cjs", "Cargo.toml"])),
        vec!["javascript", "rust"]
    );
}

#[test]
fn ignores_vendored_and_build_directories() {
    let tracked = files(&[
        "node_modules/x/index.js",
        "web/node_modules/y.ts",
        "target/a.rs",
        "vendor/b.go",
        "dist/c.js",
        "a.go",
    ]);
    assert_eq!(detect_languages(&tracked), vec!["go"]);
}

#[test]
fn detects_the_code_host_from_the_origin_url() {
    assert_eq!(
        detect_code_host("git@github.com:org/repo.git"),
        Some(CodeHostKind::Github)
    );
    assert_eq!(
        detect_code_host("https://github.com/org/repo"),
        Some(CodeHostKind::Github)
    );
    assert_eq!(
        detect_code_host("https://user@gitlab.example.com/x.git"),
        Some(CodeHostKind::Gitlab)
    );
    assert_eq!(
        detect_code_host("ssh://git@gitlab.com:2222/x.git"),
        Some(CodeHostKind::Gitlab)
    );
    assert_eq!(detect_code_host("https://bitbucket.org/x"), None);
    assert_eq!(detect_code_host("https://notgithub.com/x"), None);
}

#[test]
fn compose_files_win_over_npm_start() {
    let pkg = r#"{"scripts": {"start": "node server.js"}}"#;
    let detected = detect_start_stop(&files(&["compose.yaml", "package.json"]), Some(pkg));
    assert_eq!(
        detected.start.as_deref(),
        Some("docker compose up -d --wait")
    );
    assert_eq!(detected.stop.as_deref(), Some("docker compose down -v"));

    let detected = detect_start_stop(&files(&["package.json"]), Some(pkg));
    assert_eq!(detected.start.as_deref(), Some("npm start"));
    assert_eq!(detected.stop, None);

    let detected = detect_start_stop(
        &files(&["package.json"]),
        Some(r#"{"scripts": {"test": "x"}}"#),
    );
    assert_eq!(detected, StartStop::default());
    assert_eq!(
        detect_start_stop(&[], Some("not json")),
        StartStop::default()
    );
}

#[test]
fn detects_the_ticket_project_from_branch_names() {
    let branches = files(&["main", "feature/x", "SHOP-123-fix-cart", "OPS-1-y"]);
    assert_eq!(detect_ticket_project(&branches), Some("SHOP".into()));
    assert_eq!(
        detect_ticket_project(&files(&["origin/AB2-77-x"])),
        Some("AB2".into())
    );
    assert_eq!(detect_ticket_project(&files(&["main", "shop-1"])), None);
}

#[test]
fn proposes_one_area_per_top_level_source_directory() {
    let tracked = files(&[
        "web/src/app.ts",
        "services/orders/main.go",
        "Mobile App/x.kt",
        "docs/readme.md",
        "docs/gen.py",
        ".github/workflows/x.yml",
        "e2e-playwright/tests/a.spec.ts",
        ".hidden/x.ts",
        "assets/logo.png",
        "root.ts",
        "9lives/a.go",
    ]);
    let areas = propose_areas(&tracked);
    let keys: Vec<(&str, &str)> = areas
        .iter()
        .map(|a| (a.key.as_str(), a.dir.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("area_9lives", "9lives"),
            ("mobile_app", "Mobile App"),
            ("services", "services"),
            ("web", "web")
        ]
    );
}

#[test]
fn colliding_area_keys_get_a_suffix_and_a_warning() {
    let tracked = files(&[
        "Web/a.ts",
        "web/b.ts",
        "my-app/c.ts",
        "my_app/d.ts",
        "my_app_2/e.ts",
    ]);
    let (areas, warnings) = propose_areas_with_warnings(&tracked);
    let keys: Vec<(&str, &str)> = areas
        .iter()
        .map(|a| (a.key.as_str(), a.dir.as_str()))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("my_app", "my-app"),
            ("my_app_2", "my_app_2"),
            ("my_app_3", "my_app"),
            ("web", "Web"),
            ("web_2", "web"),
        ]
    );
    assert_eq!(
        warnings,
        vec![
            "directories \"my-app\" and \"my_app\" both map to area key `my_app`; proposed `my_app_3` for \"my_app\"",
            "directories \"Web\" and \"web\" both map to area key `web`; proposed `web_2` for \"web\"",
        ]
    );
    assert_eq!(propose_areas(&tracked), areas);
}

#[test]
fn too_many_areas_fall_back_to_a_single_app_area() {
    let tracked: Vec<String> = (0..256).map(|i| format!("d{i:03}/x.ts")).collect();
    let (areas, warnings) = propose_areas_with_warnings(&tracked);
    assert_eq!(
        areas,
        vec![ProposedArea {
            key: "app".into(),
            dir: String::new()
        }]
    );
    assert_eq!(
        warnings,
        vec![
            "256 top-level directories exceed the limit of 255 areas; proposed a single [areas.app]"
        ]
    );
    let (areas, warnings) = propose_areas_with_warnings(&tracked[..255]);
    assert_eq!(areas.len(), 255);
    assert!(warnings.is_empty());
}

#[test]
fn falls_back_to_a_single_app_area() {
    let areas = propose_areas(&files(&["main.go", "README.md"]));
    assert_eq!(
        areas,
        vec![ProposedArea {
            key: "app".into(),
            dir: String::new()
        }]
    );
}

#[test]
fn normalises_the_app_name() {
    assert_eq!(normalize_app_name("Demo Shop"), "demo-shop");
    assert_eq!(normalize_app_name("e2e-agentdev"), "e2e-agentdev");
    assert_eq!(normalize_app_name("__My_App"), "my_app");
    assert_eq!(normalize_app_name("!!!"), "app");
}

#[test]
fn rendered_target_toml_always_validates() {
    let plan = InitPlan {
        name: "shop".into(),
        languages: vec!["go".into(), "typescript".into()],
        start: Some("docker compose up -d --wait".into()),
        stop: Some("docker compose down -v".into()),
        code_host: Some(CodeHostKind::Github),
        ticket_project: Some("SHOP".into()),
        base_url: "http://localhost:3000".into(),
        ready_path: "/healthz".into(),
        services: vec![],
        areas: vec![
            ProposedArea {
                key: "services".into(),
                dir: "services".into(),
            },
            ProposedArea {
                key: "web".into(),
                dir: "web".into(),
            },
        ],
    };
    let text = render_target_toml(&plan);
    let config = parse_target(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(config.app.name, "shop");
    assert_eq!(config.app.base_url, "http://localhost:3000");
    assert_eq!(config.app.ready_path, "/healthz");
    assert_eq!(
        config.app.start.as_deref(),
        Some("docker compose up -d --wait")
    );
    assert_eq!(config.areas["web"].paths, vec!["web/**"]);
    assert_eq!(config.areas["web"].description, "TODO: describe web");
    let tracker = config.tracker.unwrap();
    assert_eq!(tracker.project.as_deref(), Some("SHOP"));
    assert_eq!(tracker.ticket_pattern.as_deref(), Some(r"SHOP-\d+"));
    assert_eq!(config.code_host.unwrap().kind, CodeHostKind::Github);
}

#[test]
fn rendered_minimal_plan_validates_and_escapes_strings() {
    let plan = InitPlan {
        name: "x".into(),
        languages: vec![],
        start: Some(r#"node "server.js" \ x"#.into()),
        stop: None,
        code_host: None,
        ticket_project: None,
        base_url: "http://localhost:3000".into(),
        ready_path: "/healthz".into(),
        services: vec![],
        areas: vec![ProposedArea {
            key: "app".into(),
            dir: String::new(),
        }],
    };
    let text = render_target_toml(&plan);
    let config = parse_target(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(config.app.start.as_deref(), Some(r#"node "server.js" \ x"#));
    assert_eq!(config.areas["app"].paths, vec!["**"]);
    assert_eq!(config.areas["app"].description, "TODO: describe app");
    assert!(config.tracker.is_none());
    assert!(config.code_host.is_none());
}

#[test]
fn empty_catalog_is_version_one_with_no_journeys() {
    let catalog = crate::catalog::parse_catalog(EMPTY_CATALOG).unwrap();
    assert!(catalog.journeys.is_empty());
}

fn service(name: &str, cwd: &str, start: &str, ready_url: Option<&str>) -> crate::config::Service {
    crate::config::Service {
        name: name.into(),
        cwd: cwd.into(),
        start: start.into(),
        stop: None,
        ready_url: ready_url.map(str::to_string),
    }
}

#[test]
fn rendered_services_plan_validates_with_base_url_and_ready_path() {
    let plan = InitPlan {
        name: "saas".into(),
        languages: vec!["php".into(), "typescript".into()],
        start: Some("docker compose -f docker-compose.yml up -d --wait postgres redis".into()),
        stop: Some("docker compose -f docker-compose.yml down".into()),
        code_host: None,
        ticket_project: None,
        base_url: "http://localhost:3100".into(),
        ready_path: "/".into(),
        services: vec![
            service(
                "api",
                "api",
                "bun run dev",
                Some("http://localhost:3001/health"),
            ),
            service("web", "apps/web", "npm run dev", None),
        ],
        areas: vec![ProposedArea {
            key: "app".into(),
            dir: String::new(),
        }],
    };
    let text = render_target_toml(&plan);
    let config = parse_target(&text).unwrap_or_else(|e| {
        panic!(
            "{e}
{text}"
        )
    });
    assert_eq!(config.app.base_url, "http://localhost:3100");
    assert_eq!(config.app.ready_path, "/");
    assert_eq!(config.services, plan.services);
    let app = text.find("[app]").unwrap();
    let services = text.find("[[services]]").unwrap();
    let areas = text.find("[areas.app]").unwrap();
    assert!(app < services && services < areas, "{text}");
}

fn detect(contents: &[(&str, &str)], files: &[&str]) -> DetectionReport {
    let mut all: Vec<String> = files.iter().map(|f| f.to_string()).collect();
    all.extend(contents.iter().map(|(p, _)| p.to_string()));
    detect_app(&DetectInput {
        files: all,
        contents: contents
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect(),
        repo_name: "demo".into(),
    })
}

#[test]
fn app_plan_of_a_single_root_service_uses_its_start_as_the_app_start() {
    let report = detect(
        &[(
            "package.json",
            r#"{"scripts":{"dev":"next dev -p 3100"},"dependencies":{"next":"16"}}"#,
        )],
        &["package-lock.json"],
    );
    let plan = app_plan(&report);
    assert_eq!(plan.start.as_deref(), Some("npm run dev"));
    assert_eq!(plan.stop, None);
    assert_eq!(plan.base_url, "http://localhost:3100");
    assert_eq!(plan.ready_path, "/");
    assert!(plan.services.is_empty());
}

#[test]
fn app_plan_of_several_services_puts_compose_infra_in_the_app_and_services_in_order() {
    let report = detect(
        &[
            (
                "docker-compose.yml",
                "services:
  db:
    image: postgres:16
",
            ),
            (
                "api/package.json",
                r#"{"scripts":{"dev":"node --watch src/index.js"},"dependencies":{"express":"4"}}"#,
            ),
            (
                "api/src/index.js",
                "app.get('/health', h);
app.listen(4000);
",
            ),
            (
                "web/package.json",
                r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"6"}}"#,
            ),
        ],
        &[],
    );
    let plan = app_plan(&report);
    assert_eq!(
        plan.start.as_deref(),
        Some("docker compose -f docker-compose.yml up -d --wait db")
    );
    assert_eq!(
        plan.stop.as_deref(),
        Some("docker compose -f docker-compose.yml down")
    );
    assert_eq!(
        plan.services,
        vec![
            service(
                "api",
                "api",
                "npm run dev",
                Some("http://localhost:4000/health")
            ),
            service("web", "web", "npm run dev", Some("http://localhost:5173/")),
        ]
    );
    assert_eq!(plan.base_url, "http://localhost:5173");
    assert_eq!(plan.ready_path, "/");
}

#[test]
fn app_plan_of_a_compose_only_repo_runs_the_whole_stack() {
    let report = detect(
        &[(
            "docker-compose.yml",
            "services:
  app:
    image: acme/app
",
        )],
        &[],
    );
    let plan = app_plan(&report);
    assert_eq!(plan.start.as_deref(), Some("docker compose up -d --wait"));
    assert_eq!(plan.stop.as_deref(), Some("docker compose down -v"));
    assert_eq!(plan.base_url, "http://localhost:3000");
    assert_eq!(plan.ready_path, "/healthz");
}

fn services_plan() -> InitPlan {
    InitPlan {
        name: "saunas".into(),
        languages: vec!["php".into(), "typescript".into()],
        start: Some(
            "docker compose -f docker-compose.yml up -d --wait postgres redis mailpit".into(),
        ),
        stop: Some("docker compose -f docker-compose.yml down".into()),
        code_host: None,
        ticket_project: Some("SAU".into()),
        base_url: "http://localhost:3000".into(),
        ready_path: "/".into(),
        services: vec![
            service(
                "backend",
                "backend",
                "bun run dev",
                Some("http://localhost:3001/health"),
            ),
            service(
                "frontend",
                "frontend",
                "bun run dev",
                Some("http://localhost:3000/"),
            ),
        ],
        areas: vec![
            ProposedArea {
                key: "backend".into(),
                dir: "backend".into(),
            },
            ProposedArea {
                key: "frontend".into(),
                dir: "frontend".into(),
            },
        ],
    }
}

#[test]
fn self_mode_render_matches_the_contract_example() {
    let text = render_self_target_toml(&services_plan());
    let expected = r#"# target.toml, generated by guard init.
# Contract: docs/contracts/target-toml.md of the guard repository.
version = 1

[app]
name = "saunas"
languages = ["php", "typescript"]
base_url = "http://localhost:3000"
ready_path = "/"

# guard does not start the app: you run it yourself (guard init --runner self).
# Detected commands, uncomment to let guard start it (needed in CI):
# start = "docker compose -f docker-compose.yml up -d --wait postgres redis mailpit"
# stop = "docker compose -f docker-compose.yml down"
#
# [[services]]
# name = "backend"
# cwd = "backend"
# start = "bun run dev"
# ready_url = "http://localhost:3001/health"
#
# [[services]]
# name = "frontend"
# cwd = "frontend"
# start = "bun run dev"
# ready_url = "http://localhost:3000/"

[areas.backend]
description = "TODO: describe backend"
paths = ["backend/**"]

[areas.frontend]
description = "TODO: describe frontend"
paths = ["frontend/**"]

[tracker]
kind = "jira"
project = "SAU"
ticket_pattern = 'SAU-\d+'
"#;
    assert_eq!(text, expected);
}

/// Removes the leading `# ` of every block line, keeping the two header lines.
fn uncomment(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line == crate::self_mode::SELF_MODE_HEADER
                || line == crate::self_mode::SELF_MODE_MARKER
            {
                line.to_string()
            } else if let Some(rest) = line.strip_prefix("# ") {
                rest.to_string()
            } else if line == "#" {
                String::new()
            } else {
                line.to_string()
            }
        })
        .map(|line| line + "\n")
        .collect()
}

#[test]
fn self_mode_render_validates_without_start_config_and_uncomments_into_the_guard_shape() {
    let plan = services_plan();
    let text = render_self_target_toml(&plan);
    let config = parse_target(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(config.is_self_mode());
    assert_eq!(config.app.start, None);
    assert_eq!(config.app.stop, None);
    assert!(config.services.is_empty());

    // The generated header lines start with `# target.toml` / `# Contract`;
    // uncommenting them is harmless only if they stay comments, so skip them.
    let enabled_text = uncomment(&text)
        .replacen("target.toml, generated by guard init.\n", "", 1)
        .replacen(
            "Contract: docs/contracts/target-toml.md of the guard repository.\n",
            "",
            1,
        );
    let enabled = parse_target(&enabled_text).unwrap_or_else(|e| panic!("{e}\n{enabled_text}"));
    assert!(!enabled.is_self_mode());
    assert_eq!(enabled.app.start, plan.start);
    assert_eq!(enabled.app.stop, plan.stop);
    assert_eq!(enabled.services, plan.services);
}

#[test]
fn self_mode_render_of_a_single_app_comments_only_its_start() {
    let plan = InitPlan {
        services: vec![],
        start: Some("npm run dev".into()),
        stop: None,
        ..services_plan()
    };
    let text = render_self_target_toml(&plan);
    let block = format!(
        "ready_path = \"/\"\n\n{}\n{}\n# start = \"npm run dev\"\n\n[areas.backend]",
        crate::self_mode::SELF_MODE_HEADER,
        crate::self_mode::SELF_MODE_MARKER
    );
    assert!(text.contains(&block), "{text}");
    assert!(!text.contains("[[services]]"), "{text}");
    assert!(parse_target(&text).unwrap().is_self_mode());
}

#[test]
fn self_mode_render_without_detected_commands_has_no_block() {
    let plan = InitPlan {
        services: vec![],
        start: None,
        stop: None,
        ..services_plan()
    };
    let text = render_self_target_toml(&plan);
    assert!(!text.contains("# Detected commands"), "{text}");
    assert_eq!(text, render_target_toml(&plan));
}

#[test]
fn self_mode_hint_reads_the_rendered_block() {
    let lines = crate::self_mode::hint_lines(&render_self_target_toml(&services_plan()));
    assert_eq!(lines[2], "  backend (in backend): bun run dev");
    assert_eq!(lines.len(), 5);
}
