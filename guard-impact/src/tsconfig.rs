//! `tsconfig.json` / `jsconfig.json`: the `compilerOptions.baseUrl` and
//! `compilerOptions.paths` that steer non-relative module resolution, with
//! `extends` chains (a string or, since TypeScript 5.0, an array) followed
//! inside the repository. Files are JSONC: comments and trailing commas are
//! allowed.

use crate::resolve::{file_name, join_dir, normalize, parent};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// How deep an `extends` chain is followed (cycles stop earlier).
const MAX_EXTENDS_DEPTH: usize = 16;

/// `paths` patterns (pattern -> substitutions) in declaration order.
pub(crate) type PathPatterns = Vec<(String, Vec<String>)>;

/// The resolution options of one config, after `extends`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TsOptions {
    /// Repo-relative `baseUrl` directory.
    pub(crate) base_url: Option<String>,
    /// `paths` patterns in declaration order, with the directory their
    /// substitutions are relative to when no `baseUrl` is set (the directory
    /// of the config that declared `paths`).
    pub(crate) paths: Option<(String, PathPatterns)>,
}

impl TsOptions {
    fn is_empty(&self) -> bool {
        self.base_url.is_none() && self.paths.is_none()
    }

    /// Candidate repo paths (before extension probing) for a non-relative
    /// specifier, in the order TypeScript tries them: the substitutions of the
    /// best `paths` pattern, then `baseUrl/<spec>`.
    pub(crate) fn candidates(&self, spec: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Some((paths_dir, patterns)) = &self.paths {
            let base = self.base_url.as_deref().unwrap_or(paths_dir);
            if let Some((star, substitutions)) = best_pattern(patterns, spec) {
                for sub in substitutions {
                    let sub = match &star {
                        Some(s) => sub.replacen('*', s, 1),
                        None => sub.clone(),
                    };
                    out.extend(normalize(&join_dir(base, &sub)));
                }
            }
        }
        if let Some(base) = &self.base_url {
            out.extend(normalize(&join_dir(base, spec)));
        }
        out
    }
}

/// An exact pattern wins; otherwise the wildcard pattern with the longest
/// prefix. Returns the text the `*` matched (`None` for an exact match).
fn best_pattern<'a>(
    patterns: &'a PathPatterns,
    spec: &str,
) -> Option<(Option<String>, &'a Vec<String>)> {
    if let Some((_, subs)) = patterns.iter().find(|(p, _)| !p.contains('*') && p == spec) {
        return Some((None, subs));
    }
    patterns
        .iter()
        .filter_map(|(p, subs)| {
            let (prefix, suffix) = p.split_once('*')?;
            let middle = spec.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((prefix.len(), middle, subs))
        })
        .max_by_key(|(len, _, _)| *len)
        .map(|(_, middle, subs)| (Some(middle.to_string()), subs))
}

/// The resolution options of every config directory of the repository.
#[derive(Debug, Default)]
pub(crate) struct TsConfigs {
    /// directory -> options of its `tsconfig.json` (else `jsconfig.json`).
    by_dir: BTreeMap<String, TsOptions>,
}

impl TsConfigs {
    /// `configs` are the repo's `tsconfig.json` / `jsconfig.json` files;
    /// `read` returns any repository file (for `extends` targets).
    pub(crate) fn load(
        files: &BTreeSet<String>,
        configs: &[String],
        read: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let mut by_dir = BTreeMap::new();
        // tsconfig.json first, so it wins over a jsconfig.json beside it.
        let mut ordered: Vec<&String> = configs.iter().collect();
        ordered.sort_by_key(|c| (file_name(c) != "tsconfig.json", c.to_string()));
        for config in ordered {
            let dir = parent(config).to_string();
            if by_dir.contains_key(&dir) {
                continue;
            }
            // Kept even without baseUrl/paths: the nearest config governs
            // its files, so a parent config's options never apply to them.
            let options = effective(files, config, read, &mut Vec::new());
            by_dir.insert(dir, options);
        }
        Self { by_dir }
    }

    /// Options of the nearest config at or above `file`'s directory; `None`
    /// when there is none, or when it sets neither `baseUrl` nor `paths`.
    pub(crate) fn for_file(&self, file: &str) -> Option<&TsOptions> {
        let mut dir = parent(file);
        loop {
            if let Some(o) = self.by_dir.get(dir) {
                return (!o.is_empty()).then_some(o);
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent(dir);
        }
    }
}

/// Options of `config` with its `extends` chain applied (the config's own
/// keys override inherited ones, later `extends` entries override earlier).
fn effective(
    files: &BTreeSet<String>,
    config: &str,
    read: &dyn Fn(&str) -> Option<String>,
    chain: &mut Vec<String>,
) -> TsOptions {
    if chain.len() >= MAX_EXTENDS_DEPTH || chain.iter().any(|c| c == config) {
        return TsOptions::default();
    }
    let Some(json) = read(config).and_then(|t| parse_jsonc(&t)) else {
        return TsOptions::default();
    };
    chain.push(config.to_string());
    let dir = parent(config);
    let mut options = TsOptions::default();
    let extends: Vec<&str> = match json.get("extends") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    for ext in extends {
        if let Some(base) = extends_target(files, dir, ext) {
            let inherited = effective(files, &base, read, chain);
            if inherited.base_url.is_some() {
                options.base_url = inherited.base_url;
            }
            if inherited.paths.is_some() {
                options.paths = inherited.paths;
            }
        }
    }
    chain.pop();
    let compiler = json.get("compilerOptions");
    if let Some(base_url) = compiler
        .and_then(|c| c.get("baseUrl"))
        .and_then(Value::as_str)
    {
        options.base_url = normalize(&join_dir(dir, base_url));
    }
    if let Some(paths) = compiler
        .and_then(|c| c.get("paths"))
        .and_then(Value::as_object)
    {
        let patterns = paths
            .iter()
            .map(|(pattern, subs)| {
                let subs = subs
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                (pattern.clone(), subs)
            })
            .collect();
        options.paths = Some((dir.to_string(), patterns));
    }
    options
}

/// A relative `extends` names a file (`.json` optional); a package name is
/// outside the repository and is not followed.
fn extends_target(files: &BTreeSet<String>, dir: &str, ext: &str) -> Option<String> {
    if !(ext.starts_with("./") || ext.starts_with("../")) {
        return None;
    }
    let path = normalize(&join_dir(dir, ext))?;
    [path.clone(), format!("{path}.json")]
        .into_iter()
        .find(|p| files.contains(p))
}

/// Parses JSON with `//` and `/* */` comments and trailing commas.
pub(crate) fn parse_jsonc(text: &str) -> Option<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match c {
                '\\' => out.extend(chars.next()),
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
                out.push(' ');
            }
            '}' | ']' => {
                let trimmed = out.trim_end().len();
                if out[..trimmed].ends_with(',') {
                    out.truncate(trimmed - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    serde_json::from_str(&out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_comments_and_trailing_commas_are_accepted() {
        let v = parse_jsonc(
            "\u{feff}{\n // c \"x\"\n \"a\": \"http://x/*y*/\", /* b */ \"b\": [1, 2,],\n}",
        )
        .unwrap();
        assert_eq!(v["a"], "http://x/*y*/");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert!(parse_jsonc("{ \"a\": }").is_none());
    }

    #[test]
    fn exact_patterns_beat_wildcards_and_longer_prefixes_win() {
        let o = TsOptions {
            base_url: None,
            paths: Some((
                "web".into(),
                vec![
                    ("@/*".into(), vec!["src/*".into()]),
                    ("@/ui/*".into(), vec!["ui/*".into(), "ui2/*".into()]),
                    ("@/ui/button".into(), vec!["button.ts".into()]),
                    ("*".into(), vec!["vendor/*".into()]),
                ],
            )),
        };
        assert_eq!(o.candidates("@/ui/button"), vec!["web/button.ts"]);
        assert_eq!(o.candidates("@/ui/x"), vec!["web/ui/x", "web/ui2/x"]);
        assert_eq!(o.candidates("@/a/b"), vec!["web/src/a/b"]);
        assert_eq!(o.candidates("lodash"), vec!["web/vendor/lodash"]);
        let escaping = TsOptions {
            base_url: Some(String::new()),
            paths: None,
        };
        assert!(escaping.candidates("../x").is_empty());
    }

    #[test]
    fn extends_chains_stop_at_cycles_and_packages() {
        let map: BTreeMap<String, String> = [
            ("a/tsconfig.json", "{ \"extends\": [\"@tsconfig/node20\", \"../b\"] }"),
            ("b.json", "{ \"extends\": \"./a/tsconfig.json\", \"compilerOptions\": { \"baseUrl\": \"lib\" } }"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let files: BTreeSet<String> = map.keys().cloned().collect();
        let configs = TsConfigs::load(&files, &["a/tsconfig.json".into()], &|p| {
            map.get(p).cloned()
        });
        assert_eq!(
            configs
                .for_file("a/src/x.ts")
                .and_then(|o| o.base_url.as_deref()),
            Some("lib")
        );
        assert!(configs.for_file("other/x.ts").is_none());
    }

    #[test]
    fn a_nearer_config_without_resolution_options_still_governs_its_files() {
        let map: BTreeMap<String, String> = [
            (
                "tsconfig.json",
                "{ \"compilerOptions\": { \"baseUrl\": \"src\" } }",
            ),
            (
                "pkg/tsconfig.json",
                "{ \"compilerOptions\": { \"strict\": true } }",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let files: BTreeSet<String> = map.keys().cloned().collect();
        let configs = TsConfigs::load(
            &files,
            &["tsconfig.json".into(), "pkg/tsconfig.json".into()],
            &|p| map.get(p).cloned(),
        );
        assert!(configs.for_file("pkg/a.ts").is_none());
        assert!(configs.for_file("pkg/deep/a.ts").is_none());
        assert_eq!(
            configs
                .for_file("other/a.ts")
                .and_then(|o| o.base_url.as_deref()),
            Some("src")
        );
    }
}
