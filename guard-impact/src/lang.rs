//! Language plugins: grammar, file extensions and the import query of each
//! language (`languages/<lang>/imports.scm` at the repository root).
//!
//! Adding a language = a grammar crate, an `imports.scm`, a resolver in
//! [`crate::resolve`] and a fixture mini-repo under `tests/fixtures/<lang>/`.

use serde::Serialize;

/// A language with a tree-sitter plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Go,
    Rust,
    CSharp,
    Php,
}

impl Language {
    pub const ALL: [Language; 7] = [
        Language::TypeScript,
        Language::JavaScript,
        Language::Python,
        Language::Go,
        Language::Rust,
        Language::CSharp,
        Language::Php,
    ];

    /// The name used in `[app].languages` of `target.toml` and in `languages/<name>/`.
    pub fn name(self) -> &'static str {
        match self {
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::CSharp => "csharp",
            Language::Php => "php",
        }
    }

    pub fn from_name(name: &str) -> Option<Language> {
        Language::ALL.into_iter().find(|l| l.name() == name)
    }

    /// The plugin that analyzes `path`, by file extension.
    pub fn for_path(path: &str) -> Option<Language> {
        Grammar::for_path(path).map(Grammar::language)
    }

    /// Languages a `[app].languages` entry turns on. TypeScript projects
    /// routinely mix in JavaScript files (`allowJs`, config files), so
    /// `typescript` also enables `javascript`.
    pub(crate) fn enables(self) -> &'static [Language] {
        match self {
            Language::TypeScript => &[Language::TypeScript, Language::JavaScript],
            Language::JavaScript => &[Language::JavaScript],
            Language::Python => &[Language::Python],
            Language::Go => &[Language::Go],
            Language::Rust => &[Language::Rust],
            Language::CSharp => &[Language::CSharp],
            Language::Php => &[Language::Php],
        }
    }

    /// The import query (`languages/<name>/imports.scm`), embedded at build time.
    pub fn import_query(self) -> &'static str {
        match self {
            Language::TypeScript => include_str!("../../languages/typescript/imports.scm"),
            Language::JavaScript => include_str!("../../languages/javascript/imports.scm"),
            Language::Python => include_str!("../../languages/python/imports.scm"),
            Language::Go => include_str!("../../languages/go/imports.scm"),
            Language::Rust => include_str!("../../languages/rust/imports.scm"),
            Language::CSharp => include_str!("../../languages/csharp/imports.scm"),
            Language::Php => include_str!("../../languages/php/imports.scm"),
        }
    }
}

/// A concrete tree-sitter grammar. TypeScript has two (`.ts` and `.tsx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Grammar {
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Rust,
    CSharp,
    Php,
}

impl Grammar {
    pub(crate) const ALL: [Grammar; 8] = [
        Grammar::TypeScript,
        Grammar::Tsx,
        Grammar::JavaScript,
        Grammar::Python,
        Grammar::Go,
        Grammar::Rust,
        Grammar::CSharp,
        Grammar::Php,
    ];

    pub(crate) fn for_path(path: &str) -> Option<Grammar> {
        let name = path.rsplit('/').next().unwrap_or(path);
        let ext = name.rsplit_once('.')?.1;
        Some(match ext {
            "ts" | "mts" | "cts" => Grammar::TypeScript,
            "tsx" => Grammar::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Grammar::JavaScript,
            "py" | "pyi" => Grammar::Python,
            "go" => Grammar::Go,
            "rs" => Grammar::Rust,
            "cs" => Grammar::CSharp,
            "php" => Grammar::Php,
            _ => return None,
        })
    }

    pub(crate) fn language(self) -> Language {
        match self {
            Grammar::TypeScript | Grammar::Tsx => Language::TypeScript,
            Grammar::JavaScript => Language::JavaScript,
            Grammar::Python => Language::Python,
            Grammar::Go => Language::Go,
            Grammar::Rust => Language::Rust,
            Grammar::CSharp => Language::CSharp,
            Grammar::Php => Language::Php,
        }
    }

    pub(crate) fn ts_language(self) -> tree_sitter::Language {
        match self {
            Grammar::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Grammar::Python => tree_sitter_python::LANGUAGE.into(),
            Grammar::Go => tree_sitter_go::LANGUAGE.into(),
            Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
            Grammar::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            // The full PHP grammar: text outside `<?php ... ?>` is HTML.
            Grammar::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for l in Language::ALL {
            assert_eq!(Language::from_name(l.name()), Some(l));
        }
        assert_eq!(Language::from_name("java"), None);
        assert_eq!(Language::from_name("csharp"), Some(Language::CSharp));
        assert_eq!(Language::CSharp.enables(), &[Language::CSharp]);
        assert_eq!(Language::from_name("php"), Some(Language::Php));
        assert_eq!(Language::Php.enables(), &[Language::Php]);
    }

    #[test]
    fn extensions_map_to_grammars() {
        assert_eq!(Grammar::for_path("a/b.ts"), Some(Grammar::TypeScript));
        assert_eq!(Grammar::for_path("a/b.d.ts"), Some(Grammar::TypeScript));
        assert_eq!(Grammar::for_path("a/b.tsx"), Some(Grammar::Tsx));
        assert_eq!(Grammar::for_path("b.cjs"), Some(Grammar::JavaScript));
        assert_eq!(Grammar::for_path("x/y.pyi"), Some(Grammar::Python));
        assert_eq!(Grammar::for_path("main.go"), Some(Grammar::Go));
        assert_eq!(Grammar::for_path("src/lib.rs"), Some(Grammar::Rust));
        assert_eq!(Grammar::for_path("Shop/Order.cs"), Some(Grammar::CSharp));
        assert_eq!(Grammar::for_path("Shop/Shop.csproj"), None);
        assert_eq!(Grammar::for_path("Makefile"), None);
        assert_eq!(Grammar::for_path("a.b/Makefile"), None);
        assert_eq!(Language::for_path("x.tsx"), Some(Language::TypeScript));
        assert_eq!(Language::for_path("A.cs"), Some(Language::CSharp));
        assert_eq!(Grammar::for_path("app/Models/User.php"), Some(Grammar::Php));
        assert_eq!(Grammar::for_path("composer.json"), None);
        assert_eq!(Language::for_path("index.php"), Some(Language::Php));
    }

    #[test]
    fn csharp_and_php_serialize_as_their_target_toml_names() {
        for l in [Language::CSharp, Language::Php] {
            assert_eq!(
                serde_json::to_string(&l).unwrap(),
                format!("\"{}\"", l.name())
            );
        }
    }

    #[test]
    fn every_query_compiles_against_its_grammars() {
        for g in Grammar::ALL {
            let q = tree_sitter::Query::new(&g.ts_language(), g.language().import_query());
            assert!(q.is_ok(), "{g:?}: {:?}", q.err());
        }
    }
}
