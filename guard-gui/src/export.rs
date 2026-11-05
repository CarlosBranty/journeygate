//! Where an exported explanation is written (`guard explain --format md|xlsx
//! --out <path>`, cli.md): `<repo>/.guard/explain/explain-<stamp>.<ext>`.

use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// A file format `guard explain --format` writes with `--out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Excel,
}

impl ExportFormat {
    /// The value of `--format`.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Excel => "xlsx",
        }
    }

    /// The extension of the written file.
    pub fn extension(self) -> &'static str {
        self.cli_name()
    }
}

/// `YYYYMMDD-HHMMSS` in UTC, for file names.
pub fn file_stamp(unix_seconds: u64) -> String {
    // `YYYY-MM-DDTHH:MM:SSZ` with the separators taken out.
    let rfc = guard_core::timefmt::rfc3339_utc(unix_seconds);
    let digits: String = rfc.chars().filter(char::is_ascii_digit).collect();
    format!("{}-{}", &digits[..8], &digits[8..])
}

/// The file an export started at `unix_seconds` writes.
pub fn export_path(repo: &Path, format: ExportFormat, unix_seconds: u64) -> PathBuf {
    repo.join(".guard").join("explain").join(format!(
        "explain-{}.{}",
        file_stamp(unix_seconds),
        format.extension()
    ))
}
