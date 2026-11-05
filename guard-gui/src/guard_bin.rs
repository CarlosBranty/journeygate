//! Finding the `guard` binary: `GUARD_BIN`, else `PATH`, else next to this
//! executable (a `cargo build --release` puts both in `target/release`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

pub const ENV_VAR: &str = "GUARD_BIN";

pub const EXE_NAME: &str = if cfg!(windows) { "guard.exe" } else { "guard" };

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotFound {
    #[error("GUARD_BIN points at {0}, which is not a file")]
    GuardBin(PathBuf),
    #[error("guard not found: set GUARD_BIN or put guard on PATH")]
    Nowhere,
}

/// The pure lookup; `is_file` is injected so it is testable.
pub fn locate(
    guard_bin: Option<OsString>,
    path_var: Option<OsString>,
    exe_dir: Option<PathBuf>,
    is_file: impl Fn(&Path) -> bool,
) -> Result<PathBuf, NotFound> {
    if let Some(bin) = guard_bin.filter(|b| !b.is_empty()) {
        let bin = PathBuf::from(bin);
        return if is_file(&bin) {
            Ok(bin)
        } else {
            Err(NotFound::GuardBin(bin))
        };
    }
    let path_dirs = path_var
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path_dirs
        .into_iter()
        .chain(exe_dir)
        .map(|dir| dir.join(EXE_NAME))
        .find(|candidate| is_file(candidate))
        .ok_or(NotFound::Nowhere)
}

/// [`locate`] against this process's environment and filesystem.
pub fn from_process() -> Result<PathBuf, NotFound> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    locate(
        std::env::var_os(ENV_VAR),
        std::env::var_os("PATH"),
        exe_dir,
        Path::is_file,
    )
}
