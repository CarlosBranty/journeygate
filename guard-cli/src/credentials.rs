//! The user credentials file (cli.md, "User credentials file"): where it
//! lives, the startup load into the environment and the atomic private write.
//! Format, masking and the environment precedence: `guard_core::credentials`.

use crate::error::warning;
use guard_core::credentials::{self, Credentials};
use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Keys the startup load exported from the file (for `guard connect --list`).
static FROM_FILE: OnceLock<BTreeSet<String>> = OnceLock::new();

/// `GUARD_CREDENTIALS_FILE`; else on Windows `%APPDATA%\guard\credentials.toml`;
/// else `$XDG_CONFIG_HOME/guard/credentials.toml`; else
/// `~/.config/guard/credentials.toml`. `None` when no base dir is known.
pub fn path_from(lookup: impl Fn(&str) -> Option<String>, windows: bool) -> Option<PathBuf> {
    let get = |key: &str| lookup(key).filter(|v| !v.trim().is_empty());
    if let Some(explicit) = get("GUARD_CREDENTIALS_FILE") {
        return Some(PathBuf::from(explicit));
    }
    let file = |base: PathBuf| base.join("guard").join("credentials.toml");
    if windows {
        if let Some(appdata) = get("APPDATA") {
            return Some(file(PathBuf::from(appdata)));
        }
    }
    if let Some(xdg) = get("XDG_CONFIG_HOME").filter(|v| Path::new(v).is_absolute()) {
        return Some(file(PathBuf::from(xdg)));
    }
    let home = get("HOME").or_else(|| windows.then(|| get("USERPROFILE")).flatten())?;
    Some(file(PathBuf::from(home).join(".config")))
}

/// The credentials file of this process.
pub fn path() -> Option<PathBuf> {
    path_from(|key| std::env::var(key).ok(), cfg!(windows))
}

/// Startup: exports every known key of the file whose variable is not set
/// yet. Problems are warnings; the command runs anyway.
///
/// Must run before any thread is started (it calls `std::env::set_var`).
pub fn load_at_startup() {
    let mut exported = BTreeSet::new();
    if let Some(path) = path() {
        if let Some(found) = read(&path) {
            for (key, value) in
                credentials::to_export(&found, |key| std::env::var_os(key).is_some())
            {
                // SAFETY: called first thing in `main`, before the tokio
                // runtime or any other thread exists, so nothing reads the
                // environment concurrently.
                unsafe { std::env::set_var(&key, value) };
                exported.insert(key);
            }
        }
    }
    let _ = FROM_FILE.set(exported);
}

/// `true` when the startup load set `key` from the file.
pub fn exported_from_file(key: &str) -> bool {
    FROM_FILE.get().is_some_and(|keys| keys.contains(key))
}

/// The file's known keys; `None` (after a warning) when it is unreadable or
/// invalid, and silently when it does not exist.
fn read(path: &Path) -> Option<Credentials> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warning(format!("cannot read {}: {}", path.display(), e.kind()));
            return None;
        }
    };
    match credentials::parse(&text) {
        Ok((found, warnings)) => {
            for message in warnings {
                warning(message);
            }
            Some(found)
        }
        Err(message) => {
            warning(message);
            None
        }
    }
}

/// The current content for an update: empty when the file does not exist;
/// an error (never quoting the file) when it cannot be read or parsed, so a
/// broken file is not silently replaced.
pub fn read_for_update(path: &Path) -> Result<Credentials, String> {
    match fs::read_to_string(path) {
        Ok(text) => credentials::parse(&text).map(|(found, _)| found),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Credentials::new()),
        Err(e) => Err(format!("cannot read {}: {}", path.display(), e.kind())),
    }
}

/// Writes `found` atomically (temp file + rename) with mode `0600` in a
/// `0700` directory on Unix.
pub fn write(path: &Path, found: &Credentials) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_private_dir(dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "credentials.toml".to_string());
    let tmp = dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = private_file(&tmp)?;
        file.write_all(credentials::render(found).as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

#[cfg(unix)]
fn private_file(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn path_prefers_the_override_then_the_platform_dirs() {
        // An absolute dir on the platform running the test.
        let xdg = if cfg!(windows) {
            "C:\\cfg"
        } else {
            "/home/ada/.cfg"
        };
        let file = |base: &str| PathBuf::from(base).join("guard").join("credentials.toml");
        let all = [
            ("GUARD_CREDENTIALS_FILE", "/tmp/c.toml"),
            ("APPDATA", "C:\\Users\\ada\\AppData\\Roaming"),
            ("XDG_CONFIG_HOME", xdg),
            ("HOME", "/home/ada"),
        ];
        assert_eq!(
            path_from(lookup(&all), true),
            Some(PathBuf::from("/tmp/c.toml"))
        );
        assert_eq!(
            path_from(lookup(&all[1..]), true),
            Some(file("C:\\Users\\ada\\AppData\\Roaming"))
        );
        // APPDATA only counts on Windows.
        assert_eq!(path_from(lookup(&all[1..]), false), Some(file(xdg)));
        // A relative XDG_CONFIG_HOME is ignored (XDG base dir spec).
        let relative = [("XDG_CONFIG_HOME", "cfg"), ("HOME", "/home/ada")];
        assert_eq!(
            path_from(lookup(&relative), false),
            Some(
                PathBuf::from("/home/ada")
                    .join(".config")
                    .join("guard")
                    .join("credentials.toml")
            )
        );
        assert_eq!(path_from(lookup(&[]), false), None);
    }

    #[test]
    fn write_then_read_round_trips_and_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("credentials.toml");
        let mut found = Credentials::new();
        found.insert("GITHUB_TOKEN".into(), "ghp_one".into());
        write(&path, &found).unwrap();
        found.insert("GITHUB_TOKEN".into(), "ghp_two".into());
        write(&path, &found).unwrap();
        assert_eq!(read_for_update(&path).unwrap(), found);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            leftovers,
            vec![std::ffi::OsString::from("credentials.toml")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard").join("credentials.toml");
        write(&path, &Credentials::new()).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[test]
    fn a_broken_file_is_not_replaced_and_its_content_is_not_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        fs::write(&path, "GITHUB_TOKEN = ghp_s3cr3t\n").unwrap();
        let error = read_for_update(&path).unwrap_err();
        assert!(!error.contains("s3cr3t"), "{error}");
        assert!(
            read_for_update(&dir.path().join("missing.toml"))
                .unwrap()
                .is_empty()
        );
    }
}
