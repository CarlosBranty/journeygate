//! [`ImpactIndex`]: the reverse import graph of a repository plus the
//! `target.toml` path rules, implementing [`ImpactAnalyzer`].

use crate::extract::{Extractor, RawImport};
use crate::lang::{Grammar, Language};
use crate::resolve::{Repo, file_name};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use guard_core::config::TargetConfig;
use guard_core::ports::{Area, ImpactAnalyzer, ImpactError, ImpactFacts};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

/// Path patterns of a schema migration (`docs/contracts/ports.md`).
pub const MIGRATION_GLOBS: [&str; 6] = [
    "**/migrations/**",
    "**/migrate/**",
    "**/alembic/versions/**",
    "**/db/schema.rb",
    "**/schema.prisma",
    "**/*.sql",
];

/// Files larger than this are listed but not parsed (generated bundles, vendored code).
pub const MAX_PARSED_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// At most this many paths come back from `resolve_mentions`.
pub const MAX_MENTION_PATHS: usize = 200;

/// How a file's impact was computed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisMethod {
    /// Through the tree-sitter import graph.
    Graph,
    /// No plugin for the file's language: its own imports are not parsed and
    /// only the `target.toml` globs classify it (areas, hard rules,
    /// migrations). Fan-out still comes from the graph, so a file that other
    /// files import (`.css`, `.json`, a `#[path]` target) can be non-zero.
    GlobFallback,
}

/// Impact of one changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileImpact {
    pub path: String,
    pub language: Option<Language>,
    pub method: AnalysisMethod,
    /// Distinct repository files importing it directly.
    pub direct_fan_out: u32,
    /// Distinct repository files reaching it through any chain of imports.
    pub transitive_fan_out: u32,
    pub areas: Vec<Area>,
    pub human_only: bool,
    pub schema_migration: bool,
}

/// The analyzer, built once per process from a checkout and its `target.toml`.
#[derive(Debug, Clone)]
pub struct ImpactIndex {
    files: BTreeSet<String>,
    enabled: BTreeSet<Language>,
    fallback_languages: Vec<String>,
    imports: BTreeMap<String, BTreeSet<String>>,
    importers: BTreeMap<String, BTreeSet<String>>,
    areas: Vec<(Area, GlobSet)>,
    human_only: GlobSet,
    migrations: GlobSet,
}

impl ImpactIndex {
    /// Walks `root` (honoring `.gitignore`, `.ignore` and `.git/info/exclude`,
    /// skipping hidden files and `.git/`), parses every file of an enabled
    /// language and builds the graph.
    pub fn build(root: &Path, config: &TargetConfig) -> Result<Self, ImpactError> {
        if !root.is_dir() {
            return Err(ImpactError::Io(format!(
                "{} is not a directory",
                root.display()
            )));
        }
        let mut files = BTreeSet::new();
        let walker = ignore::WalkBuilder::new(root)
            .require_git(false)
            .filter_entry(|e| e.file_name() != ".git")
            .build();
        for entry in walker {
            let entry = entry.map_err(|e| ImpactError::Io(e.to_string()))?;
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if !rel.is_empty() {
                files.insert(rel);
            }
        }
        Self::build_with(files, config, &|rel: &str| {
            let path = root.join(rel);
            let meta = std::fs::metadata(&path).ok()?;
            if meta.len() > MAX_PARSED_FILE_BYTES {
                return None;
            }
            std::fs::read_to_string(path).ok()
        })
    }

    /// Builds the index from in-memory sources (`path -> contents`); every key
    /// is a repository file. For callers that already hold the tree (and tests).
    pub fn from_sources<I, P, C>(sources: I, config: &TargetConfig) -> Result<Self, ImpactError>
    where
        I: IntoIterator<Item = (P, C)>,
        P: Into<String>,
        C: Into<String>,
    {
        let map: BTreeMap<String, String> = sources
            .into_iter()
            .map(|(p, c)| (normalize_input(&p.into()), c.into()))
            .collect();
        let files = map.keys().cloned().collect();
        Self::build_with(files, config, &|p: &str| map.get(p).cloned())
    }

    fn build_with(
        files: BTreeSet<String>,
        config: &TargetConfig,
        read: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, ImpactError> {
        let (enabled, fallback_languages) = enabled_languages(&config.app.languages);
        let areas = config
            .areas
            .iter()
            .map(|(key, area)| Ok((Area::new(key.clone()), glob_set(&area.paths)?)))
            .collect::<Result<Vec<_>, ImpactError>>()?;
        let human_only = glob_set(&config.hard_rules.human_only_paths)?;
        let migrations = glob_set(&MIGRATION_GLOBS)?;

        let grammars: Vec<Grammar> = Grammar::ALL
            .into_iter()
            .filter(|g| enabled.contains(&g.language()))
            .collect();
        let extractor = Extractor::new(grammars)?;
        let ts_or_js =
            enabled.contains(&Language::TypeScript) || enabled.contains(&Language::JavaScript);
        let manifests = |p: &str| match file_name(p) {
            "go.mod" if enabled.contains(&Language::Go) => read(p),
            "Cargo.toml" if enabled.contains(&Language::Rust) => read(p),
            "pyproject.toml" if enabled.contains(&Language::Python) => read(p),
            "composer.json" if enabled.contains(&Language::Php) => read(p),
            // MSBuild project files: SDK, implicit usings, references.
            name if enabled.contains(&Language::CSharp)
                && (name.ends_with(".csproj")
                    || name == "Directory.Build.props"
                    || name == "Directory.Build.targets") =>
            {
                read(p)
            }
            // tsconfig.json / jsconfig.json and the JSON files they extend.
            name if ts_or_js && name.ends_with(".json") => read(p),
            _ => None,
        };
        let mut repo = Repo::new(files, &manifests);

        // Extract every file first: Rust module trees need the `mod`
        // declarations of the whole crate before any `use` resolves.
        let mut raw_by_file: BTreeMap<String, (Grammar, Vec<RawImport>)> = BTreeMap::new();
        for file in repo.files() {
            let Some(grammar) = Grammar::for_path(file) else {
                continue;
            };
            if !enabled.contains(&grammar.language()) {
                continue;
            }
            let Some(source) = read(file) else {
                continue;
            };
            raw_by_file.insert(file.clone(), (grammar, extractor.extract(grammar, &source)));
        }
        if enabled.contains(&Language::Rust) {
            let rust: BTreeMap<String, Vec<RawImport>> = raw_by_file
                .iter()
                .filter(|(_, (g, _))| matches!(g, Grammar::Rust))
                .map(|(f, (_, raw))| (f.clone(), raw.clone()))
                .collect();
            repo.prepare_rust(&rust);
        }
        if enabled.contains(&Language::CSharp) {
            // C# references resolve through the types every file declares.
            let csharp: BTreeMap<String, Vec<RawImport>> = raw_by_file
                .iter()
                .filter(|(_, (g, _))| matches!(g, Grammar::CSharp))
                .map(|(f, (_, raw))| (f.clone(), raw.clone()))
                .collect();
            repo.prepare_csharp(&csharp);
        }
        if enabled.contains(&Language::Php) {
            // Classes outside the autoload maps resolve through declarations.
            let php: BTreeMap<String, Vec<RawImport>> = raw_by_file
                .iter()
                .filter(|(_, (g, _))| matches!(g, Grammar::Php))
                .map(|(f, (_, raw))| (f.clone(), raw.clone()))
                .collect();
            repo.prepare_php(&php);
        }

        let mut imports: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut importers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (file, (grammar, raw)) in &raw_by_file {
            let targets = repo.resolve(file, *grammar, raw);
            for t in &targets {
                importers.entry(t.clone()).or_default().insert(file.clone());
            }
            if !targets.is_empty() {
                imports.insert(file.clone(), targets);
            }
        }

        Ok(Self {
            files: repo.files().clone(),
            enabled,
            fallback_languages,
            imports,
            importers,
            areas,
            human_only,
            migrations,
        })
    }

    /// Every repository file the walk found, sorted.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(String::as_str)
    }

    /// Languages analyzed through the graph.
    pub fn enabled_languages(&self) -> impl Iterator<Item = Language> + '_ {
        self.enabled.iter().copied()
    }

    /// `[app].languages` entries without a plugin: their files fall back to globs.
    pub fn fallback_languages(&self) -> &[String] {
        &self.fallback_languages
    }

    /// The enabled plugin that analyzes `path`, if any.
    pub fn language_of(&self, path: &str) -> Option<Language> {
        Language::for_path(path).filter(|l| self.enabled.contains(l))
    }

    /// Repository files `path` imports directly, sorted.
    pub fn imports_of(&self, path: &str) -> Vec<&str> {
        let path = normalize_input(path);
        self.imports
            .get(&path)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// Repository files importing `path` directly, sorted.
    pub fn importers_of(&self, path: &str) -> Vec<&str> {
        let path = normalize_input(path);
        self.importers
            .get(&path)
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect()
    }

    /// Repository files reaching `path` through any chain of imports (cycles
    /// included once, `path` itself excluded), sorted.
    pub fn transitive_importers(&self, path: &str) -> Vec<&str> {
        let start = normalize_input(path);
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        if let Some(direct) = self.importers.get(&start) {
            queue.extend(direct.iter().map(String::as_str));
        }
        while let Some(f) = queue.pop_front() {
            if f == start || !seen.insert(f) {
                continue;
            }
            if let Some(next) = self.importers.get(f) {
                queue.extend(next.iter().map(String::as_str));
            }
        }
        seen.into_iter().collect()
    }

    pub fn direct_fan_out(&self, path: &str) -> u32 {
        saturating_u32(self.importers_of(path).len())
    }

    pub fn transitive_fan_out(&self, path: &str) -> u32 {
        saturating_u32(self.transitive_importers(path).len())
    }

    pub fn is_schema_migration(&self, path: &str) -> bool {
        self.migrations.is_match(normalize_input(path))
    }

    pub fn is_human_only(&self, path: &str) -> bool {
        self.human_only.is_match(normalize_input(path))
    }

    fn areas_of(&self, path: &str) -> Vec<Area> {
        self.areas
            .iter()
            .filter(|(_, set)| set.is_match(path))
            .map(|(a, _)| a.clone())
            .collect()
    }

    /// Per-file impact of `changed`, sorted by path and deduplicated.
    pub fn file_impacts(&self, changed: &[String]) -> Vec<FileImpact> {
        let paths: BTreeSet<String> = changed.iter().map(|p| normalize_input(p)).collect();
        paths
            .into_iter()
            .map(|path| {
                let language = self.language_of(&path);
                FileImpact {
                    language,
                    method: if language.is_some() {
                        AnalysisMethod::Graph
                    } else {
                        AnalysisMethod::GlobFallback
                    },
                    direct_fan_out: self.direct_fan_out(&path),
                    transitive_fan_out: self.transitive_fan_out(&path),
                    areas: self.areas_of(&path),
                    human_only: self.is_human_only(&path),
                    schema_migration: self.is_schema_migration(&path),
                    path,
                }
            })
            .collect()
    }
}

impl ImpactAnalyzer for ImpactIndex {
    fn fan_out(&self, changed_files: &[String]) -> Result<ImpactFacts, ImpactError> {
        let impacts = self.file_impacts(changed_files);
        let fan_out: BTreeMap<String, u32> = impacts
            .iter()
            .map(|i| (i.path.clone(), i.direct_fan_out))
            .collect();
        let human_only_paths: Vec<String> = impacts
            .iter()
            .filter(|i| i.human_only)
            .map(|i| i.path.clone())
            .collect();
        Ok(ImpactFacts {
            max_fan_out: fan_out.values().copied().max().unwrap_or(0),
            fan_out,
            human_only_path_hit: !human_only_paths.is_empty(),
            human_only_paths,
            schema_migration: impacts.iter().any(|i| i.schema_migration),
        })
    }

    fn areas_for(&self, changed_paths: &[String]) -> Vec<Area> {
        changed_paths
            .iter()
            .flat_map(|p| self.areas_of(&normalize_input(p)))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn resolve_mentions(&self, mentions: &[String]) -> Vec<String> {
        let mut out = BTreeSet::new();
        for mention in mentions {
            let m = normalize_input(mention.trim());
            if m.is_empty() {
                continue;
            }
            let suffix = format!("/{m}");
            let stems = if m.contains(['/', '.']) {
                Vec::new()
            } else {
                stem_variants(&m)
            };
            for f in &self.files {
                let by_path = *f == m || f.ends_with(&suffix);
                let by_stem = !stems.is_empty() && {
                    let name = file_name(f);
                    let stem = name.split('.').next().unwrap_or(name);
                    name.contains('.') && stems.iter().any(|s| s == stem)
                };
                if by_path || by_stem {
                    out.insert(f.clone());
                }
            }
        }
        if out.len() <= MAX_MENTION_PATHS {
            return out.into_iter().collect();
        }
        // Over the cap: keep the hard-rule paths (human-only, migrations)
        // first, so truncation never hides a rule hit from `fan_out` (fail
        // closed); fill the rest alphabetically; return sorted.
        let (hard, rest): (Vec<String>, Vec<String>) = out
            .into_iter()
            .partition(|p| self.is_human_only(p) || self.is_schema_migration(p));
        let mut kept: Vec<String> = hard
            .into_iter()
            .chain(rest)
            .take(MAX_MENTION_PATHS)
            .collect();
        kept.sort();
        kept
    }
}

/// `OrderService` -> `OrderService`, `order-service`, `order_service`.
fn stem_variants(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' || c == '_' || c == ' ' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let boundary = c.is_uppercase()
            && prev.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.extend(c.to_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut out = vec![name.to_string(), words.join("-"), words.join("_")];
    out.dedup();
    out
}

/// Turns `[app].languages` into enabled plugins and fallback names. An empty
/// list enables every plugin (nothing declared = analyze what we can).
fn enabled_languages(declared: &[String]) -> (BTreeSet<Language>, Vec<String>) {
    if declared.is_empty() {
        return (Language::ALL.into_iter().collect(), Vec::new());
    }
    let mut enabled = BTreeSet::new();
    let mut fallback = Vec::new();
    for name in declared {
        match Language::from_name(name) {
            Some(l) => enabled.extend(l.enables().iter().copied()),
            None => fallback.push(name.clone()),
        }
    }
    (enabled, fallback)
}

fn glob_set<S: AsRef<str>>(globs: &[S]) -> Result<GlobSet, ImpactError> {
    let mut builder = GlobSetBuilder::new();
    for g in globs {
        let g = g.as_ref();
        let glob = GlobBuilder::new(g)
            .literal_separator(true)
            .build()
            .map_err(|e| ImpactError::InvalidGlob(format!("{g}: {e}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|e| ImpactError::InvalidGlob(e.to_string()))
}

fn normalize_input(path: &str) -> String {
    let p = path.replace('\\', "/");
    let mut p = p.as_str();
    while let Some(rest) = p.strip_prefix("./") {
        p = rest;
    }
    p.to_string()
}

fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::config::parse_target;

    fn config(languages: &str, extra: &str) -> TargetConfig {
        parse_target(&format!(
            "version = 1\n[app]\nname = \"t\"\nlanguages = {languages}\nbase_url = \"http://localhost\"\n\
             [areas.web]\ndescription = \"Web\"\npaths = [\"web/**\"]\n{extra}"
        ))
        .unwrap()
    }

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn cycles_count_each_file_once_and_exclude_the_start() {
        let idx = ImpactIndex::from_sources(
            [
                ("a.ts", "import './b';"),
                ("b.ts", "import './c';"),
                ("c.ts", "import './a';"),
                ("d.ts", "import './a';"),
            ],
            &config("[\"typescript\"]", ""),
        )
        .unwrap();
        assert_eq!(idx.direct_fan_out("a.ts"), 2);
        assert_eq!(
            idx.transitive_importers("a.ts"),
            vec!["b.ts", "c.ts", "d.ts"]
        );
    }

    #[test]
    fn self_imports_are_not_edges() {
        let idx =
            ImpactIndex::from_sources([("a.ts", "import './a';")], &config("[\"typescript\"]", ""))
                .unwrap();
        assert_eq!(idx.direct_fan_out("a.ts"), 0);
    }

    #[test]
    fn disabled_languages_are_not_parsed() {
        let idx = ImpactIndex::from_sources(
            [
                ("a.py", "from . import b"),
                ("b.py", ""),
                ("x.ts", "import './y';"),
                ("y.ts", ""),
            ],
            &config("[\"python\"]", ""),
        )
        .unwrap();
        assert_eq!(idx.direct_fan_out("b.py"), 1);
        assert_eq!(idx.direct_fan_out("y.ts"), 0);
        assert_eq!(idx.language_of("x.ts"), None);
        assert!(idx.fallback_languages().is_empty());
    }

    #[test]
    fn empty_language_list_enables_every_plugin() {
        let idx =
            ImpactIndex::from_sources([("x.ts", "import './y';"), ("y.ts", "")], &config("[]", ""))
                .unwrap();
        assert_eq!(idx.direct_fan_out("y.ts"), 1);
        assert_eq!(idx.enabled_languages().count(), Language::ALL.len());
    }

    #[test]
    fn migration_patterns_follow_the_contract() {
        let idx =
            ImpactIndex::from_sources(Vec::<(String, String)>::new(), &config("[]", "")).unwrap();
        for p in [
            "migrations/001.sql",
            "services/orders/migrations/002_add.py",
            "db/migrate/2024_init.rb",
            "app/alembic/versions/abc.py",
            "db/schema.rb",
            "prisma/schema.prisma",
            "sql/seed.sql",
            ".\\db\\migrations\\x.go",
        ] {
            assert!(idx.is_schema_migration(p), "{p}");
        }
        for p in ["src/migration.ts", "docs/migrations.md", "schema.rb"] {
            assert!(!idx.is_schema_migration(p), "{p}");
        }
    }

    #[test]
    fn hard_rules_and_areas_use_literal_separators() {
        let idx = ImpactIndex::from_sources(
            Vec::<(String, String)>::new(),
            &config("[]", "[hard_rules]\nhuman_only_paths = [\"infra/*\"]\n"),
        )
        .unwrap();
        let facts = idx
            .fan_out(&s(&["infra/a.tf", "infra/deep/b.tf", "web\\x.ts"]))
            .unwrap();
        assert_eq!(facts.human_only_paths, vec!["infra/a.tf"]);
        assert!(facts.human_only_path_hit);
        assert!(!facts.schema_migration);
        assert_eq!(facts.fan_out.len(), 3);
        assert!(facts.fan_out.contains_key("web/x.ts"));
        assert_eq!(
            idx.areas_for(&s(&["web/x.ts", "web/y.ts", "api/z.go"])),
            vec![Area::new("web")]
        );
        assert!(idx.areas_for(&s(&["api/z.go"])).is_empty());
    }

    #[test]
    fn empty_change_set_has_no_facts() {
        let idx =
            ImpactIndex::from_sources(Vec::<(String, String)>::new(), &config("[]", "")).unwrap();
        assert_eq!(idx.fan_out(&[]).unwrap(), ImpactFacts::default());
    }

    #[test]
    fn invalid_globs_fail_the_build() {
        let mut cfg = config("[]", "");
        cfg.hard_rules.human_only_paths = vec!["a/[".into()];
        let err = ImpactIndex::from_sources(Vec::<(String, String)>::new(), &cfg).unwrap_err();
        assert!(
            matches!(err, ImpactError::InvalidGlob(ref m) if m.starts_with("a/[")),
            "{err:?}"
        );
    }

    #[test]
    fn mentions_match_paths_suffixes_and_stem_variants() {
        let idx = ImpactIndex::from_sources(
            [
                ("src/orders/OrderService.ts", ""),
                ("src/orders/order-service.test.ts", ""),
                ("py/order_service.py", ""),
                ("src/orders/OrderServiceImpl.ts", ""),
                ("README", ""),
            ],
            &config("[]", ""),
        )
        .unwrap();
        assert_eq!(
            idx.resolve_mentions(&s(&["OrderService"])),
            vec![
                "py/order_service.py",
                "src/orders/OrderService.ts",
                "src/orders/order-service.test.ts"
            ]
        );
        assert_eq!(
            idx.resolve_mentions(&s(&["orders/OrderServiceImpl.ts", " ", "README"])),
            vec!["README", "src/orders/OrderServiceImpl.ts"]
        );
        assert!(
            idx.resolve_mentions(&s(&["rders/OrderService.ts"]))
                .is_empty()
        );
    }

    #[test]
    fn mentions_are_capped() {
        let files: Vec<(String, String)> = (0..250)
            .map(|i| (format!("m{i:03}/Thing.ts"), String::new()))
            .collect();
        let idx = ImpactIndex::from_sources(files, &config("[]", "")).unwrap();
        let got = idx.resolve_mentions(&s(&["Thing"]));
        assert_eq!(got.len(), MAX_MENTION_PATHS);
        assert_eq!(got[0], "m000/Thing.ts");
    }

    #[test]
    fn the_cap_never_drops_hard_rule_paths() {
        let mut files: Vec<(String, String)> = (0..250)
            .map(|i| (format!("m{i:03}/Thing.ts"), String::new()))
            .collect();
        // Both sort after the 200 alphabetically first matches.
        files.push(("zinfra/Thing.ts".into(), String::new()));
        files.push(("zz/migrations/0001_Thing.sql".into(), String::new()));
        files.push(("zz/db/migrations/Thing.sql".into(), String::new()));
        let idx = ImpactIndex::from_sources(
            files,
            &config("[]", "[hard_rules]\nhuman_only_paths = [\"zinfra/**\"]\n"),
        )
        .unwrap();
        let got = idx.resolve_mentions(&s(&["Thing"]));
        assert_eq!(got.len(), MAX_MENTION_PATHS);
        assert!(got.contains(&"zinfra/Thing.ts".to_string()), "{got:?}");
        assert!(
            got.contains(&"zz/db/migrations/Thing.sql".to_string()),
            "{got:?}"
        );
        let mut sorted = got.clone();
        sorted.sort();
        assert_eq!(got, sorted, "still sorted");
        let facts = idx.fan_out(&got).unwrap();
        assert!(facts.human_only_path_hit);
        assert!(facts.schema_migration);
    }

    #[test]
    fn stem_variants_split_camel_case_and_acronyms() {
        assert_eq!(
            stem_variants("OrderService"),
            vec!["OrderService", "order-service", "order_service"]
        );
        assert_eq!(
            stem_variants("HTTPClient"),
            vec!["HTTPClient", "http-client", "http_client"]
        );
        assert_eq!(stem_variants("cart"), vec!["cart"]);
    }

    #[test]
    fn build_honors_gitignore_and_skips_hidden_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, body: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        write(".gitignore", "dist/\n");
        write("src/money.ts", "export const m = 1;");
        write("src/a.ts", "import './money';");
        write("dist/bundle.js", "require('../src/money');");
        write(".hidden/c.ts", "import '../src/money';");
        let idx = ImpactIndex::build(root, &config("[\"typescript\"]", "")).unwrap();
        assert_eq!(idx.importers_of("src/money.ts"), vec!["src/a.ts"]);
        assert!(!idx.files().any(|f| f.starts_with("dist/")));
    }

    #[test]
    fn build_fails_on_a_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        let err = ImpactIndex::build(&dir.path().join("nope"), &config("[]", "")).unwrap_err();
        assert!(matches!(err, ImpactError::Io(_)));
    }

    #[test]
    fn the_index_is_an_object_safe_analyzer() {
        let idx =
            ImpactIndex::from_sources(Vec::<(String, String)>::new(), &config("[]", "")).unwrap();
        let shared: std::sync::Arc<dyn ImpactAnalyzer> = std::sync::Arc::new(idx);
        assert!(shared.resolve_mentions(&[]).is_empty());
    }
}
