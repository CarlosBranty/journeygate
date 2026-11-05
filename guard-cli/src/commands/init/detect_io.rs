//! The I/O half of detection (detection.md "Inputs"): list the repository's
//! files and read only the small files `guard_core::detect::wanted_files`
//! asks for. Nothing else is ever opened.

use crate::context::{Context, slash};
use crate::git;
use guard_core::detect::{DETECT_IGNORED_DIRS, DetectInput, wanted_files};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Most files listed; the rest is dropped with a note.
const MAX_FILES: usize = 50_000;
/// Larger files are skipped with a note.
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_ROUNDS: usize = 3;

fn ignored(path: &str) -> bool {
    path.split('/')
        .rev()
        .skip(1)
        .any(|segment| DETECT_IGNORED_DIRS.contains(&segment))
}

/// A walk of `root` without the ignored directories and dot-directories.
pub fn walk(root: &Path) -> Vec<String> {
    fn visit(dir: &Path, base: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if out.len() > MAX_FILES {
                return;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !name.starts_with('.') && !DETECT_IGNORED_DIRS.contains(&name.as_str()) {
                    visit(&path, base, out);
                }
            } else if kind.is_file()
                && let Ok(rel) = path.strip_prefix(base)
            {
                out.push(slash(rel));
            }
        }
    }
    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

/// Only an allowlisted, repo-relative path is read. Belt and braces on top of
/// `wanted_files`: no dotenv file but `.env.example`, no absolute or `..` path.
fn readable(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let dotenv = name == ".env" || name.starts_with(".env.");
    let other_dotenv = dotenv && name != ".env.example";
    let escapes = path.starts_with('/')
        || path.contains(':')
        || path.contains('\\')
        || path.split('/').any(|s| s == ".." || s.is_empty());
    !other_dotenv && !escapes
}

/// Reads the wanted files of `files` under `root`, in at most three rounds.
pub fn read_wanted(
    root: &Path,
    files: &[String],
    notes: &mut Vec<String>,
) -> BTreeMap<String, String> {
    let mut contents = BTreeMap::new();
    let mut skipped = std::collections::BTreeSet::new();
    for _ in 0..MAX_ROUNDS {
        let mut new = false;
        for path in wanted_files(files, &contents) {
            if contents.contains_key(&path) || skipped.contains(&path) || !readable(&path) {
                continue;
            }
            let abs = root.join(&path);
            match fs::metadata(&abs) {
                Ok(meta) if meta.len() > MAX_FILE_BYTES => {
                    notes.push(format!("{path} is larger than 256 KiB; not read"));
                    skipped.insert(path);
                }
                Ok(_) => match fs::read_to_string(&abs) {
                    Ok(text) => {
                        contents.insert(path, text);
                        new = true;
                    }
                    Err(_) => {
                        skipped.insert(path);
                    }
                },
                Err(_) => {
                    skipped.insert(path);
                }
            }
        }
        if !new {
            break;
        }
    }
    contents
}

/// The file list and contents detection needs, plus notes about truncation.
pub fn detect_input(ctx: &Context) -> (DetectInput, Vec<String>) {
    let mut notes = Vec::new();
    let mut files = if ctx.in_git {
        git::listed_files(&ctx.root).unwrap_or_else(|_| walk(&ctx.root))
    } else {
        walk(&ctx.root)
    };
    // A subtree entry (`frontend/`, an uninitialized submodule) is not a file.
    files.retain(|f| !ignored(f) && !f.ends_with('/'));
    if files.len() > MAX_FILES {
        files.truncate(MAX_FILES);
        notes.push(format!(
            "more than {MAX_FILES} files; detection used the first {MAX_FILES}"
        ));
    }
    let contents = read_wanted(&ctx.root, &files, &mut notes);
    let repo_name = ctx
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (
        DetectInput {
            files,
            contents,
            repo_name,
        },
        notes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    #[test]
    fn walk_skips_ignored_and_dot_directories() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        for rel in [
            "package.json",
            ".env.example",
            "web/node_modules/x/package.json",
            "web/package.json",
            ".git/config",
            ".idea/x.xml",
            "e2e-playwright/package.json",
            "api/vendor/a/composer.json",
        ] {
            write(root, rel, "{}");
        }
        assert_eq!(
            walk(root),
            vec![".env.example", "package.json", "web/package.json"]
        );
    }

    #[test]
    fn only_wanted_files_are_read_and_never_a_dotenv_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(
            root,
            "api/package.json",
            r#"{"scripts":{"dev":"node src/index.js"},"dependencies":{"express":"4"}}"#,
        );
        write(root, "api/src/index.js", "app.listen(4000);\n");
        write(root, "api/.env", "PORT=9999\n");
        write(root, "api/.env.local", "PORT=9998\n");
        write(root, "api/.env.example", "PORT=4000\n");
        write(root, "api/README.md", "secret\n");
        let mut notes = Vec::new();
        let files = walk(root);
        let contents = read_wanted(root, &files, &mut notes);
        let read: Vec<&str> = contents.keys().map(String::as_str).collect();
        assert_eq!(
            read,
            vec!["api/.env.example", "api/package.json", "api/src/index.js"]
        );
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn readable_refuses_dotenv_and_escaping_paths() {
        assert!(readable("a/.env.example"));
        assert!(readable("package.json"));
        for path in [
            ".env",
            "a/.env",
            "a/.env.local",
            "../x",
            "/etc/passwd",
            "C:/x",
            "a\\b",
        ] {
            assert!(!readable(path), "{path}");
        }
    }

    #[test]
    fn large_files_are_skipped_with_a_note() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        write(root, "package.json", &" ".repeat(300 * 1024));
        let mut notes = Vec::new();
        let contents = read_wanted(root, &walk(root), &mut notes);
        assert!(contents.is_empty());
        assert_eq!(notes, vec!["package.json is larger than 256 KiB; not read"]);
    }
}
