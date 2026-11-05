//! App detection for `guard init` (contract: `detection.md`): services,
//! frameworks, package managers, start commands, ports, ready paths and compose
//! infrastructure, each guess with its confidence and evidence. Pure.

use super::compose::{self, ParsedService};
use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

#[cfg(test)]
mod tests;

/// What the CLI hands to [`detect_app`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectInput {
    /// Repo-relative paths, forward slashes.
    pub files: Vec<String>,
    /// Contents of the files [`wanted_files`] asked for.
    pub contents: BTreeMap<String, String>,
    /// Directory name of the repo root (fallback service name).
    pub repo_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// A guessed value with how sure detection is and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Guess<T> {
    pub value: T,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Framework {
    Next,
    Vite,
    Nuxt,
    Sveltekit,
    Astro,
    Remix,
    Elysia,
    Hono,
    Express,
    Fastify,
    Nestjs,
    Laravel,
    Symfony,
    Dotnet,
    Django,
    Fastapi,
    Flask,
    Rails,
    Go,
    Rust,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Backend,
    Unknown,
    Frontend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Bun,
    Pnpm,
    Yarn,
    Npm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedService {
    pub name: String,
    pub root: String,
    pub kind: ServiceKind,
    pub confidence: Confidence,
    pub framework: Guess<Framework>,
    pub package_manager: Option<Guess<PackageManager>>,
    pub start: Guess<String>,
    pub port: Guess<u16>,
    pub ready_path: Guess<String>,
    pub ready_url: String,
    pub also_in_compose: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SkippedRoot {
    pub root: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComposeRole {
    Infra,
    Native,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeServiceReport {
    pub name: String,
    pub image: Option<String>,
    /// Build context, repo-relative and normalised (`.` for the repo root).
    pub build: Option<String>,
    pub ports: Vec<super::compose::PortMapping>,
    pub role: ComposeRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComposeReport {
    pub file: String,
    pub services: Vec<ComposeServiceReport>,
    pub start: Option<Guess<String>>,
    pub stop: Option<Guess<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IgnoredComposeFile {
    pub file: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetShape {
    Single,
    Services,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectionReport {
    pub version: u32,
    pub services: Vec<DetectedService>,
    pub skipped: Vec<SkippedRoot>,
    pub compose: Option<ComposeReport>,
    pub ignored_compose_files: Vec<IgnoredComposeFile>,
    pub base_url: Guess<String>,
    pub ready_path: Guess<String>,
    pub target_shape: TargetShape,
    pub notes: Vec<String>,
}

impl Framework {
    pub fn as_str(self) -> &'static str {
        match self {
            Framework::Next => "next",
            Framework::Vite => "vite",
            Framework::Nuxt => "nuxt",
            Framework::Sveltekit => "sveltekit",
            Framework::Astro => "astro",
            Framework::Remix => "remix",
            Framework::Elysia => "elysia",
            Framework::Hono => "hono",
            Framework::Express => "express",
            Framework::Fastify => "fastify",
            Framework::Nestjs => "nestjs",
            Framework::Laravel => "laravel",
            Framework::Symfony => "symfony",
            Framework::Dotnet => "dotnet",
            Framework::Django => "django",
            Framework::Fastapi => "fastapi",
            Framework::Flask => "flask",
            Framework::Rails => "rails",
            Framework::Go => "go",
            Framework::Rust => "rust",
            Framework::Unknown => "unknown",
        }
    }

    pub fn kind(self) -> ServiceKind {
        match self {
            Framework::Next
            | Framework::Vite
            | Framework::Nuxt
            | Framework::Sveltekit
            | Framework::Astro
            | Framework::Remix => ServiceKind::Frontend,
            Framework::Unknown => ServiceKind::Unknown,
            _ => ServiceKind::Backend,
        }
    }

    fn is_node(self) -> bool {
        matches!(
            self,
            Framework::Next
                | Framework::Vite
                | Framework::Nuxt
                | Framework::Sveltekit
                | Framework::Astro
                | Framework::Remix
                | Framework::Elysia
                | Framework::Hono
                | Framework::Express
                | Framework::Fastify
                | Framework::Nestjs
                | Framework::Unknown
        )
    }

    /// Default dev port and its confidence.
    fn default_port(self) -> (u16, Confidence) {
        use Confidence::{Low, Medium};
        match self {
            Framework::Next | Framework::Nuxt | Framework::Remix => (3000, Medium),
            Framework::Vite | Framework::Sveltekit => (5173, Medium),
            Framework::Astro => (4321, Medium),
            Framework::Laravel | Framework::Django => (8000, Medium),
            Framework::Fastapi | Framework::Symfony => (8000, Low),
            Framework::Flask => (5000, Low),
            Framework::Rails => (3000, Low),
            Framework::Dotnet => (5000, Low),
            Framework::Go | Framework::Rust => (8080, Low),
            _ => (3000, Low),
        }
    }
}

fn guess<T>(value: T, confidence: Confidence, evidence: Vec<String>) -> Guess<T> {
    Guess {
        value,
        confidence,
        evidence,
    }
}

/// Directories never searched, at any depth (contract: "Ignored directories").
pub const DETECT_IGNORED_DIRS: [&str; 16] = [
    "node_modules",
    "vendor",
    "target",
    "dist",
    "build",
    ".git",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".venv",
    "venv",
    "__pycache__",
    "bin",
    "obj",
    "e2e-playwright",
    "src-tauri",
];
const MANIFESTS: [&str; 8] = [
    "package.json",
    "composer.json",
    "pyproject.toml",
    "requirements.txt",
    "manage.py",
    "go.mod",
    "Cargo.toml",
    "Gemfile",
];
const UNSUPPORTED_MANIFESTS: [&str; 3] = ["pom.xml", "build.gradle", "build.gradle.kts"];
const CONFIG_PREFIXES: [&str; 6] = [
    "next.config.",
    "vite.config.",
    "nuxt.config.",
    "svelte.config.",
    "astro.config.",
    "remix.config.",
];
const LOCKFILES: [(&str, PackageManager); 5] = [
    ("bun.lock", PackageManager::Bun),
    ("bun.lockb", PackageManager::Bun),
    ("pnpm-lock.yaml", PackageManager::Pnpm),
    ("yarn.lock", PackageManager::Yarn),
    ("package-lock.json", PackageManager::Npm),
];
const ROOT_COMPOSE_FILES: [&str; 4] = [
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];
const MAX_SOURCES: usize = 12;
const MAX_FRAGMENT: usize = 120;

static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\bfrom|\bimport|\brequire)\s*\(?\s*['"](\.\.?/[^'"]+)['"]"#)
        .expect("built-in pattern compiles")
});
static FLAG_PORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|\s)(?:-p|--port)(?:=|\s+)(\d+)\b").expect("built-in pattern compiles")
});
static LAUNCH_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#""applicationUrl"\s*:\s*"([^"]+)""#).expect("built-in pattern compiles")
});
static SOURCE_PORT_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r#"\.listen\(\s*\{\s*port\s*:\s*['"]?(\d+)['"]?"#,
        r#"\.listen\(\s*['"]?(\d+)['"]?"#,
        r#"(?:^|[^A-Za-z0-9_$])(PORT\s*(?:\|\||\?\?)\s*['"]?(\d+)['"]?)"#,
        r#"ListenAndServe\(\s*"[^"]*:(\d+)""#,
        r#"Addr:\s*"[^"]*:(\d+)""#,
        r#"bind\(\s*"[^"]*:(\d+)""#,
    ]
    .iter()
    .map(|p| Regex::new(p).expect("built-in pattern compiles"))
    .collect()
});
/// A bare `port: <n>` key: weaker than the patterns above (it also matches
/// database and cache configs), so it is only tried when none of them hit.
static KEY_PORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|[^A-Za-z0-9_$])(port\s*:\s*['"]?(\d+)['"]?)"#)
        .expect("built-in pattern compiles")
});
/// `.default(<n>)` only counts on a line naming `PORT` as a whole word.
static PORT_WORD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[^A-Za-z0-9_$])port(?:$|[^A-Za-z0-9_$])")
        .expect("built-in pattern compiles")
});
/// Well-known ports of infrastructure (databases, caches, brokers, SMTP
/// catchers): a bare `port: <n>` with one of these is not the app's port.
const INFRA_PORTS: [u16; 13] = [
    1025, 1433, 1521, 2181, 3306, 5432, 5672, 6379, 9042, 9092, 9200, 11211, 27017,
];
static DEFAULT_PORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\.default\(\s*['"]?(\d+)['"]?"#).expect("built-in pattern compiles")
});
static ENV_PORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s*(PORT\s*=\s*['"]?(\d+))"#).expect("built-in pattern compiles")
});
static CONFIG_PORT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bport\s*:\s*(\d+)").expect("built-in pattern compiles"));
static HEALTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:Route::get|\.get|MapGet|HandleFunc|\bpath)\(\s*['"]/?(healthz?)/?['"]"#)
        .expect("built-in pattern compiles")
});
static PY_DEP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?im)(?:^|["'\s])(django|fastapi|flask)\s*(?:[=<>~!\[;,"']|$)"#)
        .expect("built-in pattern compiles")
});
static RAILS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"gem\s+['"]rails['"]"#).expect("built-in pattern compiles"));
static CARGO_WEB_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*(?:\[dependencies\.)?(axum|actix-web|rocket|warp|poem|salvo)\s*[=\]]")
        .expect("built-in pattern compiles")
});

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn join(root: &str, rel: &str) -> String {
    if root == "." || root.is_empty() {
        rel.to_string()
    } else {
        format!("{root}/{rel}")
    }
}

fn depth(dir: &str) -> usize {
    if dir == "." {
        0
    } else {
        dir.split('/').count()
    }
}

/// Resolves `.` and `..` segments; `None` when the path leaves the repo.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    })
}

fn is_ignored_path(path: &str) -> bool {
    let dir = dir_of(path);
    dir != "." && dir.split('/').any(|s| DETECT_IGNORED_DIRS.contains(&s))
}

fn is_manifest(name: &str) -> bool {
    MANIFESTS.contains(&name) || name.ends_with(".csproj")
}

fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn fragment(text: &str) -> String {
    text.chars().take(MAX_FRAGMENT).collect()
}

struct Index<'a> {
    files: BTreeSet<&'a str>,
    by_dir: BTreeMap<&'a str, Vec<&'a str>>,
}

impl<'a> Index<'a> {
    fn new(files: &'a [String]) -> Self {
        let mut index = Index {
            files: BTreeSet::new(),
            by_dir: BTreeMap::new(),
        };
        for file in files.iter().map(String::as_str) {
            if is_ignored_path(file) {
                continue;
            }
            index.files.insert(file);
            index
                .by_dir
                .entry(dir_of(file))
                .or_default()
                .push(name_of(file));
        }
        index
    }

    fn has(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    fn names(&self, dir: &str) -> &[&'a str] {
        self.by_dir.get(dir).map_or(&[], Vec::as_slice)
    }

    /// Supported service roots and roots with only an unsupported manifest.
    fn roots(&self) -> (Vec<String>, Vec<(String, &'static str)>) {
        let mut roots = Vec::new();
        let mut unsupported = Vec::new();
        for (dir, names) in &self.by_dir {
            if depth(dir) > 2 {
                continue;
            }
            if names.iter().any(|n| is_manifest(n)) {
                roots.push(dir.to_string());
            } else if depth(dir) >= 1
                && let Some(m) = UNSUPPORTED_MANIFESTS.iter().find(|m| names.contains(m))
            {
                unsupported.push((dir.to_string(), *m));
            }
        }
        let all: Vec<String> = roots
            .iter()
            .chain(unsupported.iter().map(|(d, _)| d))
            .filter(|d| *d != ".")
            .cloned()
            .collect();
        unsupported.retain(|(dir, _)| {
            !all.iter()
                .any(|other| dir.starts_with(&format!("{other}/")))
        });
        (roots, unsupported)
    }
}

fn conventional_sources() -> Vec<String> {
    let mut out = Vec::new();
    for base in ["src/", ""] {
        for stem in ["index", "main", "server", "app"] {
            for ext in ["ts", "js", "mjs"] {
                out.push(format!("{base}{stem}.{ext}"));
            }
        }
    }
    for rel in [
        "src/config/env.ts",
        "src/config/env.js",
        "src/env.ts",
        "src/env.js",
        "config/env.ts",
        "config/env.js",
        "main.py",
        "app/main.py",
        "app.py",
        "main.go",
        "src/main.rs",
        "routes/web.php",
        "routes/api.php",
    ] {
        out.push(rel.to_string());
    }
    out
}

fn parse_json(text: Option<&String>) -> Option<serde_json::Value> {
    text.and_then(|t| serde_json::from_str(t).ok())
}

/// The `dev` script, else the `start` script.
fn chosen_script(pkg: &serde_json::Value) -> Option<(&'static str, String)> {
    ["dev", "start"].into_iter().find_map(|name| {
        pkg["scripts"][name]
            .as_str()
            .map(|script| (name, script.to_string()))
    })
}

/// The relative source file a script runs (`bun run --watch src/index.ts`).
fn script_entry(script: &str) -> Option<String> {
    script
        .split_whitespace()
        .filter(|t| !t.starts_with('-'))
        .map(|t| t.trim_start_matches("./"))
        .find(|t| {
            [".ts", ".tsx", ".mts", ".js", ".mjs", ".cjs"]
                .iter()
                .any(|ext| t.ends_with(ext))
                && !t.starts_with("dist/")
                && !t.starts_with("build/")
                && !t.starts_with('/')
        })
        .map(str::to_string)
}

fn resolve_import(from: &str, spec: &str, index: &Index<'_>) -> Option<String> {
    let base = normalize(&join(dir_of(from), spec))?;
    let mut candidates = vec![base.clone()];
    if let Some(stem) = base.strip_suffix(".js") {
        candidates.push(format!("{stem}.ts"));
    }
    for suffix in [".ts", ".js", ".mjs", "/index.ts", "/index.js"] {
        candidates.push(format!("{base}{suffix}"));
    }
    candidates.into_iter().find(|c| index.has(c))
}

/// Source files scanned for ports and health routes, in scan order.
fn sources(root: &str, index: &Index<'_>, contents: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |path: String| {
        if index.has(&path) && !out.contains(&path) {
            out.push(path);
        }
    };
    let entry = parse_json(contents.get(&join(root, "package.json")))
        .and_then(|pkg| chosen_script(&pkg))
        .and_then(|(_, script)| script_entry(&script))
        .and_then(|rel| normalize(&join(root, &rel)));
    if let Some(entry) = entry.filter(|e| index.has(e)) {
        push(entry.clone());
        if let Some(text) = contents.get(&entry) {
            for capture in IMPORT_RE.captures_iter(text) {
                if let Some(path) = resolve_import(&entry, &capture[1], index) {
                    push(path);
                }
            }
        }
    }
    for rel in conventional_sources() {
        push(join(root, &rel));
    }
    let cmd = join(root, "cmd/");
    let mains: Vec<String> = index
        .files
        .iter()
        .filter(|f| {
            f.strip_prefix(&cmd)
                .and_then(|rest| rest.strip_suffix("/main.go"))
                .is_some_and(|name| !name.contains('/'))
        })
        .map(|f| f.to_string())
        .collect();
    for main in mains {
        push(main);
    }
    out.truncate(MAX_SOURCES);
    out
}

fn is_considered_compose(path: &str, roots: &[String]) -> bool {
    let dir = dir_of(path);
    compose::is_compose_name(name_of(path)) && (dir == "." || roots.iter().any(|r| r == dir))
}

/// The files the CLI should read before calling [`detect_app`]. Call it again
/// with what was read until it returns no new path (at most 3 rounds). Only
/// allowlisted names are ever returned; no dotenv file but `.env.example`.
pub fn wanted_files(files: &[String], contents: &BTreeMap<String, String>) -> Vec<String> {
    let index = Index::new(files);
    let (roots, _) = index.roots();
    let mut out = BTreeSet::new();
    for file in &index.files {
        if is_considered_compose(file, &roots) && compose::ignore_word(name_of(file)).is_none() {
            out.insert(file.to_string());
        }
    }
    for root in &roots {
        for name in index.names(root) {
            let wanted = (is_manifest(name) && *name != "manage.py")
                || CONFIG_PREFIXES.iter().any(|p| name.starts_with(p))
                || *name == ".env.example";
            if wanted {
                out.insert(join(root, name));
            }
        }
        let launch = join(root, "Properties/launchSettings.json");
        if index.has(&launch) {
            out.insert(launch);
        }
        out.extend(sources(root, &index, contents));
    }
    out.into_iter().collect()
}

/// Everything known about one service root before naming and ordering.
struct Draft {
    root: String,
    framework: Guess<Framework>,
    package_manager: Option<Guess<PackageManager>>,
    start: Guess<String>,
    port: Guess<u16>,
    ready_path: Guess<String>,
    also_in_compose: Option<String>,
}

struct RootView<'a> {
    root: &'a str,
    index: &'a Index<'a>,
    contents: &'a BTreeMap<String, String>,
}

impl RootView<'_> {
    fn path(&self, rel: &str) -> String {
        join(self.root, rel)
    }

    fn has(&self, rel: &str) -> bool {
        self.index.has(&self.path(rel))
    }

    fn text(&self, rel: &str) -> Option<&String> {
        self.contents.get(&self.path(rel))
    }

    fn composer_requires(&self, package: &str) -> bool {
        parse_json(self.text("composer.json")).is_some_and(|c| {
            c["require"].get(package).is_some() || c["require-dev"].get(package).is_some()
        })
    }

    fn csproj_web(&self) -> Option<String> {
        self.index
            .names(self.root)
            .iter()
            .filter(|n| n.ends_with(".csproj"))
            .find(|n| {
                self.text(n)
                    .is_some_and(|t| t.contains("Microsoft.NET.Sdk.Web"))
            })
            .map(|n| self.path(n))
    }

    fn python_dep(&self, wanted: &str) -> Option<String> {
        ["requirements.txt", "pyproject.toml"]
            .iter()
            .find_map(|file| {
                let text = self.text(file)?;
                PY_DEP_RE
                    .captures_iter(text)
                    .any(|c| c[1].eq_ignore_ascii_case(wanted))
                    .then(|| format!("{} {wanted}", self.path(file)))
            })
    }
}

/// Whether a dependency name selects a framework.
type DepMatcher = fn(&str) -> bool;

fn node_dep(pkg: &serde_json::Value, matches: impl Fn(&str) -> bool) -> Option<String> {
    ["dependencies", "devDependencies"]
        .iter()
        .find_map(|section| {
            pkg[section]
                .as_object()?
                .keys()
                .find(|k| matches(k))
                .map(|k| format!("{section}.{k}"))
        })
}

fn detect_framework(
    view: &RootView<'_>,
    pkg: Option<&serde_json::Value>,
) -> (Framework, Vec<String>) {
    if view.has("artisan") && view.composer_requires("laravel/framework") {
        return (
            Framework::Laravel,
            vec![
                view.path("artisan"),
                format!("{} require.laravel/framework", view.path("composer.json")),
            ],
        );
    }
    if view.composer_requires("symfony/framework-bundle") {
        return (
            Framework::Symfony,
            vec![format!(
                "{} require.symfony/framework-bundle",
                view.path("composer.json")
            )],
        );
    }
    if view.has("manage.py") {
        return (Framework::Django, vec![view.path("manage.py")]);
    }
    for (name, framework) in [
        ("django", Framework::Django),
        ("fastapi", Framework::Fastapi),
        ("flask", Framework::Flask),
    ] {
        if let Some(evidence) = view.python_dep(name) {
            return (framework, vec![evidence]);
        }
    }
    if view.text("Gemfile").is_some_and(|t| RAILS_RE.is_match(t)) {
        return (
            Framework::Rails,
            vec![format!("{} gem rails", view.path("Gemfile"))],
        );
    }
    if let Some(csproj) = view.csproj_web() {
        return (
            Framework::Dotnet,
            vec![format!("{csproj} Microsoft.NET.Sdk.Web")],
        );
    }
    let pkg_path = view.path("package.json");
    if let Some(pkg) = pkg {
        let dep = |names: &[&str]| node_dep(pkg, |k| names.contains(&k));
        let next_config: Vec<String> = view
            .index
            .names(view.root)
            .iter()
            .filter(|n| n.starts_with("next.config."))
            .map(|n| view.path(n))
            .collect();
        let mut evidence: Vec<String> = dep(&["next"])
            .map(|d| format!("{pkg_path} {d}"))
            .into_iter()
            .collect();
        if !evidence.is_empty() || !next_config.is_empty() {
            evidence.extend(next_config);
            return (Framework::Next, evidence);
        }
        let checks: [(Framework, DepMatcher); 10] = [
            (Framework::Nuxt, |k| k == "nuxt"),
            (Framework::Remix, |k| {
                k.starts_with("@remix-run/") || k == "@react-router/dev"
            }),
            (Framework::Sveltekit, |k| k == "@sveltejs/kit"),
            (Framework::Astro, |k| k == "astro"),
            (Framework::Nestjs, |k| k == "@nestjs/core"),
            (Framework::Elysia, |k| k == "elysia"),
            (Framework::Hono, |k| k == "hono"),
            (Framework::Fastify, |k| k == "fastify"),
            (Framework::Express, |k| k == "express"),
            (Framework::Vite, |k| k == "vite"),
        ];
        for (framework, matches) in checks {
            if let Some(d) = node_dep(pkg, matches) {
                return (framework, vec![format!("{pkg_path} {d}")]);
            }
        }
    } else if view.has("next.config.js")
        || view.has("next.config.mjs")
        || view.has("next.config.ts")
    {
        // A next.config without a readable package.json still says Next.
        let config = view
            .index
            .names(view.root)
            .iter()
            .find(|n| n.starts_with("next.config."))
            .map(|n| view.path(n))
            .unwrap_or_default();
        return (Framework::Next, vec![config]);
    }
    if view.has("go.mod") {
        return (Framework::Go, vec![view.path("go.mod")]);
    }
    if let Some(text) = view.text("Cargo.toml")
        && let Some(c) = CARGO_WEB_RE.captures(text)
    {
        return (
            Framework::Rust,
            vec![format!(
                "{} dependencies.{}",
                view.path("Cargo.toml"),
                &c[1]
            )],
        );
    }
    (Framework::Unknown, Vec::new())
}

fn package_manager(view: &RootView<'_>) -> Guess<PackageManager> {
    let mut dir = view.root.to_string();
    loop {
        let present: Vec<(&str, PackageManager)> = LOCKFILES
            .iter()
            .filter(|(name, _)| view.index.has(&join(&dir, name)))
            .copied()
            .collect();
        match present.as_slice() {
            [] => {}
            [(name, pm)] => return guess(*pm, Confidence::High, vec![join(&dir, name)]),
            [(_, pm), ..] => {
                let names: Vec<String> = present.iter().map(|(n, _)| n.to_string()).collect();
                let label = if dir == "." {
                    "repo root"
                } else {
                    dir.as_str()
                };
                let pm_name = match pm {
                    PackageManager::Bun => "bun",
                    PackageManager::Pnpm => "pnpm",
                    PackageManager::Yarn => "yarn",
                    PackageManager::Npm => "npm",
                };
                return guess(
                    *pm,
                    Confidence::Medium,
                    vec![format!("{label}: {}; using {pm_name}", and_list(&names))],
                );
            }
        }
        if dir == "." {
            break;
        }
        dir = dir_of(&dir).to_string();
    }
    guess(
        PackageManager::Npm,
        Confidence::Low,
        vec!["no lockfile".into()],
    )
}

fn pm_cli(pm: PackageManager) -> &'static str {
    match pm {
        PackageManager::Bun => "bun",
        PackageManager::Pnpm => "pnpm",
        PackageManager::Yarn => "yarn",
        PackageManager::Npm => "npm",
    }
}

fn valid_port(digits: &str) -> Option<u16> {
    digits
        .parse::<u32>()
        .ok()
        .filter(|p| (1..=65535).contains(p))
        .map(|p| p as u16)
}

/// The first line of `files` that `find` matches: `(path, 1-based line, value, fragment)`.
fn scan_lines<T>(
    files: &[String],
    contents: &BTreeMap<String, String>,
    find: impl Fn(&str) -> Option<(T, String)>,
) -> Option<(String, usize, T, String)> {
    files.iter().find_map(|path| {
        let text = contents.get(path)?;
        text.lines()
            .enumerate()
            .find_map(|(i, line)| find(line).map(|(v, f)| (path.clone(), i + 1, v, f)))
    })
}

fn source_port(line: &str) -> Option<(u16, String)> {
    for re in SOURCE_PORT_RES.iter() {
        // The port is the last group; a pattern with a leading boundary
        // guard wraps its evidence fragment in group 1.
        if let Some(c) = re.captures(line)
            && let Some(port) = valid_port(&c[c.len() - 1])
        {
            let frag = if c.len() > 2 { &c[1] } else { &c[0] };
            return Some((port, fragment(frag)));
        }
    }
    if PORT_WORD_RE.is_match(line)
        && let Some(c) = DEFAULT_PORT_RE.captures(line)
        && let Some(port) = valid_port(&c[1])
    {
        return Some((port, fragment(&c[0])));
    }
    None
}

fn key_port(line: &str) -> Option<(u16, String)> {
    let c = KEY_PORT_RE.captures(line)?;
    let port = valid_port(&c[2]).filter(|p| !INFRA_PORTS.contains(p))?;
    Some((port, fragment(&c[1])))
}

fn detect_port(
    view: &RootView<'_>,
    framework: Framework,
    script: Option<&(&'static str, String)>,
    scanned: &[String],
) -> Guess<u16> {
    if let Some((name, script)) = script
        && let Some(port) = FLAG_PORT_RE
            .captures(script)
            .and_then(|c| valid_port(&c[1]))
    {
        return guess(
            port,
            Confidence::High,
            vec![format!(
                "{} scripts.{name}: {script}",
                view.path("package.json")
            )],
        );
    }
    let launch = "Properties/launchSettings.json";
    if let Some(text) = view.text(launch)
        && let Some(c) = LAUNCH_URL_RE.captures(text)
        && let Some(url) = c[1].split(';').find(|u| u.starts_with("http://"))
        && let Some(port) = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .and_then(|authority| authority.rsplit_once(':'))
            .and_then(|(_, p)| valid_port(p))
    {
        return guess(
            port,
            Confidence::High,
            vec![format!("{} applicationUrl {url}", view.path(launch))],
        );
    }
    if framework.kind() != ServiceKind::Frontend
        && let Some((path, line, port, frag)) = scan_lines(scanned, view.contents, source_port)
    {
        return guess(
            port,
            Confidence::High,
            vec![format!("{path}:{line} {frag}")],
        );
    }
    if framework.kind() != ServiceKind::Frontend
        && let Some((path, line, port, frag)) = scan_lines(scanned, view.contents, key_port)
    {
        return guess(
            port,
            Confidence::Medium,
            vec![format!("{path}:{line} {frag}")],
        );
    }
    let env_example = vec![view.path(".env.example")];
    if let Some((path, line, port, frag)) = scan_lines(&env_example, view.contents, |line| {
        let c = ENV_PORT_RE.captures(line)?;
        Some((valid_port(&c[2])?, fragment(&c[1])))
    }) {
        return guess(
            port,
            Confidence::Medium,
            vec![format!("{path}:{line} {frag}")],
        );
    }
    if framework.kind() == ServiceKind::Frontend {
        let configs: Vec<String> = view
            .index
            .names(view.root)
            .iter()
            .filter(|n| n.starts_with("vite.config.") || n.starts_with("nuxt.config."))
            .map(|n| view.path(n))
            .collect();
        if let Some((path, line, port, frag)) = scan_lines(&configs, view.contents, |line| {
            let c = CONFIG_PORT_RE.captures(line)?;
            Some((valid_port(&c[1])?, fragment(&c[0])))
        }) {
            return guess(
                port,
                Confidence::High,
                vec![format!("{path}:{line} {frag}")],
            );
        }
    }
    let (port, confidence) = framework.default_port();
    let evidence = if framework == Framework::Unknown {
        format!("no port found; using {port}")
    } else {
        format!("{} default port {port}", framework.as_str())
    };
    guess(port, confidence, vec![evidence])
}

fn detect_ready_path(
    framework: Framework,
    scanned: &[String],
    contents: &BTreeMap<String, String>,
) -> Guess<String> {
    match framework.kind() {
        ServiceKind::Frontend => {
            return guess("/".into(), Confidence::High, vec!["frontend".into()]);
        }
        ServiceKind::Unknown => return guess("/".into(), Confidence::Low, Vec::new()),
        ServiceKind::Backend => {}
    }
    let mut health: Option<(String, String)> = None;
    let mut healthz: Option<(String, String)> = None;
    for path in scanned {
        let Some(text) = contents.get(path) else {
            continue;
        };
        let prefix = if framework == Framework::Laravel && path.ends_with("routes/api.php") {
            "/api"
        } else {
            ""
        };
        for (i, line) in text.lines().enumerate() {
            for c in HEALTH_RE.captures_iter(line) {
                let slot = if &c[1] == "healthz" {
                    &mut healthz
                } else {
                    &mut health
                };
                if slot.is_none() {
                    *slot = Some((
                        format!("{prefix}/{}", &c[1]),
                        format!("{path}:{} {}", i + 1, fragment(&c[0])),
                    ));
                }
            }
        }
    }
    match healthz.or(health) {
        Some((value, evidence)) => guess(value, Confidence::High, vec![evidence]),
        None => guess("/".into(), Confidence::Low, Vec::new()),
    }
}

fn analyze_root(view: &RootView<'_>) -> Result<Draft, String> {
    let pkg = parse_json(view.text("package.json"));
    let (framework, framework_evidence) = detect_framework(view, pkg.as_ref());
    let is_node = pkg.is_some() && framework.is_node();
    let script = if is_node {
        pkg.as_ref().and_then(chosen_script)
    } else {
        None
    };
    if is_node && script.is_none() {
        let workspace = pkg.as_ref().is_some_and(|p| p.get("workspaces").is_some())
            || view.has("pnpm-workspace.yaml");
        if workspace {
            return Err("workspace container without dev or start script".into());
        }
    }
    if framework == Framework::Unknown && script.is_none() {
        let only_cargo = view.has("Cargo.toml")
            && !view
                .index
                .names(view.root)
                .iter()
                .any(|n| *n != "Cargo.toml" && is_manifest(n));
        return Err(if only_cargo {
            "Cargo.toml without a web framework dependency".into()
        } else {
            "no dev or start script and no known framework".into()
        });
    }
    let package_manager = is_node.then(|| package_manager(view));
    let scanned = sources(view.root, view.index, view.contents);
    let port = detect_port(view, framework, script.as_ref(), &scanned);
    let p = port.value;
    let high = |command: String, why: &str| guess(command, Confidence::High, vec![why.to_string()]);
    let start = if is_node {
        let pm = package_manager
            .as_ref()
            .map_or(PackageManager::Npm, |g| g.value);
        match &script {
            Some((name, text)) => guess(
                format!("{} run {name}", pm_cli(pm)),
                Confidence::High,
                vec![format!(
                    "{} scripts.{name}: {text}",
                    view.path("package.json")
                )],
            ),
            None => {
                let command = match framework {
                    Framework::Next => "npx next dev",
                    Framework::Nuxt => "npx nuxt dev",
                    Framework::Vite => "npx vite",
                    Framework::Astro => "npx astro dev",
                    _ => return Err("no dev or start script".into()),
                };
                guess(
                    command.to_string(),
                    Confidence::Low,
                    vec![format!(
                        "no dev or start script; {} CLI",
                        framework.as_str()
                    )],
                )
            }
        }
    } else {
        match framework {
            Framework::Laravel => high(
                format!("php artisan serve --port {p}"),
                "laravel local dev server",
            ),
            Framework::Symfony => high(
                format!("php -S 127.0.0.1:{p} -t public"),
                "symfony built-in PHP server",
            ),
            Framework::Django => high(
                format!("python manage.py runserver {p}"),
                "django development server",
            ),
            Framework::Fastapi => {
                let (module, confidence) = if view.has("main.py") {
                    ("main", Confidence::High)
                } else if view.has("app/main.py") {
                    ("app.main", Confidence::High)
                } else {
                    ("main", Confidence::Low)
                };
                guess(
                    format!("uvicorn {module}:app --reload --port {p}"),
                    confidence,
                    vec![format!("fastapi app module {module}")],
                )
            }
            Framework::Flask => high(format!("flask run --port {p}"), "flask development server"),
            Framework::Rails => high(
                format!("bin/rails server -p {p}"),
                "rails development server",
            ),
            Framework::Dotnet => high("dotnet run".into(), "dotnet web project"),
            Framework::Go => {
                let cmds: Vec<&String> = scanned
                    .iter()
                    .filter(|s| s.contains("/cmd/") || s.starts_with("cmd/"))
                    .filter(|s| shell_safe_path(s))
                    .collect();
                match (view.has("main.go"), cmds.as_slice()) {
                    (false, [only]) => {
                        let rel = only
                            .strip_prefix(&join(view.root, ""))
                            .unwrap_or(only)
                            .trim_end_matches("/main.go");
                        high(format!("go run ./{rel}"), "go main package")
                    }
                    _ => high("go run .".into(), "go main package"),
                }
            }
            Framework::Rust => high("cargo run".into(), "rust web crate"),
            _ => return Err("no dev or start script and no known framework".into()),
        }
    };
    let ready_path = detect_ready_path(framework, &scanned, view.contents);
    let framework_confidence = if framework == Framework::Unknown {
        Confidence::Low
    } else {
        Confidence::High
    };
    Ok(Draft {
        root: view.root.to_string(),
        framework: guess(framework, framework_confidence, framework_evidence),
        package_manager,
        start,
        port,
        ready_path,
        also_in_compose: None,
    })
}

fn service_name(root: &str, repo_name: &str) -> String {
    let base = if root == "." {
        repo_name
    } else {
        name_of(root)
    };
    let mapped: String = base
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if mapped.starts_with(|c: char| c.is_ascii_lowercase()) {
        mapped
    } else {
        format!("s-{mapped}")
    }
}

struct ComposeOutcome {
    report: Option<ComposeReport>,
    ignored: Vec<IgnoredComposeFile>,
    /// `(service root, compose service)` of every `native` service.
    natives: Vec<(String, String)>,
}

/// True when `path` can go into a generated shell command unquoted:
/// only `[A-Za-z0-9_./-]`.
fn shell_safe_path(path: &str) -> bool {
    !path.is_empty()
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
}

/// Docker compose's own service-name charset: `[a-zA-Z0-9][a-zA-Z0-9_.-]*`.
fn valid_compose_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn compose_section(
    index: &Index<'_>,
    contents: &BTreeMap<String, String>,
    roots: &[String],
    service_roots: &BTreeSet<String>,
    notes: &mut Vec<String>,
) -> ComposeOutcome {
    let mut files: Vec<&str> = index
        .files
        .iter()
        .copied()
        .filter(|f| is_considered_compose(f, roots))
        .collect();
    files.sort_by_key(|f| (depth(dir_of(f)), *f));
    let mut ignored = Vec::new();
    let mut parsed: Vec<(&str, Vec<ParsedService>)> = Vec::new();
    for file in files {
        if let Some(word) = compose::ignore_word(name_of(file)) {
            ignored.push(IgnoredComposeFile {
                file: file.to_string(),
                reason: format!("production compose file ({word})"),
            });
            continue;
        }
        if !shell_safe_path(file) {
            notes.push(format!(
                "{file}: path has characters guard does not put in a shell command; ignored"
            ));
            continue;
        }
        let Some(text) = contents.get(file) else {
            notes.push(format!("{file} was not read"));
            continue;
        };
        match compose::parse_compose(text) {
            None => notes.push(format!("{file} has no services")),
            Some(mut services) => {
                // Service names end up in `[app].start`: never let one that
                // compose itself would reject reach the shell.
                services.retain(|service| {
                    let valid = valid_compose_name(&service.name);
                    if !valid {
                        notes.push(format!(
                            "{file} service {}: not a valid compose service name; ignored",
                            fragment(&service.name)
                        ));
                    }
                    valid
                });
                for service in &services {
                    for problem in &service.unreadable {
                        notes.push(format!("{file} service {}: {problem}", service.name));
                    }
                }
                parsed.push((file, services));
            }
        }
    }
    let has_infra = |services: &[ParsedService]| {
        services
            .iter()
            .any(|s| s.image.as_deref().is_some_and(compose::is_infra_image))
    };
    let used = parsed
        .iter()
        .position(|(_, s)| has_infra(s))
        .or((!parsed.is_empty()).then_some(0));
    let Some(used) = used else {
        return ComposeOutcome {
            report: None,
            ignored,
            natives: Vec::new(),
        };
    };
    let (file, services) = &parsed[used];
    let compose_dir = dir_of(file);
    let known_infra: BTreeSet<&str> = services
        .iter()
        .filter(|s| s.image.as_deref().is_some_and(compose::is_infra_image))
        .map(|s| s.name.as_str())
        .collect();
    let mut natives = Vec::new();
    let mut infra_names: Vec<String> = Vec::new();
    let mut dependency_evidence: Vec<String> = Vec::new();
    let mut other_notes: Vec<String> = Vec::new();
    let mut reports = Vec::new();
    for service in services {
        let build = service
            .build
            .as_deref()
            .map(|ctx| normalize(&join(compose_dir, ctx)).unwrap_or_else(|| ctx.to_string()));
        let role = if let Some(b) = build.as_ref().filter(|b| service_roots.contains(*b)) {
            // A second compose service building the same root (a queue worker
            // next to the web server) is not the native dev command: say so.
            if let Some((_, first)) = natives.iter().find(|(root, _)| root == b) {
                let command = service
                    .command
                    .as_deref()
                    .map(|c| format!(" (command: {})", fragment(c)))
                    .unwrap_or_default();
                other_notes.push(format!(
                    "compose service {} also builds {b}, which runs natively as {first}; it is not started{command}: add a [[services]] entry if the app needs it",
                    service.name
                ));
            }
            natives.push((b.clone(), service.name.clone()));
            ComposeRole::Native
        } else if known_infra.contains(service.name.as_str()) {
            infra_names.push(service.name.clone());
            ComposeRole::Infra
        } else if service.image.is_some()
            && service.build.is_none()
            && !service.depends_on.is_empty()
            && service
                .depends_on
                .iter()
                .all(|d| known_infra.contains(d.as_str()))
        {
            infra_names.push(service.name.clone());
            dependency_evidence.push(format!(
                "{file} service {} depends only on infrastructure ({})",
                service.name,
                service.depends_on.join(", ")
            ));
            ComposeRole::Infra
        } else {
            let what = service
                .image
                .clone()
                .or_else(|| build.as_ref().map(|b| format!("build {b}")))
                .unwrap_or_else(|| "no image".into());
            other_notes.push(format!(
                "compose service {} ({what}) is not started by default",
                service.name
            ));
            ComposeRole::Other
        };
        reports.push(ComposeServiceReport {
            name: service.name.clone(),
            image: service.image.clone(),
            build,
            ports: service.ports.clone(),
            role,
        });
    }
    let whole_stack = service_roots.is_empty() && ROOT_COMPOSE_FILES.contains(file);
    let (start, stop) = if whole_stack {
        let why = vec!["no service found; the whole compose stack is the app".to_string()];
        (
            Some(guess(
                "docker compose up -d --wait".to_string(),
                Confidence::Medium,
                why.clone(),
            )),
            Some(guess(
                "docker compose down -v".to_string(),
                Confidence::Medium,
                why,
            )),
        )
    } else if infra_names.is_empty() {
        (None, None)
    } else {
        let mut evidence = vec![format!("{file} services {}", infra_names.join(", "))];
        evidence.extend(dependency_evidence);
        (
            Some(guess(
                format!(
                    "docker compose -f {file} up -d --wait {}",
                    infra_names.join(" ")
                ),
                Confidence::High,
                evidence,
            )),
            Some(guess(
                format!("docker compose -f {file} down"),
                Confidence::High,
                vec![file.to_string()],
            )),
        )
    };
    if !whole_stack {
        notes.extend(other_notes);
    }
    for (other, services) in &parsed {
        if other != file && has_infra(services) {
            notes.push(format!("{other} has infrastructure but {file} is used"));
        }
    }
    ComposeOutcome {
        report: Some(ComposeReport {
            file: file.to_string(),
            services: reports,
            start,
            stop,
        }),
        ignored,
        natives,
    }
}

/// Detects how the app runs. Same input, same report.
pub fn detect_app(input: &DetectInput) -> DetectionReport {
    let index = Index::new(&input.files);
    let (roots, unsupported) = index.roots();
    let mut notes = Vec::new();
    let mut skipped = Vec::new();
    let mut drafts = Vec::new();
    for root in &roots {
        let view = RootView {
            root,
            index: &index,
            contents: &input.contents,
        };
        match analyze_root(&view) {
            Ok(draft) => drafts.push(draft),
            Err(reason) => skipped.push(SkippedRoot {
                root: root.clone(),
                reason,
            }),
        }
    }
    for (root, manifest) in unsupported {
        skipped.push(SkippedRoot {
            root,
            reason: format!("no supported manifest ({manifest})"),
        });
    }
    skipped.sort_by(|a, b| a.root.cmp(&b.root));

    let service_roots: BTreeSet<String> = drafts.iter().map(|d| d.root.clone()).collect();
    let compose = compose_section(&index, &input.contents, &roots, &service_roots, &mut notes);
    for (root, compose_service) in &compose.natives {
        if let Some(draft) = drafts.iter_mut().find(|d| &d.root == root)
            && draft.also_in_compose.is_none()
        {
            draft.also_in_compose = Some(compose_service.clone());
            if let Some(report) = &compose.report {
                draft.start.evidence.push(format!(
                    "{} service {compose_service} builds {root}",
                    report.file
                ));
            }
        }
    }

    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut named: Vec<(String, Draft)> = Vec::new();
    for draft in drafts {
        let natural = service_name(&draft.root, &input.repo_name);
        let name = if taken.contains(&natural) {
            (2..)
                .map(|n| format!("{natural}-{n}"))
                .find(|n| !taken.contains(n))
                .expect("an unbounded range always has a free suffix")
        } else {
            natural
        };
        taken.insert(name.clone());
        named.push((name, draft));
    }
    named.sort_by(|(_, a), (_, b)| {
        (a.framework.value.kind(), &a.root).cmp(&(b.framework.value.kind(), &b.root))
    });

    let mut by_port: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    for (i, (_, draft)) in named.iter().enumerate() {
        by_port.entry(draft.port.value).or_default().push(i);
    }
    for (port, owners) in by_port.iter().filter(|(_, o)| o.len() > 1) {
        let names: Vec<String> = owners.iter().map(|&i| named[i].0.clone()).collect();
        for &i in owners {
            named[i].1.port.confidence = Confidence::Low;
        }
        notes.push(format!("port {port} claimed by {}", and_list(&names)));
    }

    let services: Vec<DetectedService> = named
        .into_iter()
        .map(|(name, d)| {
            let kind = d.framework.value.kind();
            let confidence = d
                .framework
                .confidence
                .min(d.start.confidence)
                .min(d.port.confidence);
            DetectedService {
                ready_url: format!("http://localhost:{}{}", d.port.value, d.ready_path.value),
                name,
                root: d.root,
                kind,
                confidence,
                framework: d.framework,
                package_manager: d.package_manager,
                start: d.start,
                port: d.port,
                ready_path: d.ready_path,
                also_in_compose: d.also_in_compose,
            }
        })
        .collect();

    let primary = [ServiceKind::Frontend, ServiceKind::Backend]
        .into_iter()
        .find_map(|kind| {
            services.iter().find(|s| s.kind == kind).map(|s| {
                let label = if kind == ServiceKind::Frontend {
                    "frontend"
                } else {
                    "backend"
                };
                (s, format!("first {label} service {}", s.name))
            })
        })
        .or_else(|| {
            services
                .first()
                .map(|s| (s, format!("first service {}", s.name)))
        });
    let (base_url, ready_path) = match primary {
        Some((s, why)) => (
            guess(
                format!("http://localhost:{}", s.port.value),
                s.port.confidence,
                vec![why],
            ),
            guess(
                s.ready_path.value.clone(),
                s.ready_path.confidence,
                vec![format!("{} ready path", s.name)],
            ),
        ),
        None => (
            guess(
                "http://localhost:3000".into(),
                Confidence::Low,
                vec!["no service detected".into()],
            ),
            guess(
                "/healthz".into(),
                Confidence::Low,
                vec!["no service detected".into()],
            ),
        ),
    };
    let infra_start = compose
        .report
        .as_ref()
        .is_some_and(|c| c.start.is_some() && !services.is_empty());
    let target_shape = match services.as_slice() {
        [] => TargetShape::Single,
        [only] if only.root == "." && !infra_start => TargetShape::Single,
        _ => TargetShape::Services,
    };
    DetectionReport {
        version: 1,
        services,
        skipped,
        compose: compose.report,
        ignored_compose_files: compose.ignored,
        base_url,
        ready_path,
        target_shape,
        notes,
    }
}
