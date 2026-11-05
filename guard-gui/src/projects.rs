//! Projects: a project is a folder with a `target.toml` (the app repo guard
//! runs in). Validation, the recent list and the small naming helpers of the
//! project selector live here; the window only shows them.

use crate::catalog;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

pub const TARGET_TOML: &str = "target.toml";

/// How many projects the recent list keeps.
pub const MAX_RECENT: usize = 10;

/// A file a project needs and does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    TargetToml,
    Catalog,
}

impl Missing {
    /// The path, relative to the project root.
    pub fn label(self) -> &'static str {
        match self {
            Self::TargetToml => TARGET_TOML,
            Self::Catalog => "e2e-playwright/catalog/journeys.json",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    pub missing: Vec<Missing>,
}

impl Validation {
    /// A folder is a project when it has a `target.toml`.
    pub fn is_project(&self) -> bool {
        !self.missing.contains(&Missing::TargetToml)
    }

    pub fn is_complete(&self) -> bool {
        self.missing.is_empty()
    }

    /// `missing a, b`, or `None` when nothing is missing.
    pub fn summary(&self) -> Option<String> {
        if self.missing.is_empty() {
            return None;
        }
        let names: Vec<&str> = self.missing.iter().map(|m| m.label()).collect();
        Some(format!("missing {}", names.join(", ")))
    }
}

/// Checks the files a project needs; `is_file` is injected so it is testable.
pub fn validate(root: &Path, is_file: impl Fn(&Path) -> bool) -> Validation {
    let mut missing = Vec::new();
    if !is_file(&root.join(TARGET_TOML)) {
        missing.push(Missing::TargetToml);
    }
    if !is_file(&catalog::catalog_path(root)) {
        missing.push(Missing::Catalog);
    }
    Validation { missing }
}

/// [`validate`] against the filesystem.
pub fn validate_fs(root: &Path) -> Validation {
    validate(root, Path::is_file)
}

pub fn is_project_fs(root: &Path) -> bool {
    root.join(TARGET_TOML).is_file()
}

/// Recently opened projects, most recent first, unique by canonical path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentProjects {
    paths: Vec<PathBuf>,
}

impl RecentProjects {
    /// A clean list from loaded data: no empty paths, no duplicates, capped.
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut list = Self::default();
        for path in paths {
            if path.as_os_str().is_empty() || list.contains(&path) {
                continue;
            }
            if list.paths.len() == MAX_RECENT {
                break;
            }
            list.paths.push(path);
        }
        list
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn most_recent(&self) -> Option<&Path> {
        self.paths.first().map(PathBuf::as_path)
    }

    pub fn contains(&self, path: &Path) -> bool {
        let key = path_key(path, cfg!(windows));
        self.paths.iter().any(|p| path_key(p, cfg!(windows)) == key)
    }

    /// Moves `path` (already canonical) to the front, dropping the oldest
    /// entry past [`MAX_RECENT`].
    pub fn touch(&mut self, path: PathBuf) {
        self.remove(&path);
        self.paths.insert(0, path);
        self.paths.truncate(MAX_RECENT);
    }

    /// Removes `path`; true when it was in the list.
    pub fn remove(&mut self, path: &Path) -> bool {
        let key = path_key(path, cfg!(windows));
        let before = self.paths.len();
        self.paths.retain(|p| path_key(p, cfg!(windows)) != key);
        self.paths.len() != before
    }
}

/// A comparison key: `/` separators, no trailing separator, and folded case
/// when the filesystem is case-insensitive (Windows).
pub fn path_key(path: &Path, fold_case: bool) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let trimmed = text.trim_end_matches('/');
    let text = if trimmed.is_empty() { "/" } else { trimmed };
    if fold_case {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

/// True when `candidate` (canonical) is the project already open, so opening
/// it again is a no-op: no cancel prompt, no state reset.
pub fn is_current(current: Option<&Path>, candidate: &Path, fold_case: bool) -> bool {
    current.is_some_and(|c| path_key(c, fold_case) == path_key(candidate, fold_case))
}

/// `\\?\C:\x` becomes `C:\x` and `\\?\UNC\server\share` becomes
/// `\\server\share`, the forms people know (`fs::canonicalize` on Windows
/// returns the extended ones).
pub fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return PathBuf::from(rest);
        }
    }
    path
}

/// The canonical form of `path` used for the recent list, or its absolute
/// form when it does not exist.
pub fn canonical(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(real) => strip_verbatim(real),
        Err(_) => std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
    }
}

/// The folder name, as the selector shows it.
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Two capitals for the project badge: the first letters of the first two
/// words (`-`, `_`, spaces and camelCase split words), or the first two
/// letters of a single word.
pub fn initials(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    let letters: String = match words.as_slice() {
        [] => return "?".into(),
        [one] => one.chars().filter(|c| c.is_alphabetic()).take(2).collect(),
        [first, second, ..] => first
            .chars()
            .find(|c| c.is_alphabetic())
            .into_iter()
            .chain(second.chars().find(|c| c.is_alphabetic()))
            .collect(),
    };
    if letters.is_empty() {
        "?".into()
    } else {
        letters.to_uppercase()
    }
}

/// A stable colour slot for the project badge (FNV-1a of the name).
pub fn badge_index(name: &str, slots: usize) -> usize {
    if slots == 0 {
        return 0;
    }
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize % slots
}

/// The project to open at launch: `--repo`, else the most recent project that
/// still exists, else the current directory when it is a project. An empty
/// answer means "show the welcome screen": nothing is read from the GUI's
/// own directory unless it is a project.
pub fn initial_project(
    repo_flag: Option<PathBuf>,
    recents: &RecentProjects,
    cwd: &Path,
    is_project: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if repo_flag.is_some() {
        return repo_flag;
    }
    recents
        .paths()
        .iter()
        .find(|p| is_project(p))
        .cloned()
        .or_else(|| is_project(cwd).then(|| cwd.to_path_buf()))
}

/// The output of `git rev-parse --abbrev-ref HEAD`.
pub fn parse_branch(stdout: &str) -> Option<String> {
    match stdout.trim() {
        "" => None,
        "HEAD" => Some("detached HEAD".into()),
        name => Some(name.to_string()),
    }
}
