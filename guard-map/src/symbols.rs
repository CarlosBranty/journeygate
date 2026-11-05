//! A small, dependency-free definition scanner for backends that expose an
//! import graph but no symbol table (`guard-impact`).
//!
//! It is line-based and deliberately conservative: it recognizes the common
//! declaration keywords of Rust, Python, Go and TypeScript/JavaScript at the
//! start of a line and never parses expressions. Precision matters more than
//! recall here: the map shows at most a handful of names per file.

use crate::Symbol;

/// Language family of `path` for scanning, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Rust,
    Python,
    Go,
    Script,
}

fn family(path: &str) -> Option<Family> {
    let ext = path.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => Family::Rust,
        "py" | "pyi" => Family::Python,
        "go" => Family::Go,
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Family::Script,
        _ => return None,
    })
}

/// Definitions of `source` (the contents of `path`), in source order.
/// Unknown languages yield an empty list.
pub fn scan_symbols(path: &str, source: &str) -> Vec<Symbol> {
    let Some(family) = family(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let indented = line.starts_with([' ', '\t']);
        if let Some((kind, name)) = scan_line(family, line.trim_start(), indented) {
            out.push(Symbol {
                name,
                kind: kind.to_string(),
                line: u32::try_from(i + 1).unwrap_or(u32::MAX),
            });
        }
    }
    out
}

fn scan_line(family: Family, line: &str, indented: bool) -> Option<(&'static str, String)> {
    match family {
        Family::Rust => {
            let rest = strip_modifiers(
                line,
                &[
                    "pub(crate)",
                    "pub(super)",
                    "pub(self)",
                    "pub",
                    "async",
                    "unsafe",
                    "extern \"C\"",
                ],
            );
            // `const fn` is a function, `const NAME` a constant.
            let rest = rest
                .strip_prefix("const ")
                .filter(|r| r.trim_start().starts_with("fn "))
                .map_or(rest, str::trim_start);
            keyword(
                rest,
                &[
                    ("fn", "function"),
                    ("struct", "struct"),
                    ("enum", "enum"),
                    ("trait", "trait"),
                    ("type", "type"),
                    ("const", "const"),
                    ("static", "static"),
                    ("mod", "module"),
                    ("macro_rules!", "macro"),
                ],
            )
        }
        Family::Python => {
            let rest = strip_modifiers(line, &["async"]);
            let found = keyword(rest, &[("def", "function"), ("class", "class")])?;
            // Nested helpers are noise; methods keep their class's indentation level 1.
            if indented && found.0 == "class" {
                return None;
            }
            Some(found)
        }
        Family::Go => {
            if let Some(rest) = line.strip_prefix("func ") {
                // Methods: `func (r *T) Name(`.
                let rest = if rest.starts_with('(') {
                    rest.split_once(')')
                        .map(|(_, r)| r.trim_start())
                        .unwrap_or("")
                } else {
                    rest
                };
                return ident(rest).map(|n| ("function", n));
            }
            if indented {
                return None;
            }
            keyword(line, &[("type", "type")])
        }
        Family::Script => {
            let rest =
                strip_modifiers(line, &["export", "default", "declare", "abstract", "async"]);
            let found = keyword(
                rest,
                &[
                    ("function*", "function"),
                    ("function", "function"),
                    ("class", "class"),
                    ("interface", "interface"),
                    ("type", "type"),
                    ("enum", "enum"),
                    ("const", "const"),
                    ("let", "variable"),
                    ("var", "variable"),
                ],
            )?;
            if indented && matches!(found.0, "const" | "variable") {
                return None;
            }
            Some(found)
        }
    }
}

/// Removes any number of leading `modifiers` (each followed by whitespace).
fn strip_modifiers<'a>(mut line: &'a str, modifiers: &[&str]) -> &'a str {
    loop {
        let before = line;
        for m in modifiers {
            if let Some(rest) = line.strip_prefix(m)
                && rest.starts_with([' ', '\t'])
            {
                line = rest.trim_start();
            }
        }
        if line == before {
            return line;
        }
    }
}

/// `keyword name...` for one of `keywords`, returning `(kind, name)`.
fn keyword(line: &str, keywords: &[(&str, &'static str)]) -> Option<(&'static str, String)> {
    for (kw, kind) in keywords {
        if let Some(rest) = line.strip_prefix(kw) {
            let glued = kw.ends_with(['!', '*']);
            if !glued && !rest.starts_with([' ', '\t']) {
                continue;
            }
            if let Some(name) = ident(rest.trim_start()) {
                return Some((kind, name));
            }
        }
    }
    None
}

fn ident(text: &str) -> Option<String> {
    let name: String = text
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
        .collect();
    let first = name.chars().next()?;
    (!first.is_ascii_digit()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(path: &str, src: &str) -> Vec<String> {
        scan_symbols(path, src)
            .into_iter()
            .map(|s| format!("{}:{}:{}", s.kind, s.name, s.line))
            .collect()
    }

    #[test]
    fn rust_definitions() {
        let src = "use x;\npub struct A;\npub(crate) fn b() {}\nimpl A {\n    pub async fn c(&self) {}\n}\nenum E {}\npub const fn d() {}\nmacro_rules! m {}\nconst N: u8 = 1;\n// fn not_me\nlet x = 1;\n";
        assert_eq!(
            names("src/lib.rs", src),
            [
                "struct:A:2",
                "function:b:3",
                "function:c:5",
                "enum:E:7",
                "function:d:8",
                "macro:m:9",
                "const:N:10"
            ]
        );
    }

    #[test]
    fn python_definitions() {
        let src = "import os\nclass Console:\n    def print(self):\n        class Inner: pass\nasync def main():\n    pass\n";
        assert_eq!(
            names("rich/console.py", src),
            ["class:Console:2", "function:print:3", "function:main:5"]
        );
    }

    #[test]
    fn go_definitions() {
        let src = "package x\nfunc New() *T {}\nfunc (t *T) Serve() {}\ntype T struct{}\n";
        assert_eq!(
            names("x/x.go", src),
            ["function:New:2", "function:Serve:3", "type:T:4"]
        );
    }

    #[test]
    fn script_definitions() {
        let src = "import { a } from './a'\nexport default class Hono {\n  const inner = 1\n}\nexport async function serve() {}\nexport const handler = () => {}\nexport interface Env {}\ntype Path = string\nfunction* gen() {}\n";
        assert_eq!(
            names("src/hono.ts", src),
            [
                "class:Hono:2",
                "function:serve:5",
                "const:handler:6",
                "interface:Env:7",
                "type:Path:8",
                "function:gen:9"
            ]
        );
    }

    #[test]
    fn unknown_language_is_empty() {
        assert!(scan_symbols("README.md", "fn x() {}").is_empty());
        assert!(scan_symbols("Makefile", "class A").is_empty());
    }
}
