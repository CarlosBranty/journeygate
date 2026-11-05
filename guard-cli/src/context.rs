//! Paths every command shares (repo root, target.toml, Playwright dir) and loaders.

use crate::cli::Cli;
use crate::error::{CliError, CliResult};
use crate::git;
use guard_core::catalog::{Catalog, CatalogError, parse_catalog};
use guard_core::config::{ConfigError, TargetConfig, parse_target};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Context {
    /// `git rev-parse --show-toplevel`, else the current dir.
    pub root: PathBuf,
    /// True when `root` came from git.
    pub in_git: bool,
    pub config_path: PathBuf,
    pub e2e_dir: PathBuf,
}

impl Context {
    pub fn resolve(cli: &Cli) -> CliResult<Self> {
        let cwd = std::env::current_dir()
            .map_err(|e| CliError::env(format!("cannot read the current dir: {e}")))?;
        let (root, in_git) = match git::toplevel(&cwd) {
            Some(root) => (root, true),
            None => (cwd.clone(), false),
        };
        let absolute = |p: &PathBuf| {
            if p.is_absolute() {
                p.clone()
            } else {
                cwd.join(p)
            }
        };
        Ok(Self {
            config_path: cli
                .config
                .as_ref()
                .map(absolute)
                .unwrap_or_else(|| root.join("target.toml")),
            e2e_dir: cli
                .e2e_dir
                .as_ref()
                .map(absolute)
                .unwrap_or_else(|| root.join("e2e-playwright")),
            root,
            in_git,
        })
    }

    /// The default layout of a checkout at `root` (`guard serve --repo`,
    /// the baseline worktree).
    pub fn at(root: &Path) -> Self {
        Self {
            in_git: git::toplevel(root).is_some(),
            config_path: root.join("target.toml"),
            e2e_dir: root.join("e2e-playwright"),
            root: root.to_path_buf(),
        }
    }

    pub fn catalog_path(&self) -> PathBuf {
        self.e2e_dir.join("catalog").join("journeys.json")
    }

    pub fn guard_dir(&self) -> PathBuf {
        self.root.join(".guard")
    }

    pub fn durations_path(&self) -> PathBuf {
        self.guard_dir().join("durations.json")
    }

    /// `path` relative to the repo root with forward slashes, for messages.
    pub fn display(&self, path: &Path) -> String {
        let shown = path.strip_prefix(&self.root).unwrap_or(path);
        slash(shown)
    }

    pub fn read_config(&self) -> Result<TargetConfig, Vec<String>> {
        let text = fs::read_to_string(&self.config_path).map_err(|e| {
            vec![format!(
                "cannot read {}: {e}",
                self.display(&self.config_path)
            )]
        })?;
        parse_target(&text).map_err(|e| match e {
            ConfigError::Parse(message) => vec![message],
            ConfigError::Invalid(errors) => errors.iter().map(ToString::to_string).collect(),
        })
    }

    pub fn read_catalog(&self) -> Result<Catalog, Vec<String>> {
        let text = fs::read_to_string(self.catalog_path()).map_err(|e| {
            vec![format!(
                "cannot read {}: {e}",
                self.display(&self.catalog_path())
            )]
        })?;
        parse_catalog(&text).map_err(|e| match e {
            CatalogError::Parse(message) => vec![message],
            CatalogError::Invalid(errors) => errors.iter().map(ToString::to_string).collect(),
        })
    }

    /// Loads target.toml; any problem is exit 2.
    pub fn load_config(&self) -> CliResult<TargetConfig> {
        self.read_config().map_err(|messages| {
            CliError::usage(format!("invalid {}", self.display(&self.config_path)))
                .with_messages(messages)
        })
    }

    /// Loads the catalog and checks it against target.toml; any problem is exit 2.
    pub fn load_catalog(&self, config: &TargetConfig) -> CliResult<Catalog> {
        let catalog = self.read_catalog().map_err(|messages| {
            CliError::usage(format!("invalid {}", self.display(&self.catalog_path())))
                .with_messages(messages)
        })?;
        let issues = guard_core::catalog::check_against_config(&catalog, config);
        if !issues.is_empty() {
            return Err(
                CliError::usage(format!("invalid {}", self.display(&self.catalog_path())))
                    .with_messages(issues.iter().map(ToString::to_string)),
            );
        }
        Ok(catalog)
    }
}

/// Forward-slash rendering of a path (contract: values use `/` even on Windows).
pub fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_uses_forward_slashes() {
        assert_eq!(slash(Path::new("a\\b/c")), "a/b/c");
    }
}
