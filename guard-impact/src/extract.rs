//! Runs the import query of a language over one source file and returns the
//! raw import facts. Resolution to repository files happens in [`crate::resolve`].

use crate::csharp::{self, CsFact};
use crate::lang::Grammar;
use crate::php::{self, PhpFact};
use guard_core::ports::ImpactError;
use std::collections::BTreeMap;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Parser, Query, QueryCursor};

/// One import as written in the source, before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawImport {
    /// TypeScript / JavaScript module specifier (`./x`, `lodash`).
    Specifier(String),
    /// Go import path, quotes stripped.
    GoPath(String),
    /// Python `import a.b.c`.
    PyModule(String),
    /// Python `from <module> import <name>` (`name` absent for `*`).
    PyFrom {
        module: String,
        name: Option<String>,
    },
    /// Rust `use` path, one per leaf of the use tree. `scope` = names of the
    /// inline `mod x { .. }` blocks around the declaration, outermost first.
    RustUse {
        segments: Vec<String>,
        scope: Vec<String>,
    },
    /// Rust out-of-line `mod name;`; `path` is the value of a `#[path = ".."]`
    /// attribute on the declaration.
    RustMod {
        name: String,
        scope: Vec<String>,
        path: Option<String>,
    },
    /// Rust `extern crate krate as alias;`: `alias` names `krate` in the
    /// extern prelude of the whole target (`use alias::m::X`).
    RustExternAlias { krate: String, alias: String },
    /// C# type declaration, using directive or type-name reference.
    CSharp(CsFact),
    /// PHP declaration, `use` import, class-name reference or include.
    Php(PhpFact),
}

/// Compiled import queries, one per grammar.
pub(crate) struct Extractor {
    queries: BTreeMap<Grammar, Query>,
}

impl Extractor {
    pub(crate) fn new(grammars: impl IntoIterator<Item = Grammar>) -> Result<Self, ImpactError> {
        let mut queries = BTreeMap::new();
        for g in grammars {
            let query = Query::new(&g.ts_language(), g.language().import_query()).map_err(|e| {
                ImpactError::Io(format!(
                    "languages/{}/imports.scm: {e}",
                    g.language().name()
                ))
            })?;
            queries.insert(g, query);
        }
        Ok(Self { queries })
    }

    /// Raw imports of `source`. Unparseable input yields what the error-tolerant
    /// parser recovered; an unknown grammar yields nothing.
    pub(crate) fn extract(&self, grammar: Grammar, source: &str) -> Vec<RawImport> {
        let Some(query) = self.queries.get(&grammar) else {
            return Vec::new();
        };
        let mut parser = Parser::new();
        if parser.set_language(&grammar.ts_language()).is_err() {
            return Vec::new();
        }
        let Some(tree) = parser.parse(source, None) else {
            return Vec::new();
        };
        let bytes = source.as_bytes();
        let names = query.capture_names();
        let mut out = Vec::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(query, tree.root_node(), bytes);
        while let Some(m) = matches.next() {
            let mut caps: Vec<(&str, Node)> = Vec::new();
            for c in m.captures {
                let name = names[c.index as usize];
                if !name.starts_with('_') {
                    caps.push((name, c.node));
                }
            }
            collect(grammar, &caps, bytes, &mut out);
        }
        out
    }
}

fn text<'a>(node: Node, bytes: &'a [u8]) -> &'a str {
    node.utf8_text(bytes).unwrap_or("")
}

fn collect(grammar: Grammar, caps: &[(&str, Node)], bytes: &[u8], out: &mut Vec<RawImport>) {
    match grammar {
        Grammar::TypeScript | Grammar::Tsx | Grammar::JavaScript => {
            for (name, node) in caps {
                if *name == "import" {
                    out.push(RawImport::Specifier(text(*node, bytes).to_string()));
                }
            }
        }
        Grammar::Go => {
            for (name, node) in caps {
                if *name == "import" {
                    let raw = text(*node, bytes).trim_matches(|c| c == '"' || c == '`');
                    out.push(RawImport::GoPath(raw.to_string()));
                }
            }
        }
        Grammar::Python => {
            let from = caps.iter().find(|(n, _)| *n == "from").map(|(_, n)| *n);
            let names: Vec<&str> = caps
                .iter()
                .filter(|(n, _)| *n == "name")
                .map(|(_, node)| text(*node, bytes))
                .collect();
            for (name, node) in caps {
                if *name == "import" {
                    out.push(RawImport::PyModule(text(*node, bytes).to_string()));
                }
            }
            if let Some(from) = from {
                let module: String = text(from, bytes).split_whitespace().collect();
                if names.is_empty() {
                    out.push(RawImport::PyFrom { module, name: None });
                } else {
                    for n in names {
                        out.push(RawImport::PyFrom {
                            module: module.clone(),
                            name: Some(n.to_string()),
                        });
                    }
                }
            }
        }
        Grammar::CSharp => csharp::collect(caps, bytes, out),
        Grammar::Php => php::collect(caps, bytes, out),
        Grammar::Rust => {
            for (name, node) in caps {
                match *name {
                    "use" => {
                        let scope = inline_scope(*node, bytes);
                        let mut paths = Vec::new();
                        expand_use(*node, bytes, &[], &mut paths);
                        for segments in paths {
                            out.push(RawImport::RustUse {
                                segments,
                                scope: scope.clone(),
                            });
                        }
                    }
                    "extern" => {
                        // `extern crate x;` names the crate root of `x`.
                        let item = node.parent().unwrap_or(*node);
                        let krate = text(*node, bytes).to_string();
                        if let Some(alias) = item.child_by_field_name("alias") {
                            let alias = text(alias, bytes);
                            if alias != "_" && alias != krate {
                                out.push(RawImport::RustExternAlias {
                                    krate: krate.clone(),
                                    alias: alias.to_string(),
                                });
                            }
                        }
                        out.push(RawImport::RustUse {
                            segments: vec![krate],
                            scope: inline_scope(item, bytes),
                        });
                    }
                    "mod" => {
                        // The capture is the name; its mod_item is the parent.
                        let item = node.parent().unwrap_or(*node);
                        out.push(RawImport::RustMod {
                            name: text(*node, bytes).to_string(),
                            scope: inline_scope(item, bytes),
                            path: path_attribute(item, bytes),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The value of a `#[path = "..."]` attribute among the outer attributes
/// written directly before `item` (comments and other attributes may sit
/// between them).
fn path_attribute(item: Node, bytes: &[u8]) -> Option<String> {
    let mut current = item.prev_sibling();
    while let Some(n) = current {
        match n.kind() {
            "attribute_item" => {
                let attr = n.named_child(0).filter(|a| a.kind() == "attribute");
                if let Some(attr) = attr {
                    let is_path = attr
                        .named_child(0)
                        .is_some_and(|id| id.kind() == "identifier" && text(id, bytes) == "path");
                    let value = attr
                        .child_by_field_name("value")
                        .filter(|v| v.kind() == "string_literal");
                    if let (true, Some(value)) = (is_path, value) {
                        let raw = text(value, bytes);
                        let inner = raw.strip_prefix('"').and_then(|r| r.strip_suffix('"'));
                        return inner.map(str::to_string);
                    }
                }
            }
            "line_comment" | "block_comment" => {}
            _ => return None,
        }
        current = n.prev_sibling();
    }
    None
}

/// Names of the inline `mod x { .. }` blocks enclosing `node`, outermost first.
fn inline_scope(node: Node, bytes: &[u8]) -> Vec<String> {
    let mut scope = Vec::new();
    let mut current = node.parent();
    while let Some(n) = current {
        if n.kind() == "mod_item" && n.child_by_field_name("body").is_some() {
            if let Some(name) = n.child_by_field_name("name") {
                scope.push(text(name, bytes).to_string());
            }
        }
        current = n.parent();
    }
    scope.reverse();
    scope
}

/// Segments of a path node (`a::b::c`); `None` for paths the analysis cannot
/// follow (generic or bracketed types, macros).
fn path_segments(node: Node, bytes: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" | "crate" | "self" | "super" => Some(vec![text(node, bytes).to_string()]),
        "scoped_identifier" => {
            let mut segs = match node.child_by_field_name("path") {
                Some(p) => path_segments(p, bytes)?,
                None => Vec::new(),
            };
            segs.push(text(node.child_by_field_name("name")?, bytes).to_string());
            Some(segs)
        }
        _ => None,
    }
}

/// Expands a use tree into one segment list per leaf.
fn expand_use(node: Node, bytes: &[u8], prefix: &[String], out: &mut Vec<Vec<String>>) {
    let join = |segs: Vec<String>| {
        let mut all = prefix.to_vec();
        all.extend(segs);
        all
    };
    match node.kind() {
        "identifier" | "crate" | "self" | "super" | "scoped_identifier" => {
            if let Some(segs) = path_segments(node, bytes) {
                out.push(join(segs));
            }
        }
        "use_as_clause" => {
            if let Some(path) = node.child_by_field_name("path") {
                expand_use(path, bytes, prefix, out);
            }
        }
        "use_wildcard" => match node.named_child(0) {
            Some(path) => {
                if let Some(segs) = path_segments(path, bytes) {
                    out.push(join(segs));
                }
            }
            None => out.push(prefix.to_vec()),
        },
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                expand_use(child, bytes, prefix, out);
            }
        }
        "scoped_use_list" => {
            let base = match node.child_by_field_name("path") {
                Some(p) => match path_segments(p, bytes) {
                    Some(segs) => join(segs),
                    None => return,
                },
                None => prefix.to_vec(),
            };
            if let Some(list) = node.child_by_field_name("list") {
                expand_use(list, bytes, &base, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(g: Grammar, src: &str) -> Vec<RawImport> {
        Extractor::new([g]).unwrap().extract(g, src)
    }

    fn specs(g: Grammar, src: &str) -> Vec<String> {
        extract(g, src)
            .into_iter()
            .map(|r| match r {
                RawImport::Specifier(s) => s,
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn typescript_captures_every_import_form() {
        let src = r#"
            import a from "./a";
            import type { B } from './b';
            import "./side";
            import c = require("./c");
            export * from "./d";
            export { e } from "./e";
            const f = require("./f");
            const g = await import("./g");
            const h = notRequire("./h");
            const i = require(dynamicName);
        "#;
        assert_eq!(
            specs(Grammar::TypeScript, src),
            vec!["./a", "./b", "./side", "./c", "./d", "./e", "./f", "./g"]
        );
    }

    #[test]
    fn typescript_captures_relative_module_augmentations_only() {
        let src = r#"
            import { Hono } from "./hono";
            declare module "../.." { interface ContextVariableMap { a: string } }
            declare module "./types" { interface Env {} }
            declare module "pkg" { interface X {} }
            declare module "fs" { interface X {} }
            declare module "ab" { interface X {} }
            declare module "a" { interface X {} }
            declare module "@x/y" { interface X {} }
            declare module "..x" { interface X {} }
            declare global { interface Window {} }
            declare namespace NS { const x: number }
        "#;
        assert_eq!(
            specs(Grammar::TypeScript, src),
            vec!["./hono", "../..", "./types"]
        );
    }

    #[test]
    fn rust_reads_path_attributes_of_mod_declarations() {
        let src = r#"
            #[path = "sys/unix.rs"]
            mod sys;
            #[cfg(test)]
            // fixtures live outside src
            #[path = "../testdata/f.rs"]
            mod fixtures;
            #[path = "x.rs"]
            fn not_a_mod() {}
            mod plain;
        "#;
        let mods: Vec<(String, Option<String>)> = extract(Grammar::Rust, src)
            .into_iter()
            .filter_map(|r| match r {
                RawImport::RustMod { name, path, .. } => Some((name, path)),
                _ => None,
            })
            .collect();
        assert_eq!(
            mods,
            vec![
                ("sys".to_string(), Some("sys/unix.rs".to_string())),
                ("fixtures".to_string(), Some("../testdata/f.rs".to_string())),
                ("plain".to_string(), None),
            ]
        );
    }

    #[test]
    fn tsx_and_javascript_use_their_own_grammar() {
        let tsx = r#"import { X } from "./x"; export const Y = () => <X a="./no" />;"#;
        assert_eq!(specs(Grammar::Tsx, tsx), vec!["./x"]);
        let js = r#"const m = require("./m"); import("./n"); export * from "./o";"#;
        assert_eq!(specs(Grammar::JavaScript, js), vec!["./m", "./n", "./o"]);
    }

    #[test]
    fn python_captures_import_and_from_forms() {
        let src = "import a.b\nimport c as d\nfrom .e import f, g as h\nfrom .. import i\nfrom j.k import *\n";
        let got = extract(Grammar::Python, src);
        let from = |m: &str, n: Option<&str>| RawImport::PyFrom {
            module: m.into(),
            name: n.map(Into::into),
        };
        assert_eq!(
            got,
            vec![
                RawImport::PyModule("a.b".into()),
                RawImport::PyModule("c".into()),
                from(".e", Some("f")),
                from(".e", Some("g")),
                from("..", Some("i")),
                from("j.k", None),
            ]
        );
    }

    #[test]
    fn go_captures_single_grouped_aliased_and_raw_paths() {
        let src = "package x\nimport \"fmt\"\nimport (\n  m \"a/b\"\n  _ `c/d`\n)\n";
        assert_eq!(
            extract(Grammar::Go, src),
            vec![
                RawImport::GoPath("fmt".into()),
                RawImport::GoPath("a/b".into()),
                RawImport::GoPath("c/d".into()),
            ]
        );
    }

    #[test]
    fn rust_expands_use_trees_and_tracks_inline_modules() {
        let src = r#"
            use crate::{a::{self, b as c}, d::*};
            pub use super::e::F;
            mod g;
            mod h { use super::i; mod j; }
            extern crate k as l;
        "#;
        let seg = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let use_ = |s: &[&str], scope: &[&str]| RawImport::RustUse {
            segments: seg(s),
            scope: seg(scope),
        };
        let mut got = extract(Grammar::Rust, src);
        got.sort_by_key(|r| format!("{r:?}"));
        let mut want = vec![
            use_(&["crate", "a", "self"], &[]),
            use_(&["crate", "a", "b"], &[]),
            use_(&["crate", "d"], &[]),
            use_(&["super", "e", "F"], &[]),
            use_(&["super", "i"], &["h"]),
            use_(&["k"], &[]),
            RawImport::RustExternAlias {
                krate: "k".into(),
                alias: "l".into(),
            },
            RawImport::RustMod {
                name: "g".into(),
                scope: vec![],
                path: None,
            },
            RawImport::RustMod {
                name: "j".into(),
                scope: seg(&["h"]),
                path: None,
            },
        ];
        want.sort_by_key(|r| format!("{r:?}"));
        assert_eq!(got, want);
    }

    /// One line per C# fact: `type <full name>`, `using [global] <kind> <target> @<scope>`,
    /// `ref [global::]<name> [attr] @<namespace>|<enclosing types>`.
    fn csharp(src: &str) -> Vec<String> {
        use crate::csharp::{CsFact, UsingKind};
        let mut out: Vec<String> = extract(Grammar::CSharp, src)
            .into_iter()
            .map(|r| match r {
                RawImport::CSharp(CsFact::Type(name)) => format!("type {name}"),
                RawImport::CSharp(CsFact::Using(u)) => {
                    let kind = match &u.kind {
                        UsingKind::Namespace => "namespace".to_string(),
                        UsingKind::Static => "static".to_string(),
                        UsingKind::Alias(a) => format!("alias {a} ="),
                    };
                    let global = if u.global { "global " } else { "" };
                    format!(
                        "using {global}{kind} {} @{}",
                        u.target.join("."),
                        u.scope.join(".")
                    )
                }
                RawImport::CSharp(CsFact::Ref(r)) => format!(
                    "ref {}{}{} @{}|{}",
                    if r.global { "global::" } else { "" },
                    r.name.join("."),
                    if r.attribute { " [attr]" } else { "" },
                    r.namespace.join("."),
                    r.types.join("."),
                ),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn csharp_captures_declarations_usings_and_type_references() {
        let src = r#"
            global using Shop.Common;
            using System;
            using static Shop.Util.Math;
            using Alias = Shop.Models.Thing<int>;

            namespace Shop.App
            {
                using Inner.Ns;

                [Serializable, Obsolete("x")]
                public partial class Outer<TItem> : Base<Arg>, IFace where TItem : IConstraint
                {
                    private Field field;
                    public Prop Prop { get; set; }

                    public Ret Method(Param p, TItem item)
                    {
                        var x = new Created();
                        var t = typeof(TypeOfArg);
                        var n = nameof(NameOfArg);
                        Static.Call();
                        var c = (Cast)p;
                        if (p is Pattern pat) { }
                        var a = p as AsType;
                        try { } catch (CaughtException) { }
                        List<GenericArg[]> l = null;
                        int i = 0;
                        return default;
                    }

                    public class Nested { Nested2 n; }
                }

                public interface IThing { }
                public struct S { }
                public enum E { A }
                public record R(RecArg A);
                public record struct RS;
                public delegate DelRet Del(DelArg a);
            }
        "#;
        let at = "@Shop.App|Outer";
        let want: Vec<String> = [
            "type Shop.App.Del".to_string(),
            "type Shop.App.E".to_string(),
            "type Shop.App.IThing".to_string(),
            "type Shop.App.Outer".to_string(),
            "type Shop.App.Outer.Nested".to_string(),
            "type Shop.App.R".to_string(),
            "type Shop.App.RS".to_string(),
            "type Shop.App.S".to_string(),
            "using alias Alias = Shop.Models.Thing @".to_string(),
            "using global namespace Shop.Common @".to_string(),
            "using namespace Inner.Ns @Shop.App".to_string(),
            "using namespace System @".to_string(),
            "using static Shop.Util.Math @".to_string(),
            "ref DelArg @Shop.App|Del".to_string(),
            "ref DelRet @Shop.App|Del".to_string(),
            "ref Nested2 @Shop.App|Outer.Nested".to_string(),
            "ref RecArg @Shop.App|R".to_string(),
        ]
        .into_iter()
        .chain(
            [
                "Arg",
                "AsType",
                "Base",
                "Cast",
                "CaughtException",
                "Created",
                "Field",
                "GenericArg",
                "IConstraint",
                "IFace",
                "List",
                "NameOfArg",
                "Param",
                "Pattern",
                "Prop",
                "Ret",
                "Static",
                "TypeOfArg",
            ]
            .iter()
            .map(|n| format!("ref {n} {at}")),
        )
        .chain(
            ["Obsolete", "Serializable"]
                .iter()
                .map(|n| format!("ref {n} [attr] {at}")),
        )
        .collect();
        let mut want = want;
        want.sort();
        assert_eq!(
            csharp(src),
            want,
            "`TItem` is a type parameter, `int`/`var` are no names"
        );
    }

    #[test]
    fn csharp_file_scoped_namespaces_qualified_and_rooted_names() {
        let src = r#"
            using Before.Ns;
            namespace Shop.Core;
            using After.Ns;

            class C : global::Shop.Rooted.Base
            {
                Shop.Qualified.Type<Arg>.Nested f;
                object g = A.B.Constant.Value;
                bool h(object o) => o is IsType;
                bool i(object o) => o is not Negated;
                void k() { Generic<int>.Make(); }
            }
        "#;
        let at = "@Shop.Core|C";
        let mut want: Vec<String> = vec![
            "type Shop.Core.C".to_string(),
            "using namespace Before.Ns @".to_string(),
            "using namespace After.Ns @Shop.Core".to_string(),
            format!("ref global::Shop.Rooted.Base {at}"),
            format!("ref Shop.Qualified.Type.Nested {at}"),
            format!("ref Arg {at}"),
            format!("ref A {at}"),
            format!("ref A.B {at}"),
            format!("ref A.B.Constant {at}"),
            format!("ref IsType {at}"),
            format!("ref Negated {at}"),
            format!("ref Generic {at}"),
        ];
        want.sort();
        assert_eq!(csharp(src), want);
    }
}
