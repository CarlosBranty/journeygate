//! PHP plugin: facts extracted from the syntax tree (class-like, function and
//! constant declarations, `use` imports, class-name references, static
//! includes) and their resolution through Composer autoload maps and a
//! repository-wide symbol table, following PHP's name resolution rules.
//!
//! File A depends on file B when A names a class, interface, trait or enum
//! declared in B, imports a function or constant declared in B, or statically
//! includes B.

use crate::extract::RawImport;
use crate::resolve::{join_dir, normalize, parent};
use std::collections::{BTreeMap, BTreeSet};
use tree_sitter::Node;

/// One PHP fact of a source file. Names are fully qualified without the
/// leading `\` (`App\Models\User`), except [`PhpRef::name`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PhpFact {
    /// A class, interface, trait or enum declaration.
    Class(String),
    /// A function declaration.
    Function(String),
    /// A namespace-level `const X = ...;` (not a class constant).
    Const(String),
    Use(PhpUse),
    Ref(PhpRef),
    /// A static include: the path relative to the including file's directory
    /// (`__DIR__ . '/x.php'` and `'x.php'` both give `x.php`).
    Include(String),
}

/// What a `use` import names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UseKind {
    Class,
    Function,
    Const,
}

/// One imported name of a `use` declaration (a group yields one per member).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhpUse {
    pub(crate) kind: UseKind,
    /// Fully qualified target: `App\Models\User`.
    pub(crate) name: String,
    /// The alias, or the last segment of `name` without `as`.
    pub(crate) alias: String,
    /// Namespace region the import applies to (see [`PhpRef::region`]).
    pub(crate) region: usize,
}

/// A class name as written in a class position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PhpRef {
    /// As written: `\A\B` (fully qualified), `A\B` (qualified), `B`.
    pub(crate) name: String,
    /// Namespace around the reference (`""` = global).
    pub(crate) namespace: String,
    /// Identifies the namespace declaration the reference sits under (its
    /// start byte + 1; 0 = none). `use` imports apply within one region.
    pub(crate) region: usize,
}

/// Turns the captures of one query match into facts (`@declare`,
/// `@function`, `@const`, `@use`, `@class`, `@include`; see
/// `languages/php/imports.scm`).
pub(crate) fn collect(caps: &[(&str, Node)], bytes: &[u8], out: &mut Vec<RawImport>) {
    for (capture, node) in caps {
        let fact = |f: PhpFact| RawImport::Php(f);
        match *capture {
            "declare" | "function" => {
                let full = qualify_in(&namespace_of(*node, bytes).0, text(*node, bytes));
                out.push(fact(match *capture {
                    "declare" => PhpFact::Class(full),
                    _ => PhpFact::Function(full),
                }));
            }
            "const" => {
                // name -> const_element -> const_declaration -> its container.
                let container = node
                    .parent()
                    .and_then(|e| e.parent())
                    .and_then(|d| d.parent());
                let in_class = container.is_some_and(|c| {
                    matches!(c.kind(), "declaration_list" | "enum_declaration_list")
                });
                if !in_class {
                    let full = qualify_in(&namespace_of(*node, bytes).0, text(*node, bytes));
                    out.push(fact(PhpFact::Const(full)));
                }
            }
            "use" => out.extend(use_imports(*node, bytes).into_iter().map(fact)),
            "class" => {
                let name = compact(text(*node, bytes));
                let lower = name.to_ascii_lowercase();
                if matches!(lower.as_str(), "self" | "static" | "parent") {
                    continue;
                }
                let (namespace, region) = namespace_of(*node, bytes);
                out.push(fact(PhpFact::Ref(PhpRef {
                    name,
                    namespace,
                    region,
                })));
            }
            "include" => {
                if let Some(path) = node.named_child(0).and_then(|arg| include_path(arg, bytes)) {
                    out.push(fact(PhpFact::Include(path)));
                }
            }
            _ => {}
        }
    }
}

fn text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("")
}

/// A name without whitespace or comments between its segments.
fn compact(name: &str) -> String {
    name.split_whitespace().collect()
}

/// `namespace\name`, or `name` in the global namespace.
fn qualify_in(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!(r"{namespace}\{name}")
    }
}

/// The namespace around `node` and its region id. A braced
/// `namespace A { ... }` is an ancestor; an unbraced `namespace A;` is a
/// sibling of the statements after it, up to the next namespace declaration.
fn namespace_of(node: Node, bytes: &[u8]) -> (String, usize) {
    let mut current = node.parent();
    while let Some(n) = current {
        match n.kind() {
            "namespace_definition" => return namespace_name(n, bytes),
            "program" => {
                let mut cursor = n.walk();
                let decl = n
                    .named_children(&mut cursor)
                    .filter(|c| c.kind() == "namespace_definition")
                    .filter(|c| c.child_by_field_name("body").is_none())
                    .take_while(|c| c.start_byte() < node.start_byte())
                    .last();
                return decl.map(|d| namespace_name(d, bytes)).unwrap_or_default();
            }
            _ => current = n.parent(),
        }
    }
    (String::new(), 0)
}

fn namespace_name(decl: Node, bytes: &[u8]) -> (String, usize) {
    let name = decl
        .child_by_field_name("name")
        .map(|n| compact(text(n, bytes)))
        .unwrap_or_default();
    (name, decl.start_byte() + 1)
}

/// The kind a `type:` field (`function` / `const`) gives, if any.
fn use_kind(node: Node) -> Option<UseKind> {
    node.child_by_field_name("type").map(|t| match t.kind() {
        "function" => UseKind::Function,
        "const" => UseKind::Const,
        _ => UseKind::Class,
    })
}

/// Every imported name of a `namespace_use_declaration`.
fn use_imports(decl: Node, bytes: &[u8]) -> Vec<PhpFact> {
    let region = namespace_of(decl, bytes).1;
    let outer_kind = use_kind(decl).unwrap_or(UseKind::Class);
    let mut cursor = decl.walk();
    let children: Vec<Node> = decl.named_children(&mut cursor).collect();
    // `use A\B\{C, D as E};`: the group prefix is the namespace_name before the body.
    let (prefix, clauses) = match decl.child_by_field_name("body") {
        Some(group) => {
            let prefix = children
                .iter()
                .find(|c| c.kind() == "namespace_name")
                .map(|p| compact(text(*p, bytes)))
                .unwrap_or_default();
            let mut inner = group.walk();
            let clauses: Vec<Node> = group
                .named_children(&mut inner)
                .filter(|c| c.kind() == "namespace_use_clause")
                .collect();
            (Some(prefix), clauses)
        }
        None => (
            None,
            children
                .into_iter()
                .filter(|c| c.kind() == "namespace_use_clause")
                .collect(),
        ),
    };
    let mut out = Vec::new();
    for clause in clauses {
        let alias = clause.child_by_field_name("alias");
        let mut inner = clause.walk();
        let Some(target) = clause
            .named_children(&mut inner)
            .find(|c| matches!(c.kind(), "name" | "qualified_name") && Some(*c) != alias)
        else {
            continue;
        };
        let written = compact(text(target, bytes));
        let name = match &prefix {
            Some(p) => format!(r"{}\{written}", p.trim_matches('\\')),
            None => written.trim_start_matches('\\').to_string(),
        };
        let alias = match alias {
            Some(a) => text(a, bytes).to_string(),
            None => name.rsplit('\\').next().unwrap_or(&name).to_string(),
        };
        out.push(PhpFact::Use(PhpUse {
            kind: use_kind(clause).unwrap_or(outer_kind),
            name,
            alias,
            region,
        }));
    }
    out
}

/// The path of a static include relative to the including file's directory:
/// a string literal, or `__DIR__` / `dirname(__FILE__)` /
/// `dirname(__DIR__[, levels])` concatenated with a literal starting with `/`.
fn include_path(arg: Node, bytes: &[u8]) -> Option<String> {
    match arg.kind() {
        "parenthesized_expression" => include_path(arg.named_child(0)?, bytes),
        "string" | "encapsed_string" => {
            let lit = literal(arg, bytes)?;
            // An absolute path or a stream wrapper (`phar://`) is not a repository path.
            (!lit.is_empty() && !lit.starts_with('/') && !lit.contains(':')).then_some(lit)
        }
        "binary_expression" => {
            let op = arg.child_by_field_name("operator")?;
            if text(op, bytes) != "." {
                return None;
            }
            let up = dir_levels(arg.child_by_field_name("left")?, bytes)?;
            let lit = literal(arg.child_by_field_name("right")?, bytes)?;
            let rest = lit.strip_prefix('/')?;
            Some(format!("{}{rest}", "../".repeat(up)))
        }
        _ => None,
    }
}

/// The text of a string literal without interpolation.
fn literal(node: Node, bytes: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "encapsed_string") {
        return None;
    }
    let mut cursor = node.walk();
    let mut out = String::new();
    for c in node.named_children(&mut cursor) {
        match c.kind() {
            "string_content" => out.push_str(text(c, bytes)),
            _ => return None,
        }
    }
    Some(out)
}

/// How many directories above the file's own directory an expression names:
/// `__DIR__` and `dirname(__FILE__)` are 0, `dirname(__DIR__)` is 1,
/// `dirname(__DIR__, 2)` is 2.
fn dir_levels(node: Node, bytes: &[u8]) -> Option<usize> {
    match node.kind() {
        "name" if text(node, bytes) == "__DIR__" => Some(0),
        "function_call_expression" => {
            let function = node.child_by_field_name("function")?;
            if !text(function, bytes).eq_ignore_ascii_case("dirname") {
                return None;
            }
            let args = node.child_by_field_name("arguments")?;
            let mut cursor = args.walk();
            let values: Vec<Node> = args
                .named_children(&mut cursor)
                .filter(|a| a.kind() == "argument")
                .filter_map(|a| a.named_child(0))
                .collect();
            let levels = match values.get(1) {
                None => 1,
                Some(n) if n.kind() == "integer" => text(*n, bytes).parse().ok()?,
                Some(_) => return None,
            };
            let base = *values.first()?;
            let inner = match (base.kind(), text(base, bytes)) {
                // `dirname(__FILE__)` is the file's own directory.
                ("name", "__FILE__") => -1,
                _ => dir_levels(base, bytes)? as isize,
            };
            usize::try_from(inner + levels as isize).ok()
        }
        _ => None,
    }
}

// ------------------------------------------------------------ resolution

/// Composer autoload maps, vendor directories and a repository-wide symbol
/// table of declared classes, functions and constants.
#[derive(Debug, Default)]
pub(crate) struct PhpIndex {
    /// PSR-4 `(namespace prefix, directory)`, longest prefix first.
    psr4: Vec<(String, String)>,
    /// PSR-0 `(prefix, directory)`, longest prefix first.
    psr0: Vec<(String, String)>,
    /// Composer vendor directories (third-party code): never targets.
    vendor_dirs: Vec<String>,
    /// Lowercase fully qualified class name -> declaring files.
    classes: BTreeMap<String, BTreeSet<String>>,
    /// Lowercase fully qualified function name -> declaring files.
    functions: BTreeMap<String, BTreeSet<String>>,
    /// Constant name (lowercase namespace, name as written) -> declaring files.
    consts: BTreeMap<String, BTreeSet<String>>,
}

impl PhpIndex {
    /// Reads `autoload` / `autoload-dev` PSR-4 and PSR-0 maps of every
    /// `composer.json` (`(path, contents)`) outside a vendor directory.
    /// Directories are relative to the manifest's own directory.
    pub(crate) fn load(manifests: &[(String, String)]) -> Self {
        let mut manifests: Vec<&(String, String)> = manifests.iter().collect();
        manifests.sort_by_key(|(path, _)| (path.matches('/').count(), path.clone()));
        let mut index = PhpIndex::default();
        if !manifests.iter().any(|(path, _)| path == "composer.json") {
            index.vendor_dirs.push("vendor".to_string());
        }
        for (path, text) in manifests {
            if index.is_vendor(path) {
                continue;
            }
            let dir = parent(path);
            let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
                continue;
            };
            let vendor_dir = json
                .pointer("/config/vendor-dir")
                .and_then(|v| v.as_str())
                .unwrap_or("vendor");
            if let Some(v) = normalize(&join_dir(dir, vendor_dir)).filter(|v| !v.is_empty()) {
                index.vendor_dirs.push(v);
            }
            for section in ["autoload", "autoload-dev"] {
                for (standard, out) in [("psr-4", &mut index.psr4), ("psr-0", &mut index.psr0)] {
                    let Some(map) = json
                        .get(section)
                        .and_then(|s| s.get(standard))
                        .and_then(|m| m.as_object())
                    else {
                        continue;
                    };
                    for (prefix, dirs) in map {
                        let mut prefix = prefix.clone();
                        if standard == "psr-4" && !prefix.is_empty() && !prefix.ends_with('\\') {
                            prefix.push('\\');
                        }
                        let dirs: Vec<&str> = match dirs {
                            serde_json::Value::String(d) => vec![d.as_str()],
                            serde_json::Value::Array(ds) => {
                                ds.iter().filter_map(|d| d.as_str()).collect()
                            }
                            _ => Vec::new(),
                        };
                        for d in dirs {
                            if let Some(d) = normalize(&join_dir(dir, d)) {
                                out.push((prefix.clone(), d));
                            }
                        }
                    }
                }
            }
        }
        // Stable: equal prefixes keep manifest and declaration order.
        index.psr4.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        index.psr0.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        index
    }

    /// Adds the declarations of `raw`, the raw imports of every PHP file.
    /// Files in a vendor directory declare nothing.
    pub(crate) fn add_declarations(&mut self, raw: &BTreeMap<String, Vec<RawImport>>) {
        for (file, imports) in raw {
            if self.is_vendor(file) {
                continue;
            }
            for imp in imports {
                let (table, key) = match imp {
                    RawImport::Php(PhpFact::Class(n)) => (&mut self.classes, n.to_lowercase()),
                    RawImport::Php(PhpFact::Function(n)) => (&mut self.functions, n.to_lowercase()),
                    RawImport::Php(PhpFact::Const(n)) => (&mut self.consts, const_key(n)),
                    _ => continue,
                };
                table.entry(key).or_default().insert(file.clone());
            }
        }
    }

    /// Whether `path` lies in a Composer vendor directory.
    fn is_vendor(&self, path: &str) -> bool {
        self.vendor_dirs.iter().any(|v| {
            path.strip_prefix(v.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
    }

    /// Files `facts` (the facts of `from`) depend on.
    pub(crate) fn resolve(
        &self,
        from: &str,
        facts: &[&PhpFact],
        files: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        let mut uses: BTreeMap<usize, Vec<&PhpUse>> = BTreeMap::new();
        for fact in facts {
            if let PhpFact::Use(u) = fact {
                if u.kind == UseKind::Class {
                    uses.entry(u.region).or_default().push(u);
                }
            }
        }
        let mut out = BTreeSet::new();
        for fact in facts {
            match fact {
                PhpFact::Use(u) => out.extend(match u.kind {
                    UseKind::Class => self.class_files(&u.name, files),
                    UseKind::Function => self
                        .functions
                        .get(&u.name.to_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                    UseKind::Const => self
                        .consts
                        .get(&const_key(&u.name))
                        .cloned()
                        .unwrap_or_default(),
                }),
                PhpFact::Ref(r) => {
                    let region = uses.get(&r.region).map(Vec::as_slice).unwrap_or(&[]);
                    out.extend(self.class_files(&qualify(r, region), files));
                }
                PhpFact::Include(path) => {
                    if let Some(p) = normalize(&join_dir(parent(from), path)) {
                        if files.contains(&p) {
                            out.insert(p);
                        }
                    }
                }
                PhpFact::Class(_) | PhpFact::Function(_) | PhpFact::Const(_) => {}
            }
        }
        out.retain(|f| !self.is_vendor(f));
        out
    }

    /// The file the autoloader loads for the fully qualified class `fq`:
    /// PSR-4 (longest prefix first, every directory of a prefix in order),
    /// then PSR-0; else every file declaring it (classmap and non-PSR code).
    fn class_files(&self, fq: &str, files: &BTreeSet<String>) -> BTreeSet<String> {
        let psr4 = self.psr4.iter().filter_map(|(prefix, dir)| {
            let rest = fq.strip_prefix(prefix.as_str())?;
            Some(join_dir(dir, &format!("{}.php", rest.replace('\\', "/"))))
        });
        let psr0 = self.psr0.iter().filter_map(|(prefix, dir)| {
            fq.strip_prefix(prefix.as_str())?;
            let (namespace, class) = fq.rsplit_once('\\').unwrap_or(("", fq));
            let rel = join_dir(
                &namespace.replace('\\', "/"),
                &format!("{}.php", class.replace('_', "/")),
            );
            Some(join_dir(dir, &rel))
        });
        if let Some(hit) = psr4
            .chain(psr0)
            .find(|p| files.contains(p) && !self.is_vendor(p))
        {
            return BTreeSet::from([hit]);
        }
        self.classes
            .get(&fq.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }
}

/// Constants: the namespace is case-insensitive, the name is not.
fn const_key(full: &str) -> String {
    match full.rsplit_once('\\') {
        Some((ns, name)) => format!(r"{}\{name}", ns.to_lowercase()),
        None => full.to_string(),
    }
}

/// The fully qualified class name `r` means, by PHP's rules: `\A\B` is
/// absolute; `namespace\A` is relative to the current namespace; otherwise
/// the first segment is replaced by a class `use` import whose alias matches
/// it (case-insensitively), else the current namespace is prepended. There is
/// no global fallback for class names.
fn qualify(r: &PhpRef, uses: &[&PhpUse]) -> String {
    let name = r.name.as_str();
    if let Some(abs) = name.strip_prefix('\\') {
        return abs.to_string();
    }
    if let Some(rest) = name
        .get(..10)
        .filter(|h| h.eq_ignore_ascii_case(r"namespace\"))
        .map(|_| &name[10..])
    {
        return qualify_in(&r.namespace, rest);
    }
    let (first, rest) = match name.split_once('\\') {
        Some((f, rest)) => (f, Some(rest)),
        None => (name, None),
    };
    if let Some(u) = uses
        .iter()
        .rev()
        .find(|u| u.alias.eq_ignore_ascii_case(first))
    {
        return match rest {
            Some(rest) => format!(r"{}\{rest}", u.name),
            None => u.name.clone(),
        };
    }
    qualify_in(&r.namespace, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Extractor;
    use crate::lang::Grammar;

    fn facts(src: &str) -> Vec<PhpFact> {
        Extractor::new([Grammar::Php])
            .unwrap()
            .extract(Grammar::Php, src)
            .into_iter()
            .filter_map(|r| match r {
                RawImport::Php(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    fn refs(src: &str) -> Vec<String> {
        let mut out: Vec<String> = facts(src)
            .into_iter()
            .filter_map(|f| match f {
                PhpFact::Ref(r) => Some(format!("{}|{}", r.namespace, r.name)),
                _ => None,
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    fn manifests(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect()
    }

    #[test]
    fn composer_maps_are_relative_to_the_manifest_and_sorted_longest_prefix_first() {
        let idx = PhpIndex::load(&manifests(&[
            (
                "composer.json",
                r#"{"autoload": {"psr-4": {"App": "app/", "App\\Core\\": ["core", "../out"]},
                    "psr-0": {"": "legacy/"}},
                    "config": {"vendor-dir": "deps"}}"#,
            ),
            (
                "pkg/a/composer.json",
                r#"{"autoload-dev": {"psr-4": {"A\\": "./src"}}}"#,
            ),
            ("pkg/b/composer.json", "{ not json"),
        ]));
        let pairs = |v: &[(String, String)]| {
            v.iter()
                .map(|(p, d)| format!("{p}={d}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            pairs(&idx.psr4),
            vec![r"App\Core\=core", r"App\=app", r"A\=pkg/a/src"],
            "a missing trailing `\\` is added; `../out` escapes the repo and is dropped"
        );
        assert_eq!(pairs(&idx.psr0), vec!["=legacy"]);
        assert_eq!(
            idx.vendor_dirs,
            vec!["deps", "pkg/a/vendor"],
            "config.vendor-dir replaces `vendor`; an unparseable manifest adds nothing"
        );
        assert!(idx.is_vendor("deps/x/y.php") && !idx.is_vendor("depsx/y.php"));
    }

    #[test]
    fn without_a_root_composer_json_the_root_vendor_directory_is_third_party() {
        let idx = PhpIndex::load(&manifests(&[(
            "vendor/acme/composer.json",
            r#"{"autoload": {"psr-4": {"Acme\\": "src/"}}}"#,
        )]));
        assert!(idx.psr4.is_empty(), "a vendored manifest maps nothing");
        assert_eq!(idx.vendor_dirs, vec!["vendor"]);
    }

    #[test]
    fn declarations_are_fully_qualified_and_class_constants_are_not_constants() {
        let got = facts(
            r#"<?php
namespace App\Models;
class User { const ROLE = 'x'; }
interface HasName {}
trait Named {}
enum Status: string { case On = 'on'; }
function helper() {}
const LIMIT = 10;
"#,
        );
        let decls: Vec<&PhpFact> = got
            .iter()
            .filter(|f| {
                matches!(
                    f,
                    PhpFact::Class(_) | PhpFact::Function(_) | PhpFact::Const(_)
                )
            })
            .collect();
        assert_eq!(
            decls,
            vec![
                &PhpFact::Class(r"App\Models\User".into()),
                &PhpFact::Class(r"App\Models\HasName".into()),
                &PhpFact::Class(r"App\Models\Named".into()),
                &PhpFact::Class(r"App\Models\Status".into()),
                &PhpFact::Function(r"App\Models\helper".into()),
                &PhpFact::Const(r"App\Models\LIMIT".into()),
            ],
            "`ROLE` is a class constant, not a namespace constant"
        );
    }

    #[test]
    fn use_imports_expand_groups_aliases_and_kinds() {
        let uses: Vec<(UseKind, String, String)> = facts(
            r#"<?php
namespace App;
use App\Models\User;
use \App\Models\Post as P;
use App\Models\{Comment, Tag as T};
use function App\Support\helper;
use const App\Support\LIMIT;
use function App\Support\{a, b as c};
use App\Mixed\{Klass, function f, const K};
"#,
        )
        .into_iter()
        .filter_map(|f| match f {
            PhpFact::Use(u) => Some((u.kind, u.name, u.alias)),
            _ => None,
        })
        .collect();
        let class = |n: &str, a: &str| (UseKind::Class, n.to_string(), a.to_string());
        let func = |n: &str, a: &str| (UseKind::Function, n.to_string(), a.to_string());
        let konst = |n: &str, a: &str| (UseKind::Const, n.to_string(), a.to_string());
        assert_eq!(
            uses,
            vec![
                class(r"App\Models\User", "User"),
                class(r"App\Models\Post", "P"),
                class(r"App\Models\Comment", "Comment"),
                class(r"App\Models\Tag", "T"),
                func(r"App\Support\helper", "helper"),
                konst(r"App\Support\LIMIT", "LIMIT"),
                func(r"App\Support\a", "a"),
                func(r"App\Support\b", "c"),
                class(r"App\Mixed\Klass", "Klass"),
                func(r"App\Mixed\f", "f"),
                konst(r"App\Mixed\K", "K"),
            ]
        );
    }

    #[test]
    fn class_positions_are_captured_with_their_namespace() {
        let got = refs(
            r#"<?php
namespace App\Http;
#[Attr\Route('/x')]
final class C extends Base implements \Countable, Contracts\Foo {
    use HasStuff, \App\Traits\Other;
    private ?Typed $p;
    public function show(Param|Other2 $a, int $n): ?Ret {
        new Created();
        Stat::call();
        Prop::$x;
        Konst::NAME;
        Klass::class;
        $a instanceof Inst;
        try {} catch (Caught | \RuntimeException $e) {}
        static::make(); self::X; parent::y(); new static();
        return fn(Arrow $q): ArrowRet => $q;
    }
}
"#,
        );
        let want: Vec<String> = [
            r"Arrow",
            r"ArrowRet",
            r"Attr\Route",
            r"Base",
            r"Caught",
            r"Contracts\Foo",
            r"Created",
            r"HasStuff",
            r"Inst",
            r"Klass",
            r"Konst",
            r"Other2",
            r"Param",
            r"Prop",
            r"Ret",
            r"Stat",
            r"Typed",
            r"\App\Traits\Other",
            r"\Countable",
            r"\RuntimeException",
        ]
        .iter()
        .map(|n| format!(r"App\Http|{n}"))
        .collect();
        assert_eq!(got, want, "`self`, `static` and `parent` name no class");
    }

    #[test]
    fn namespaces_apply_to_the_statements_after_them_or_inside_their_braces() {
        let unbraced =
            refs("<?php\nnew Top();\nnamespace A;\nnew InA();\nnamespace B;\nnew InB();\n");
        assert_eq!(unbraced, vec!["A|InA", "B|InB", "|Top"]);
        let braced = refs("<?php\nnamespace A { new InA(); }\nnamespace { new Global(); }\n");
        assert_eq!(braced, vec!["A|InA", "|Global"]);
        let regions: Vec<usize> =
            facts("<?php\nnamespace A;\nuse X\\Y;\nnew Y();\nnamespace B;\nnew Y();\n")
                .into_iter()
                .filter_map(|f| match f {
                    PhpFact::Use(u) => Some(u.region),
                    PhpFact::Ref(r) => Some(r.region),
                    _ => None,
                })
                .collect();
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0], regions[1], "the use applies in namespace A");
        assert_ne!(regions[1], regions[2], "namespace B starts a new region");
    }

    #[test]
    fn includes_keep_literal_and_dir_relative_paths_only() {
        let got: Vec<String> = facts(
            r#"<?php
require __DIR__ . '/../bootstrap/app.php';
require_once 'lib/x.php';
include("y.php");
include_once dirname(__FILE__) . '/z.php';
require dirname(__DIR__) . '/up.php';
require dirname(__DIR__, 2) . "/up2.php";
require $path;
require __DIR__ . $name;
require '/etc/abs.php';
include "inter{$x}.php";
"#,
        )
        .into_iter()
        .filter_map(|f| match f {
            PhpFact::Include(p) => Some(p),
            _ => None,
        })
        .collect();
        assert_eq!(
            got,
            vec![
                "../bootstrap/app.php",
                "lib/x.php",
                "y.php",
                "z.php",
                "../up.php",
                "../../up2.php"
            ],
            "computed, absolute and interpolated paths are dropped"
        );
    }
}
