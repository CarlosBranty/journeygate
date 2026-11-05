//! The user credentials file of `guard connect` (cli.md, "User credentials
//! file"): its keys, parsing, rendering, masking and the environment
//! precedence. Pure: the CLI does the file IO and sets the variables.
//!
//! Values are secrets: no error or warning here ever contains one.

use std::collections::BTreeMap;

/// The only keys the file may hold, in the order they are written and listed.
pub const KNOWN_KEYS: &[&str] = &[
    "JIRA_BASE_URL",
    "JIRA_EMAIL",
    "JIRA_API_TOKEN",
    "GITHUB_TOKEN",
    "GITHUB_API_URL",
    "GITLAB_TOKEN",
    "GITLAB_API_URL",
];

/// Key -> value, only [`KNOWN_KEYS`].
pub type Credentials = BTreeMap<String, String>;

/// `true` for the keys whose value is masked (`*_TOKEN`).
pub fn is_secret(key: &str) -> bool {
    key.ends_with("_TOKEN")
}

/// The known keys of `text` plus one warning per ignored entry. Invalid TOML
/// is an error that names only the line, never its content.
pub fn parse(text: &str) -> Result<(Credentials, Vec<String>), String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| {
        // The toml message quotes the offending line: never pass it on.
        let line = e
            .span()
            .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1);
        match line {
            Some(line) => format!("credentials file: invalid TOML at line {line}"),
            None => "credentials file: invalid TOML".to_string(),
        }
    })?;
    let mut credentials = Credentials::new();
    let mut warnings = Vec::new();
    for (key, value) in table {
        if !KNOWN_KEYS.contains(&key.as_str()) {
            warnings.push(format!("credentials file: unknown key {}", printable(&key)));
            continue;
        }
        match value {
            toml::Value::String(value) => {
                credentials.insert(key, value);
            }
            _ => warnings.push(format!("credentials file: {key} is not a string")),
        }
    }
    Ok((credentials, warnings))
}

/// An unknown key as shown in a warning: at most 64 printable characters.
fn printable(key: &str) -> String {
    key.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// The file content: a header comment, then `KEY = "value"` lines in
/// [`KNOWN_KEYS`] order. Unknown keys are dropped.
pub fn render(credentials: &Credentials) -> String {
    let mut out = String::from(
        "# guard user credentials (written by guard connect). Keep this file private.\n",
    );
    for key in KNOWN_KEYS {
        if let Some(value) = credentials.get(*key) {
            out.push_str(&format!("{key} = {}\n", toml::Value::String(value.clone())));
        }
    }
    out
}

/// The entries startup must export: those whose variable is not set yet
/// (`is_set`), in [`KNOWN_KEYS`] order. The environment always wins.
pub fn to_export(
    credentials: &Credentials,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    KNOWN_KEYS
        .iter()
        .filter(|key| !is_set(key))
        .filter_map(|key| {
            credentials
                .get(*key)
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect()
}

/// `****` + the last 4 characters when longer than 8, else `****`.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() > 8 {
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("****{tail}")
    } else {
        "****".to_string()
    }
}

/// Where the effective value of a key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Environment,
    CredentialsFile,
    NotSet,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Environment => "environment",
            Source::CredentialsFile => "credentials file",
            Source::NotSet => "not set",
        }
    }
}

/// One `guard connect --list` row: key, shown value (`-` when unset, masked
/// for secrets) and source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRow {
    pub key: &'static str,
    pub shown: String,
    pub source: Source,
}

/// Rows for every known key. `effective` is the current environment value;
/// `from_file` says whether startup exported it from the credentials file.
pub fn list_rows(
    effective: impl Fn(&str) -> Option<String>,
    from_file: impl Fn(&str) -> bool,
) -> Vec<ListRow> {
    KNOWN_KEYS
        .iter()
        .map(|key| match effective(key) {
            None => ListRow {
                key,
                shown: "-".to_string(),
                source: Source::NotSet,
            },
            Some(value) => ListRow {
                key,
                shown: if is_secret(key) { mask(&value) } else { value },
                source: if from_file(key) {
                    Source::CredentialsFile
                } else {
                    Source::Environment
                },
            },
        })
        .collect()
}

/// [`list_rows`] as aligned text: each column as wide as its widest cell plus
/// two spaces, trailing spaces trimmed.
pub fn render_list(rows: &[ListRow]) -> String {
    let key_width = rows.iter().map(|r| r.key.len()).max().unwrap_or(0) + 2;
    let value_width = rows
        .iter()
        .map(|r| r.shown.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let mut out = String::new();
    for row in rows {
        let line = format!(
            "{:key_width$}{:value_width$}{}",
            row.key,
            row.shown,
            row.source.label()
        );
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(pairs: &[(&str, &str)]) -> Credentials {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parse_keeps_known_string_keys_and_warns_about_the_rest() {
        let (map, warnings) = parse(
            "JIRA_EMAIL = \"ada@acme.test\"\nPATH = \"/evil\"\nGITHUB_TOKEN = 42\nGITLAB_TOKEN = \"glpat-1\"\n",
        )
        .unwrap();
        assert_eq!(
            map,
            creds(&[("JIRA_EMAIL", "ada@acme.test"), ("GITLAB_TOKEN", "glpat-1")])
        );
        assert_eq!(
            warnings,
            vec![
                "credentials file: GITHUB_TOKEN is not a string".to_string(),
                "credentials file: unknown key PATH".to_string(),
            ]
        );
    }

    #[test]
    fn invalid_toml_names_the_line_but_never_the_value() {
        let error = parse("GITHUB_TOKEN = \"ok\"\nJIRA_API_TOKEN = s3cr3t-value\n").unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(!error.contains("s3cr3t"), "{error}");
    }

    #[test]
    fn render_round_trips_in_known_key_order_and_escapes() {
        let input = creds(&[
            ("GITHUB_TOKEN", "gh\"p\\x"),
            ("JIRA_BASE_URL", "https://acme.atlassian.net"),
            ("PATH", "dropped"),
        ]);
        let text = render(&input);
        assert!(text.starts_with('#'), "{text}");
        let jira = text.find("JIRA_BASE_URL").unwrap();
        let github = text.find("GITHUB_TOKEN").unwrap();
        assert!(jira < github, "{text}");
        assert!(!text.contains("PATH"), "{text}");
        let (back, warnings) = parse(&text).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            back,
            creds(&[
                ("GITHUB_TOKEN", "gh\"p\\x"),
                ("JIRA_BASE_URL", "https://acme.atlassian.net"),
            ])
        );
    }

    #[test]
    fn the_environment_wins_over_the_file() {
        let file = creds(&[
            ("GITHUB_TOKEN", "from-file"),
            ("JIRA_EMAIL", "ada@acme.test"),
            ("GITLAB_TOKEN", "glpat-file"),
        ]);
        // GITHUB_TOKEN is already set (even empty counts as set): kept.
        let exported = to_export(&file, |key| key == "GITHUB_TOKEN");
        assert_eq!(
            exported,
            vec![
                ("JIRA_EMAIL".to_string(), "ada@acme.test".to_string()),
                ("GITLAB_TOKEN".to_string(), "glpat-file".to_string()),
            ]
        );
    }

    #[test]
    fn mask_keeps_only_the_last_four_of_long_values() {
        assert_eq!(mask("abcd1234wxyz"), "****wxyz");
        assert_eq!(mask("12345678"), "****");
        assert_eq!(mask(""), "****");
        assert_eq!(mask("ééééééééé9f3a"), "****9f3a");
    }

    #[test]
    fn list_rows_mask_secrets_and_name_the_source() {
        let env = |key: &str| match key {
            "JIRA_BASE_URL" => Some("https://acme.atlassian.net".to_string()),
            "JIRA_API_TOKEN" => Some("atlassian-token-9f3a".to_string()),
            "GITHUB_TOKEN" => Some("ghp_secretb7c1".to_string()),
            _ => None,
        };
        let rows = list_rows(env, |key| key.starts_with("JIRA"));
        assert_eq!(rows.len(), KNOWN_KEYS.len());
        assert_eq!(rows[0].shown, "https://acme.atlassian.net");
        assert_eq!(rows[0].source, Source::CredentialsFile);
        assert_eq!(rows[2].shown, "****9f3a");
        assert_eq!(rows[3].source, Source::Environment);
        assert_eq!(rows[5].shown, "-");
        assert_eq!(rows[5].source, Source::NotSet);
        let text = render_list(&rows);
        assert!(!text.contains("atlassian-token"), "{text}");
        assert!(!text.contains("ghp_secret"), "{text}");
        let first = text.lines().next().unwrap();
        assert_eq!(
            first,
            "JIRA_BASE_URL   https://acme.atlassian.net  credentials file"
        );
        assert!(
            text.lines()
                .any(|l| l == "GITLAB_TOKEN    -                           not set")
        );
    }
}
