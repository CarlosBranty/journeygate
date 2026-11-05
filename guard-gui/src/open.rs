//! Opening a run attachment with the OS default application.
//!
//! The path comes from report.json, so it is not trusted: it must resolve
//! inside the Playwright dir and have a viewer extension (never an
//! executable), and on Windows it never goes through `cmd.exe`, whose parser
//! would act on `&`, `^`, `%` and friends inside the path.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(test)]
mod tests;

/// Extensions of the files Playwright attaches (screenshot, video, trace,
/// error context, logs). Anything else, executables included, is refused.
pub const OPENABLE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webm", "zip", "txt", "md", "log", "json", "html",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Other,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpenError {
    #[error("not a viewable attachment type (allowed: {})", OPENABLE_EXTENSIONS.join(", "))]
    Extension,
    #[error("not inside {0}")]
    Outside(String),
    #[error("{0}")]
    Io(String),
}

/// `path` checked for opening: a viewer extension and, once symlinks and `..`
/// are resolved, inside `root` (the Playwright dir).
pub fn checked(root: &Path, path: &Path) -> Result<PathBuf, OpenError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !OPENABLE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(OpenError::Extension);
    }
    let real_root = std::fs::canonicalize(root).map_err(|e| OpenError::Io(e.to_string()))?;
    let real = std::fs::canonicalize(path).map_err(|e| OpenError::Io(e.to_string()))?;
    if !real.starts_with(&real_root) {
        return Err(OpenError::Outside(root.display().to_string()));
    }
    Ok(path.to_path_buf())
}

/// Program and argument that open `path`. Windows uses `explorer.exe`, which
/// hands the file to its registered viewer with no shell parsing.
pub fn open_argv(os: Os, path: &Path) -> (&'static str, OsString) {
    let path = path.as_os_str().to_os_string();
    match os {
        Os::Windows => ("explorer.exe", windows_quoted(path)),
        Os::MacOs => ("open", path),
        Os::Other => ("xdg-open", path),
    }
}

/// Always wrapped in double quotes: explorer.exe has its own argument parser
/// (a bare comma splits the path), and a Windows path cannot contain `"`.
fn windows_quoted(path: OsString) -> OsString {
    let mut quoted = OsString::from("\"");
    quoted.push(path);
    quoted.push("\"");
    quoted
}

/// Opens `path` (already [`checked`]) without waiting for the application.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    let (program, arg) = open_argv(Os::current(), path);
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(arg);
    }
    #[cfg(not(windows))]
    cmd.arg(arg);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::runner::no_window(&mut cmd);
    cmd.spawn().map(|_| ())
}
