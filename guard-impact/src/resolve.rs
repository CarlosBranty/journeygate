//! Resolution of raw imports to repository files, one resolver per language.
//!
//! Every path here is repo-relative with forward slashes. Imports that do not
//! resolve to a repository file (packages, the standard library) are dropped.

use crate::cargo::{self, RustCrate};
use crate::csharp::{CsFact, CsIndex};
use crate::extract::RawImport;
use crate::lang::Grammar;
use crate::php::{PhpFact, PhpIndex};
use crate::tsconfig::TsConfigs;
use std::collections::{BTreeMap, BTreeSet};

/// The repository files plus the manifests some resolvers need.
pub(crate) struct Repo {
    files: BTreeSet<String>,
    /// directory -> files directly inside it.
    by_dir: BTreeMap<String, Vec<String>>,
    /// `(module path, go.mod directory)`, longest module first.
    go_modules: Vec<(String, String)>,
    /// Rust packages of the repository (every `Cargo.toml` with `[package]`),
    /// deepest directory first.
    crates: Vec<RustCrate>,
    /// Module trees of every Rust target, built by [`Repo::prepare_rust`].
    rust_trees: Vec<RustTree>,
    /// Target root file -> index into `rust_trees`.
    tree_of: BTreeMap<String, usize>,
    /// Module context of every Rust file reached from a target root.
    rust_contexts: BTreeMap<String, RustContext>,
    /// Python search roots configured in `pyproject.toml` files.
    py_roots: Vec<String>,
    /// `tsconfig.json` / `jsconfig.json` resolution options.
    ts: TsConfigs,
    /// MSBuild files (`.csproj`, `Directory.Build.props` / `.targets`) by path,
    /// read when C# is enabled.
    cs_manifests: BTreeMap<String, String>,
    /// C# symbol table, built by [`Repo::prepare_csharp`].
    cs: CsIndex,
    /// Composer autoload maps (from `Repo::new`) and the PHP symbol table
    /// (from [`Repo::prepare_php`]).
    php: PhpIndex,
}

/// The module tree of one Rust target: module path -> the file defining it
/// (inline modules map to the file holding them).
#[derive(Debug, Default)]
struct RustTree {
    modules: BTreeMap<Vec<String>, String>,
    /// `extern crate krate as alias;` anywhere in the target: alias -> krate.
    extern_aliases: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct RustContext {
    /// Tree of the target the file belongs to.
    tree: Option<usize>,
    /// Directory of the target root: module paths missing from the tree
    /// (macro-generated `mod`s) are looked up there by location.
    root_dir: String,
    /// Module path of the file (inline scopes are added per import).
    module_path: Vec<String>,
    /// Directory holding the files of the `mod x;` declared in this file.
    mod_dir: String,
    root_file: Option<String>,
    /// Package of the file (index into `crates`).
    krate: Option<usize>,
}

impl Repo {
    /// `read` returns the contents of a manifest (`go.mod`, `Cargo.toml`,
    /// `pyproject.toml`, `tsconfig.json` / `jsconfig.json` and the JSON files
    /// they extend, `composer.json`, `.csproj`, `Directory.Build.props` /
    /// `.targets`); `None` for a manifest the enabled
    /// languages do not need.
    pub(crate) fn new(files: BTreeSet<String>, read: &dyn Fn(&str) -> Option<String>) -> Self {
        let mut by_dir: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in &files {
            by_dir
                .entry(parent(f).to_string())
                .or_default()
                .push(f.clone());
        }
        let mut go_modules = Vec::new();
        let mut cargo_manifests = Vec::new();
        let mut ts_configs = Vec::new();
        let mut py_roots: Vec<String> = Vec::new();
        let mut composer: Vec<(String, String)> = Vec::new();
        let mut cs_manifests: BTreeMap<String, String> = BTreeMap::new();
        for f in &files {
            match file_name(f) {
                name if name.ends_with(".csproj")
                    || name == "Directory.Build.props"
                    || name == "Directory.Build.targets" =>
                {
                    if let Some(text) = read(f) {
                        cs_manifests.insert(f.clone(), text);
                    }
                }
                "go.mod" => {
                    if let Some(module) = read(f).as_deref().and_then(go_module_path) {
                        go_modules.push((module, parent(f).to_string()));
                    }
                }
                "Cargo.toml" => cargo_manifests.push(f.clone()),
                "tsconfig.json" | "jsconfig.json" => ts_configs.push(f.clone()),
                "composer.json" => {
                    if let Some(text) = read(f) {
                        composer.push((f.clone(), text));
                    }
                }
                "pyproject.toml" => {
                    if let Some(text) = read(f) {
                        for root in pyproject_roots(parent(f), &text) {
                            if !py_roots.contains(&root) {
                                py_roots.push(root);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        go_modules.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.cmp(b)));
        let mut crates = cargo::discover(&files, &cargo_manifests, read);
        crates.sort_by(|a, b| b.dir.len().cmp(&a.dir.len()).then(a.dir.cmp(&b.dir)));
        let ts = TsConfigs::load(&files, &ts_configs, read);
        Self {
            files,
            by_dir,
            go_modules,
            crates,
            rust_trees: Vec::new(),
            tree_of: BTreeMap::new(),
            rust_contexts: BTreeMap::new(),
            py_roots,
            ts,
            cs_manifests,
            cs: CsIndex::default(),
            php: PhpIndex::load(&composer),
        }
    }

    pub(crate) fn files(&self) -> &BTreeSet<String> {
        &self.files
    }

    fn has(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    /// Repository files `from` depends on through `imports`, without `from` itself.
    pub(crate) fn resolve(
        &self,
        from: &str,
        grammar: Grammar,
        imports: &[RawImport],
    ) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let rust = matches!(grammar, Grammar::Rust).then(|| self.rust_context(from));
        for imp in imports {
            match imp {
                RawImport::Specifier(spec) => out.extend(self.resolve_node(from, spec)),
                RawImport::GoPath(path) => out.extend(self.resolve_go(path)),
                RawImport::PyModule(module) => out.extend(self.resolve_python(from, module, None)),
                RawImport::PyFrom { module, name } => {
                    out.extend(self.resolve_python(from, module, name.as_deref()))
                }
                RawImport::RustUse { segments, scope } => {
                    if let Some(ctx) = &rust {
                        out.extend(self.resolve_rust_use(from, ctx, segments, scope));
                    }
                }
                RawImport::RustExternAlias { .. } | RawImport::CSharp(_) | RawImport::Php(_) => {}
                RawImport::RustMod { name, scope, path } => {
                    if let Some(ctx) = &rust {
                        out.extend(self.rust_mod_target(
                            from,
                            &ctx.mod_dir,
                            scope,
                            name,
                            path.as_deref(),
                        ));
                    }
                }
            }
        }
        if matches!(grammar, Grammar::CSharp) {
            let facts: Vec<&CsFact> = imports
                .iter()
                .filter_map(|imp| match imp {
                    RawImport::CSharp(fact) => Some(fact),
                    _ => None,
                })
                .collect();
            out.extend(self.cs.resolve(from, &facts));
        }
        if matches!(grammar, Grammar::Php) {
            let facts: Vec<&PhpFact> = imports
                .iter()
                .filter_map(|imp| match imp {
                    RawImport::Php(fact) => Some(fact),
                    _ => None,
                })
                .collect();
            out.extend(self.php.resolve(from, &facts, &self.files));
        }
        out.remove(from);
        out
    }

    /// Builds the C# symbol table (declared types, project global usings)
    /// from `raw`, the raw imports of every C# file.
    pub(crate) fn prepare_csharp(&mut self, raw: &BTreeMap<String, Vec<RawImport>>) {
        self.cs = CsIndex::build(&self.files, raw, &self.cs_manifests);
    }

    /// Adds the classes, functions and constants declared in `raw`, the raw
    /// imports of every PHP file, to the PHP symbol table.
    pub(crate) fn prepare_php(&mut self, raw: &BTreeMap<String, Vec<RawImport>>) {
        self.php.add_declarations(raw);
    }

    // ------------------------------------------------------------ node / ts

    fn resolve_node(&self, from: &str, spec: &str) -> Option<String> {
        let relative =
            spec == "." || spec == ".." || spec.starts_with("./") || spec.starts_with("../");
        if relative {
            // Bundler queries / fragments (`./x?raw`) name the same file. Not
            // for bare names: `#config` is a subpath-import alias.
            let spec = spec.split(['?', '#']).next().unwrap_or(spec);
            return self.probe_script(&normalize(&join_dir(parent(from), spec))?);
        }
        // A bare specifier is a package unless the owning tsconfig maps it
        // (`compilerOptions.paths`, then `baseUrl`).
        let options = self.ts.for_file(from)?;
        options
            .candidates(spec)
            .iter()
            .find_map(|c| self.probe_script(c))
    }

    /// The file a TS/JS module path names: itself, an emitted-extension
    /// rewrite, an added extension, or a directory `index`.
    fn probe_script(&self, base: &str) -> Option<String> {
        const EXTS: [&str; 9] = [
            ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts",
        ];
        let mut candidates = vec![base.to_string()];
        // ESM-style TypeScript: `./x.js` names the emitted file of `./x.ts`.
        for (js, tss) in [
            (".js", &[".ts", ".tsx"][..]),
            (".jsx", &[".tsx"][..]),
            (".mjs", &[".mts"][..]),
            (".cjs", &[".cts"][..]),
        ] {
            if let Some(stem) = base.strip_suffix(js) {
                candidates.extend(tss.iter().map(|ts| format!("{stem}{ts}")));
            }
        }
        candidates.extend(EXTS.iter().map(|e| format!("{base}{e}")));
        candidates.extend(EXTS.iter().map(|e| join_dir(base, &format!("index{e}"))));
        candidates.into_iter().find(|c| self.has(c))
    }

    // ------------------------------------------------------------ go

    fn resolve_go(&self, import: &str) -> Vec<String> {
        let Some((module, dir)) = self.go_modules.iter().find(|(m, _)| {
            import == m
                || import
                    .strip_prefix(m.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        }) else {
            return Vec::new();
        };
        let rest = import[module.len()..].trim_start_matches('/');
        let pkg_dir = join_dir(dir, rest);
        self.by_dir
            .get(&pkg_dir)
            .into_iter()
            .flatten()
            .filter(|f| f.ends_with(".go") && !f.ends_with("_test.go"))
            .cloned()
            .collect()
    }

    // ------------------------------------------------------------ python

    fn py_module_file(&self, path: &str) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        [
            format!("{path}.py"),
            format!("{path}.pyi"),
            format!("{path}/__init__.py"),
        ]
        .into_iter()
        .find(|c| self.has(c))
    }

    /// Targets of `import module` / `from module import name`: the module file
    /// (or package `__init__.py`) and, when `name` is a submodule, its file.
    fn py_targets(&self, module_path: &str, name: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = self.py_module_file(module_path).into_iter().collect();
        if let Some(name) = name {
            out.extend(self.py_module_file(&join_dir(module_path, name)));
        }
        out
    }

    /// The first ancestor directory of `file` that is not a package (has no
    /// `__init__.py`): where `sys.path` starts when the file runs as a script
    /// or is collected by pytest (`rootdir` prepend).
    fn py_basedir<'a>(&self, file: &'a str) -> &'a str {
        let mut dir = parent(file);
        while !dir.is_empty() && self.has(&join_dir(dir, "__init__.py")) {
            dir = parent(dir);
        }
        dir
    }

    fn resolve_python(&self, from: &str, module: &str, name: Option<&str>) -> Vec<String> {
        let level = module.chars().take_while(|c| *c == '.').count();
        let rest = module[level..].replace('.', "/");
        if level > 0 {
            let mut dir = parent(from).to_string();
            for _ in 1..level {
                if dir.is_empty() {
                    return Vec::new();
                }
                dir = parent(&dir).to_string();
            }
            if rest.is_empty() {
                // `from . import x`: the package itself, then its submodule `x`.
                let mut out: Vec<String> = Some(join_dir(&dir, "__init__.py"))
                    .filter(|p| self.has(p))
                    .into_iter()
                    .collect();
                if let Some(name) = name {
                    out.extend(self.py_module_file(&join_dir(&dir, name)));
                }
                return out;
            }
            return self.py_targets(&join_dir(&dir, &rest), name);
        }
        // Absolute: the search path importlib would see, never the package
        // directories around the importer (that would let `rich/abc.py`
        // shadow the stdlib `abc`). The basedir, the repo root, `src/`, then
        // the roots configured in `pyproject.toml`.
        let mut roots: Vec<&str> = vec![self.py_basedir(from), "", "src"];
        for root in &self.py_roots {
            roots.push(root);
        }
        let mut seen = BTreeSet::new();
        roots
            .into_iter()
            .filter(|r| seen.insert(*r))
            .map(|root| self.py_targets(&join_dir(root, &rest), name))
            .find(|t| !t.is_empty())
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ rust

    /// Builds the module tree of every Rust target from its root file,
    /// following `mod` declarations (`#[path]` included) through `raw`
    /// (the raw imports of every Rust file). Lib targets come first, so a
    /// file declared by both a lib and a bin keeps its lib context.
    pub(crate) fn prepare_rust(&mut self, raw: &BTreeMap<String, Vec<RawImport>>) {
        let mut roots: Vec<(String, usize)> = Vec::new();
        for (i, c) in self.crates.iter().enumerate() {
            roots.extend(c.lib_root.iter().map(|r| (r.clone(), i)));
        }
        for (i, c) in self.crates.iter().enumerate() {
            roots.extend(c.other_roots.iter().map(|r| (r.clone(), i)));
        }
        let mut trees = Vec::new();
        let mut tree_of = BTreeMap::new();
        let mut contexts: BTreeMap<String, RustContext> = BTreeMap::new();
        for (root, krate) in roots {
            if tree_of.contains_key(&root) {
                continue;
            }
            let t = trees.len();
            tree_of.insert(root.clone(), t);
            let mut tree = RustTree::default();
            let root_dir = parent(&root).to_string();
            tree.modules.insert(Vec::new(), root.clone());
            let mut seen = BTreeSet::from([root.clone()]);
            let mut stack = vec![(root.clone(), Vec::<String>::new(), root_dir.clone())];
            while let Some((file, module_path, mod_dir)) = stack.pop() {
                contexts.entry(file.clone()).or_insert_with(|| RustContext {
                    tree: Some(t),
                    root_dir: root_dir.clone(),
                    module_path: module_path.clone(),
                    mod_dir: mod_dir.clone(),
                    root_file: Some(root.clone()),
                    krate: Some(krate),
                });
                for imp in raw.get(&file).into_iter().flatten() {
                    if let RawImport::RustExternAlias { krate, alias } = imp {
                        tree.extern_aliases
                            .entry(alias.clone())
                            .or_insert_with(|| krate.clone());
                        continue;
                    }
                    let scope = match imp {
                        RawImport::RustUse { scope, .. } | RawImport::RustMod { scope, .. } => {
                            scope
                        }
                        _ => continue,
                    };
                    for k in 1..=scope.len() {
                        let mut inline = module_path.clone();
                        inline.extend(scope[..k].iter().cloned());
                        tree.modules.entry(inline).or_insert_with(|| file.clone());
                    }
                    let RawImport::RustMod { name, path, .. } = imp else {
                        continue;
                    };
                    let Some(target) =
                        self.rust_mod_target(&file, &mod_dir, scope, name, path.as_deref())
                    else {
                        continue;
                    };
                    let mut child = module_path.clone();
                    child.extend(scope.iter().cloned());
                    child.push(name.clone());
                    tree.modules.entry(child.clone()).or_insert(target.clone());
                    if seen.insert(target.clone()) {
                        // A `#[path]` file or `mod.rs` owns its directory; any
                        // other `x.rs` owns `x/`.
                        let child_dir = match target.strip_suffix(".rs") {
                            Some(stem) if path.is_none() && !target.ends_with("/mod.rs") => {
                                stem.to_string()
                            }
                            _ => parent(&target).to_string(),
                        };
                        stack.push((target, child, child_dir));
                    }
                }
            }
            trees.push(tree);
        }
        self.rust_trees = trees;
        self.tree_of = tree_of;
        self.rust_contexts = contexts;
    }

    /// The file of `mod name;` declared in `from` inside the inline modules
    /// `scope`: `#[path]` (relative to the file's directory, or to the inline
    /// module's directory inside `mod x { .. }`), else `name.rs` / `name/mod.rs`.
    fn rust_mod_target(
        &self,
        from: &str,
        mod_dir: &str,
        scope: &[String],
        name: &str,
        path: Option<&str>,
    ) -> Option<String> {
        let scoped = join_dir(mod_dir, &scope.join("/"));
        if let Some(path) = path {
            let base = if scope.is_empty() {
                parent(from)
            } else {
                &scoped
            };
            let target = normalize(&join_dir(base, &path.replace('\\', "/")))?;
            return self.has(&target).then_some(target);
        }
        let joined = join_dir(&scoped, name);
        [format!("{joined}.rs"), format!("{joined}/mod.rs")]
            .into_iter()
            .find(|c| self.has(c))
    }

    fn rust_context(&self, file: &str) -> RustContext {
        if let Some(ctx) = self.rust_contexts.get(file) {
            return ctx.clone();
        }
        // Not reached from any target root (macro-generated `mod`, an orphan
        // file): model it by location under the deepest target directory of
        // its package, else as a crate root of its own.
        let krate = self
            .crates
            .iter()
            .position(|c| c.dir.is_empty() || file.starts_with(&format!("{}/", c.dir)));
        let standalone = RustContext {
            tree: self.tree_of.get(file).copied(),
            root_dir: parent(file).to_string(),
            module_path: Vec::new(),
            mod_dir: parent(file).to_string(),
            root_file: Some(file.to_string()),
            krate,
        };
        let Some(k) = krate else {
            return standalone;
        };
        let c = &self.crates[k];
        let is_root = |r: &String| r == file;
        if c.lib_root.iter().any(is_root) || c.other_roots.iter().any(is_root) {
            return standalone;
        }
        // The lib root, and other roots that do not sit in the package
        // directory itself (`build.rs` would otherwise own every file).
        let candidates = c
            .lib_root
            .iter()
            .chain(c.other_roots.iter().filter(|r| parent(r) != c.dir.as_str()));
        let best = candidates
            .filter_map(|root| {
                let dir = parent(root);
                let rel = if dir.is_empty() {
                    Some(file)
                } else {
                    file.strip_prefix(&format!("{dir}/"))
                }?;
                Some((dir.len(), root, rel.strip_suffix(".rs")?))
            })
            .max_by_key(|(len, _, _)| *len);
        let Some((_, root, rel)) = best else {
            return standalone;
        };
        let mut segs: Vec<String> = rel.split('/').map(str::to_string).collect();
        if segs.last().is_some_and(|s| s == "mod") {
            segs.pop();
        }
        let mod_dir = match file.strip_suffix(".rs") {
            Some(stem) if !file.ends_with("/mod.rs") => stem.to_string(),
            _ => parent(file).to_string(),
        };
        RustContext {
            tree: self.tree_of.get(root).copied(),
            root_dir: parent(root).to_string(),
            module_path: segs,
            mod_dir,
            root_file: Some(root.clone()),
            krate,
        }
    }

    /// The file of module `path` of a target: its tree first, then by location
    /// under `root_dir` (`a/b.rs` or `a/b/mod.rs`).
    fn rust_module(&self, tree: Option<usize>, root_dir: &str, path: &[String]) -> Option<String> {
        if path.is_empty() {
            return None;
        }
        if let Some(file) = tree.and_then(|t| self.rust_trees[t].modules.get(path)) {
            return Some(file.clone());
        }
        let joined = join_dir(root_dir, &path.join("/"));
        [format!("{joined}.rs"), format!("{joined}/mod.rs")]
            .into_iter()
            .find(|c| self.has(c))
    }

    /// The lib target `name` names from a file of package `krate`: a path
    /// dependency (renames included), the package's own lib (seen by its bins,
    /// tests, examples and benches), else any repository lib of that name.
    /// Returns `(tree, root dir, root file)`.
    fn rust_extern(
        &self,
        krate: Option<usize>,
        name: &str,
    ) -> Option<(Option<usize>, String, String)> {
        let lib = |c: &RustCrate| {
            let root = c.lib_root.clone()?;
            Some((
                self.tree_of.get(&root).copied(),
                parent(&root).to_string(),
                root,
            ))
        };
        if let Some(own) = krate.map(|k| &self.crates[k]) {
            if let Some(dir) = own.path_deps.get(name) {
                if let Some(found) = self.crates.iter().find(|c| &c.dir == dir).and_then(lib) {
                    return Some(found);
                }
            }
            if own.lib_name == name {
                if let Some(found) = lib(own) {
                    return Some(found);
                }
            }
        }
        self.crates
            .iter()
            .filter(|c| c.lib_name == name)
            .find_map(lib)
    }

    fn resolve_rust_use(
        &self,
        from: &str,
        ctx: &RustContext,
        segments: &[String],
        scope: &[String],
    ) -> Option<String> {
        let mut current = ctx.module_path.clone();
        current.extend(scope.iter().cloned());
        let (first, rest) = segments.split_first()?;
        let own = (ctx.tree, ctx.root_dir.clone(), ctx.root_file.clone());
        // (tree, root dir, root file, absolute module path, own crate?)
        let ((tree, root_dir, root_file), mut abs, is_own) = match first.as_str() {
            "crate" => (own, rest.to_vec(), true),
            "self" => {
                let mut abs = current.clone();
                abs.extend(rest.iter().cloned());
                (own, abs, true)
            }
            "super" => {
                let mut abs = current.clone();
                abs.pop();
                let mut tail = rest;
                while let Some((s, t)) = tail.split_first() {
                    if s != "super" {
                        break;
                    }
                    abs.pop();
                    tail = t;
                }
                abs.extend(tail.iter().cloned());
                (own, abs, true)
            }
            name => {
                // A child module in scope (2018 uniform paths), else an extern crate.
                let mut local = current.clone();
                local.push(name.to_string());
                if self.rust_module(ctx.tree, &ctx.root_dir, &local).is_some() {
                    local.extend(rest.iter().cloned());
                    (own, local, true)
                } else {
                    let name = ctx
                        .tree
                        .and_then(|t| self.rust_trees.get(t))
                        .and_then(|t| t.extern_aliases.get(name))
                        .map_or(name, String::as_str);
                    let (tree, dir, root) = self.rust_extern(ctx.krate, name)?;
                    ((tree, dir, Some(root)), rest.to_vec(), false)
                }
            }
        };
        if abs.last().is_some_and(|s| s == "self") {
            abs.pop();
        }
        // Longest prefix that is a module file; the rest are items inside it.
        for k in (1..=abs.len()).rev() {
            if let Some(file) = self.rust_module(tree, &root_dir, &abs[..k]) {
                return Some(file);
            }
        }
        if is_own && abs.starts_with(&ctx.module_path) && !ctx.module_path.is_empty() {
            // An item of this very file (or of an inline module in it).
            return Some(from.to_string());
        }
        root_file
    }
}

// ------------------------------------------------------------ manifests

fn go_module_path(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("module")?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let module = rest.split("//").next()?.trim().trim_matches('"');
        (!module.is_empty()).then(|| module.to_string())
    })
}

/// Python search roots a `pyproject.toml` in `dir` configures: the project
/// directory itself (when not the repo root, which is always searched),
/// pytest `pythonpath`, setuptools `package-dir` (`""` key) and
/// `packages.find.where`, and poetry `packages[].from`. Invalid TOML yields
/// none.
fn pyproject_roots(dir: &str, text: &str) -> Vec<String> {
    use toml::Value;
    let Ok(table) = text.parse::<toml::Table>() else {
        return Vec::new();
    };
    let get = |path: &[&str]| -> Option<&Value> {
        let mut v = table.get(path[0])?;
        for key in &path[1..] {
            v = v.get(key)?;
        }
        Some(v)
    };
    let strings = |v: Option<&Value>| -> Vec<String> {
        match v {
            Some(Value::String(s)) => s.split_whitespace().map(str::to_string).collect(),
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    };
    let mut rel: Vec<String> = Vec::new();
    if !dir.is_empty() {
        rel.extend([".".to_string(), "src".to_string()]);
    }
    rel.extend(strings(get(&[
        "tool",
        "pytest",
        "ini_options",
        "pythonpath",
    ])));
    if let Some(root) = get(&["tool", "setuptools", "package-dir", ""]).and_then(Value::as_str) {
        rel.push(root.to_string());
    }
    rel.extend(strings(get(&[
        "tool",
        "setuptools",
        "packages",
        "find",
        "where",
    ])));
    if let Some(packages) = get(&["tool", "poetry", "packages"]).and_then(Value::as_array) {
        rel.extend(
            packages
                .iter()
                .filter_map(|p| p.get("from")?.as_str())
                .map(str::to_string),
        );
    }
    rel.iter()
        .filter_map(|r| normalize(&join_dir(dir, r)))
        .collect()
}

// ------------------------------------------------------------ paths

pub(crate) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

pub(crate) fn join_dir(dir: &str, rel: &str) -> String {
    match (dir.is_empty(), rel.is_empty()) {
        (true, _) => rel.to_string(),
        (false, true) => dir.to_string(),
        (false, false) => format!("{dir}/{rel}"),
    }
}

/// Removes `.` and `..` segments; `None` when the path escapes the repository.
pub(crate) fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(files: &[(&str, &str)]) -> Repo {
        let map: BTreeMap<String, String> = files
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
            .collect();
        Repo::new(map.keys().cloned().collect(), &|p| map.get(p).cloned())
    }

    #[test]
    fn path_helpers() {
        assert_eq!(parent("a/b/c.ts"), "a/b");
        assert_eq!(parent("c.ts"), "");
        assert_eq!(file_name("a/go.mod"), "go.mod");
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(normalize("../x"), None);
        assert_eq!(join_dir("", "x"), "x");
        assert_eq!(join_dir("a", ""), "a");
    }

    #[test]
    fn manifests_are_parsed() {
        assert_eq!(
            go_module_path("// c\nmodule example.com/x // trailing\n\ngo 1.22").as_deref(),
            Some("example.com/x")
        );
        assert_eq!(go_module_path("modules x"), None);
        assert_eq!(
            pyproject_roots(
                "py",
                "[tool.pytest.ini_options]\npythonpath = \"lib ../shared\"\n\
                 [tool.poetry]\npackages = [{ include = \"x\", from = \"code\" }]\n"
            ),
            vec!["py", "py/src", "py/lib", "shared", "py/code"]
        );
        assert!(
            pyproject_roots("", "[tool.pytest.ini_options]\npythonpath = [\"../..\"]\n").is_empty()
        );
        assert!(pyproject_roots("", "not toml [").is_empty());
    }

    #[test]
    fn node_specifiers_escaping_the_repo_or_bare_are_dropped() {
        let r = repo(&[("a.ts", ""), ("x/b.ts", "")]);
        assert_eq!(r.resolve_node("x/b.ts", "../../a"), None);
        assert_eq!(r.resolve_node("x/b.ts", "react"), None);
        assert_eq!(
            r.resolve_node("x/b.ts", "../a?raw").as_deref(),
            Some("a.ts")
        );
    }

    #[test]
    fn python_relative_imports_above_the_root_are_dropped() {
        let r = repo(&[("m.py", ""), ("__init__.py", "")]);
        assert!(r.resolve_python("m.py", "...x", None).is_empty());
        assert_eq!(r.resolve_python("m.py", ".", None), vec!["__init__.py"]);
    }

    #[test]
    fn python_absolute_imports_search_the_basedir_root_and_src() {
        let r = repo(&[
            ("src/pkg/__init__.py", ""),
            ("src/pkg/mod.py", ""),
            ("scripts/tool/helper.py", ""),
            ("scripts/tool/run.py", ""),
            ("lib/__init__.py", ""),
            ("lib/inner/__init__.py", ""),
            ("lib/inner/helper.py", ""),
            ("lib/inner/x.py", ""),
        ]);
        assert_eq!(
            r.resolve_python("x.py", "pkg.mod", None),
            vec!["src/pkg/mod.py"]
        );
        assert_eq!(
            r.resolve_python("scripts/tool/run.py", "helper", None),
            vec!["scripts/tool/helper.py"]
        );
        assert_eq!(r.py_basedir("lib/inner/x.py"), "");
        assert!(
            r.resolve_python("lib/inner/x.py", "helper", None)
                .is_empty()
        );
    }

    #[test]
    fn go_picks_the_longest_module_and_skips_unknown_paths() {
        let r = repo(&[
            ("go.mod", "module ex.com/a"),
            ("sub/go.mod", "module ex.com/a/sub"),
            ("sub/p/p.go", ""),
            ("p/p.go", ""),
        ]);
        assert_eq!(r.resolve_go("ex.com/a/sub/p"), vec!["sub/p/p.go"]);
        assert_eq!(r.resolve_go("ex.com/a/p"), vec!["p/p.go"]);
        assert!(r.resolve_go("ex.com/ab/p").is_empty());
        assert!(r.resolve_go("fmt").is_empty());
    }

    #[test]
    fn rust_items_of_the_current_file_do_not_point_at_the_root() {
        let r = repo(&[
            ("Cargo.toml", "[package]\nname = \"k\"\n"),
            ("src/lib.rs", ""),
            ("src/a.rs", ""),
        ]);
        let ctx = r.rust_context("src/a.rs");
        let seg = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            r.resolve_rust_use("src/a.rs", &ctx, &seg(&["self", "Item"]), &seg(&["tests"]))
                .as_deref(),
            Some("src/a.rs")
        );
        assert_eq!(
            r.resolve_rust_use("src/a.rs", &ctx, &seg(&["crate", "Root"]), &[])
                .as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            r.resolve_rust_use("src/a.rs", &ctx, &seg(&["serde", "Serialize"]), &[]),
            None
        );
    }

    #[test]
    fn rust_orphan_files_fall_back_to_their_location() {
        let r = repo(&[
            (
                "Cargo.toml",
                "[package]\nname = \"k\"\n[[bin]]\nname = \"b\"\npath = \"tool/main.rs\"\n",
            ),
            ("build.rs", ""),
            ("src/lib.rs", ""),
            ("src/gen/mod.rs", ""),
            ("tool/main.rs", ""),
            ("tool/cmd.rs", ""),
            ("other/x.rs", ""),
        ]);
        let generated = r.rust_context("src/gen/mod.rs");
        assert_eq!(generated.module_path, vec!["gen".to_string()]);
        assert_eq!(generated.root_file.as_deref(), Some("src/lib.rs"));
        let cmd = r.rust_context("tool/cmd.rs");
        assert_eq!(cmd.root_file.as_deref(), Some("tool/main.rs"));
        assert_eq!(cmd.mod_dir, "tool/cmd");
        // `build.rs` sits in the package directory and owns nothing else.
        assert_eq!(
            r.rust_context("other/x.rs").root_file.as_deref(),
            Some("other/x.rs")
        );
        assert_eq!(r.rust_context("build.rs").module_path, Vec::<String>::new());
    }
}
