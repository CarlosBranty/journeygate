//! `thresholds.toml` location and loading (thresholds-toml.md, "Location").

use crate::context::Context;
use crate::error::{CliError, CliResult};
use guard_core::thresholds::{Thresholds, parse_thresholds};
use std::path::{Path, PathBuf};

/// The variable that names a `thresholds.toml` path.
pub const THRESHOLDS_ENV: &str = "GUARD_THRESHOLDS";

/// First match wins: `flag`, `GUARD_THRESHOLDS`, `<root>/thresholds.toml`.
/// `None` when nothing exists (every default applies).
pub fn thresholds_path(root: &Path, flag: Option<&Path>, env: Option<String>) -> Option<PathBuf> {
    if let Some(path) = flag {
        return Some(path.to_path_buf());
    }
    if let Some(path) = env.filter(|v| !v.trim().is_empty()) {
        return Some(PathBuf::from(path));
    }
    let default = root.join("thresholds.toml");
    default.exists().then_some(default)
}

/// Loads the thresholds; a file that exists but is invalid is exit 2, an
/// explicitly named file that is missing is exit 2 too.
pub fn load_thresholds(ctx: &Context, flag: Option<&Path>) -> CliResult<Thresholds> {
    let Some(path) = thresholds_path(&ctx.root, flag, std::env::var(THRESHOLDS_ENV).ok()) else {
        return Ok(Thresholds::default());
    };
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CliError::usage(format!("cannot read {}: {e}", path.display())))?;
    parse_thresholds(&text).map_err(|e| CliError::usage(format!("invalid {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn location_order_is_flag_then_env_then_repo_root() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        assert_eq!(thresholds_path(root, None, None), None);
        std::fs::write(root.join("thresholds.toml"), "").unwrap();
        assert_eq!(
            thresholds_path(root, None, None),
            Some(root.join("thresholds.toml"))
        );
        assert_eq!(
            thresholds_path(root, None, Some("/e.toml".into())),
            Some(PathBuf::from("/e.toml"))
        );
        assert_eq!(
            thresholds_path(root, Some(Path::new("/f.toml")), Some("/e.toml".into())),
            Some(PathBuf::from("/f.toml"))
        );
        assert_eq!(
            thresholds_path(root, None, Some("  ".into())),
            Some(root.join("thresholds.toml"))
        );
    }
}
