//! Glob compilation shared by config, catalog and routing.
use globset::{GlobBuilder, GlobMatcher};

/// Compiles a glob with the contract semantics: `*` does not cross `/`, `**` does.
pub(crate) fn compile(glob: &str) -> Result<GlobMatcher, globset::Error> {
    Ok(GlobBuilder::new(glob)
        .literal_separator(true)
        .build()?
        .compile_matcher())
}

/// Returns true when the glob matches at least one of `files` (subtree
/// entries included, see [`matches_entry`]).
/// An invalid glob matches nothing (validation reports it separately).
pub(crate) fn matches_any(glob: &str, files: &[String]) -> bool {
    compile(glob).is_ok_and(|matcher| files.iter().any(|f| matches_entry(glob, &matcher, f)))
}

const METACHARACTERS: [char; 4] = ['*', '?', '[', '{'];

/// The text of `glob` before its first `*`, `?`, `[` or `{` (all of it when
/// it has none).
pub(crate) fn literal_prefix(glob: &str) -> &str {
    glob.find(METACHARACTERS).map_or(glob, |at| &glob[..at])
}

/// Whether `entry` matches `glob` (compiled as `matcher`). A plain path uses
/// the matcher. A subtree entry `D/` (a submodule whose inner change could not
/// be read, cli.md "Changed files") stands for every path under it: it matches
/// when the glob matches `D/`, when the glob's literal prefix starts with
/// `D/`, or when the glob has a metacharacter and `D/` starts with its literal
/// prefix.
pub(crate) fn matches_entry(glob: &str, matcher: &GlobMatcher, entry: &str) -> bool {
    if !entry.ends_with('/') {
        return matcher.is_match(entry);
    }
    let prefix = literal_prefix(glob);
    matcher.is_match(entry)
        || prefix.starts_with(entry)
        || (prefix.len() < glob.len() && entry.starts_with(prefix))
}

/// A compiled set of path globs (a journey's `paths`) with the contract
/// semantics; an invalid glob matches nothing.
pub struct PathMatcher {
    globs: Vec<(String, GlobMatcher)>,
}

impl PathMatcher {
    pub fn new(globs: &[String]) -> Self {
        Self {
            globs: globs
                .iter()
                .filter_map(|g| compile(g).ok().map(|m| (g.clone(), m)))
                .collect(),
        }
    }

    /// Whether `path` (or the subtree entry `D/`) matches any glob.
    pub fn is_match(&self, path: &str) -> bool {
        self.globs
            .iter()
            .any(|(glob, m)| matches_entry(glob, m, path))
    }

    pub fn is_empty(&self) -> bool {
        self.globs.is_empty()
    }

    /// Whether some glob can match a path under `prefix` (`""` for the
    /// superproject, `sub/` for a nested repository).
    pub fn may_match_under(&self, prefix: &str) -> bool {
        self.globs.iter().any(|(glob, _)| reaches(glob, prefix))
    }

    /// The globs that reach `prefix`, rewritten relative to it as git
    /// `:(glob)` pathspecs; `None` when one of them cannot be written that way
    /// (it starts above the prefix, or uses `{a,b}`), so the caller scans and
    /// filters with [`Self::is_match`] instead.
    pub fn pathspecs_under(&self, prefix: &str) -> Option<Vec<String>> {
        let mut out = Vec::new();
        for (glob, _) in self.globs.iter().filter(|(g, _)| reaches(g, prefix)) {
            let relative = glob.strip_prefix(prefix)?;
            if relative.contains('{') {
                return None;
            }
            out.push(format!(
                ":(glob){}",
                if relative.is_empty() { "**" } else { relative }
            ));
        }
        Some(out)
    }
}

/// `glob` starts inside `prefix`, or its literal part stops above it.
fn reaches(glob: &str, prefix: &str) -> bool {
    let literal = literal_prefix(glob);
    glob.starts_with(prefix) || (literal.len() < glob.len() && prefix.starts_with(literal))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_star_stays_within_one_directory() {
        let m = compile("web/*").unwrap();
        assert!(m.is_match("web/a.ts"));
        assert!(!m.is_match("web/a/b.ts"));
    }

    #[test]
    fn double_star_crosses_directories() {
        let m = compile("web/**").unwrap();
        assert!(m.is_match("web/a/b/c.ts"));
        assert!(compile("**").unwrap().is_match("a/b"));
    }

    #[test]
    fn matches_any_ignores_invalid_globs() {
        let files = vec!["a/b.ts".to_string()];
        assert!(matches_any("a/*", &files));
        assert!(!matches_any("c/**", &files));
        assert!(!matches_any("a/[", &files));
    }

    fn entry(glob: &str, entry: &str) -> bool {
        matches_entry(glob, &compile(glob).unwrap(), entry)
    }

    #[test]
    fn literal_prefix_stops_at_the_first_metacharacter() {
        assert_eq!(literal_prefix("frontend/src/**"), "frontend/src/");
        assert_eq!(literal_prefix("**/*.ts"), "");
        assert_eq!(literal_prefix("front*/x"), "front");
        assert_eq!(literal_prefix("a/[bc]"), "a/");
        assert_eq!(literal_prefix("a/{b,c}"), "a/");
        assert_eq!(literal_prefix("a/b?.ts"), "a/b");
        assert_eq!(literal_prefix("plain/file.ts"), "plain/file.ts");
    }

    #[test]
    fn subtree_entry_matches_globs_under_it() {
        for glob in [
            "frontend/**",
            "frontend/src/**",
            "**/*.ts",
            "front*/x",
            "frontend/src/a.ts",
            "frontend/*",
        ] {
            assert!(entry(glob, "frontend/"), "{glob}");
        }
        assert!(entry("apps/frontend/**", "apps/"));
    }

    #[test]
    fn subtree_entry_does_not_match_sibling_globs() {
        for glob in ["web/**", "frontendx/**", "frontend", "front/**"] {
            assert!(!entry(glob, "frontend/"), "{glob}");
        }
    }

    #[test]
    fn plain_entry_matching_is_unchanged() {
        assert!(entry("web/*", "web/a.ts"));
        assert!(!entry("web/*", "web/a/b.ts"));
        assert!(!entry("frontend/src/**", "frontend"));
        assert!(entry("**/*.ts", "x/y.ts"));
    }

    #[test]
    fn matches_any_counts_subtree_entries() {
        let files = vec!["frontend/".to_string()];
        assert!(matches_any("frontend/src/**", &files));
        assert!(!matches_any("backend/**", &files));
    }
}

#[cfg(test)]
mod path_matcher_tests {
    use super::*;

    #[test]
    fn a_path_matcher_matches_any_of_its_globs_and_skips_invalid_ones() {
        let m = PathMatcher::new(&["web/*".to_string(), "a/[".to_string(), "api/**".to_string()]);
        assert!(m.is_match("web/a.ts"));
        assert!(m.is_match("api/x/y.cs"));
        assert!(!m.is_match("web/a/b.ts"));
        assert!(!m.is_match("other.ts"));
        assert!(m.is_match("api/"));
        assert!(PathMatcher::new(&[]).is_empty());
    }

    fn globs(list: &[&str]) -> PathMatcher {
        PathMatcher::new(&list.iter().map(|g| g.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn a_path_matcher_knows_which_repository_prefixes_it_can_reach() {
        let m = globs(&["api/Areas/Promotions/**", "web/*.ts"]);
        assert!(m.may_match_under(""));
        assert!(m.may_match_under("api/"));
        assert!(!m.may_match_under("db/"));
        assert!(globs(&["**/*.cs"]).may_match_under("db/"));
        assert!(globs(&["ap*/x"]).may_match_under("api/"));
    }

    #[test]
    fn pathspecs_under_a_prefix_are_relative_git_glob_magic() {
        let m = globs(&["api/Areas/Promotions/**", "web/*.ts"]);
        assert_eq!(
            m.pathspecs_under("api/"),
            Some(vec![":(glob)Areas/Promotions/**".to_string()])
        );
        assert_eq!(
            m.pathspecs_under(""),
            Some(vec![
                ":(glob)api/Areas/Promotions/**".to_string(),
                ":(glob)web/*.ts".to_string()
            ])
        );
        // Not expressible as a pathspec under the prefix: scan and filter.
        assert_eq!(globs(&["**/*.cs"]).pathspecs_under("db/"), None);
        assert_eq!(globs(&["api/{a,b}/**"]).pathspecs_under("api/"), None);
        assert_eq!(m.pathspecs_under("db/"), Some(vec![]));
    }
}
