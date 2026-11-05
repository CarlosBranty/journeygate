//! The exact `guard` argv for every action of the GUI (cli.md).

pub use crate::export::ExportFormat;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(test)]
mod tests;

/// The default of `guard up --timeout` (cli.md).
pub const DEFAULT_UP_TIMEOUT_SECS: u32 = 120;

/// How `guard test` drives the browser (`--headed` / `--ui`, cli.md).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunMode {
    /// No flag: Playwright's default.
    #[default]
    Headless,
    /// `--headed`: the browser window is visible while the tests run.
    Headed,
    /// `--ui`: Playwright's interactive UI mode.
    Ui,
}

impl RunMode {
    pub const ALL: [Self; 3] = [Self::Headless, Self::Headed, Self::Ui];

    /// The `guard test` flag, if any.
    pub fn flag(self) -> Option<&'static str> {
        match self {
            Self::Headless => None,
            Self::Headed => Some("--headed"),
            Self::Ui => Some("--ui"),
        }
    }

    /// The selector's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Headless => "Headless",
            Self::Headed => "Watch on screen (--headed)",
            Self::Ui => "Playwright UI mode (--ui)",
        }
    }

    /// The toolbar combo's closed text.
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Headless => "Headless",
            Self::Headed => "Headed",
            Self::Ui => "UI mode",
        }
    }

    /// Playwright waits for the user: the run's progress counters may not
    /// move, and guard prints no report summary.
    pub fn interactive(self) -> bool {
        self == Self::Ui
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardCommand {
    /// `guard status --json`
    Status,
    /// `guard status --integrations --json`
    Integrations,
    /// `guard affected --json`
    Affected,
    /// `guard up --timeout <n>`
    Up { timeout_secs: u32 },
    /// `guard down`
    Down,
    /// `guard test --affected [--headed | --ui]`
    TestAffected(RunMode),
    /// `guard test --all [--headed | --ui]`
    TestAll(RunMode),
    /// `guard test --journey <id>... [--headed | --ui]`; never empty (see
    /// [`Self::test_journeys`]).
    TestJourneys(Vec<String>, RunMode),
    /// `guard report`
    Report,
    /// `guard explain --json [--journey <id>]`
    Explain { journey: Option<String> },
    /// `guard explain --format <md|xlsx> --out <path> [--journey <id>]`
    ExplainExport {
        journey: Option<String>,
        format: ExportFormat,
        out: PathBuf,
    },
    /// `guard explain --list-models --json`
    ListModels,
    /// `guard suspects --json`
    Suspects,
}

impl GuardCommand {
    /// `guard test --journey <id>` for each id, or `None` without ids.
    pub fn test_journeys<I, S>(ids: I, mode: RunMode) -> Option<Self>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let ids: Vec<String> = ids.into_iter().map(Into::into).collect();
        (!ids.is_empty()).then_some(Self::TestJourneys(ids, mode))
    }

    /// The arguments after the program name.
    pub fn args(&self) -> Vec<String> {
        let words: &[&str] = match self {
            Self::Status => &["status", "--json"],
            Self::Integrations => &["status", "--integrations", "--json"],
            Self::Affected => &["affected", "--json"],
            Self::Down => &["down"],
            Self::Report => &["report"],
            Self::ListModels => &["explain", "--list-models", "--json"],
            Self::Suspects => &["suspects", "--json"],
            Self::Up { timeout_secs } => {
                return vec!["up".into(), "--timeout".into(), timeout_secs.to_string()];
            }
            Self::TestAffected(mode) => return with_mode(&["test", "--affected"], *mode),
            Self::TestAll(mode) => return with_mode(&["test", "--all"], *mode),
            Self::TestJourneys(ids, mode) => {
                let mut args = vec!["test".to_string()];
                for id in ids {
                    args.push("--journey".into());
                    args.push(id.clone());
                }
                args.extend(mode.flag().map(String::from));
                return args;
            }
            Self::ExplainExport {
                journey,
                format,
                out,
            } => {
                let mut args: Vec<String> = ["explain", "--format", format.cli_name(), "--out"]
                    .map(String::from)
                    .into();
                args.push(out.display().to_string());
                if let Some(id) = journey {
                    args.push("--journey".into());
                    args.push(id.clone());
                }
                return args;
            }
            Self::Explain { journey } => {
                let mut args = vec!["explain".to_string(), "--json".to_string()];
                if let Some(id) = journey {
                    args.push("--journey".into());
                    args.push(id.clone());
                }
                return args;
            }
        };
        words.iter().map(|w| (*w).to_string()).collect()
    }

    /// The mode of a `guard test` run; `None` for every other command.
    pub fn run_mode(&self) -> Option<RunMode> {
        match self {
            Self::TestAffected(mode) | Self::TestAll(mode) | Self::TestJourneys(_, mode) => {
                Some(*mode)
            }
            _ => None,
        }
    }

    /// A `guard test` run (any selection, any mode).
    pub fn is_test(&self) -> bool {
        self.run_mode().is_some()
    }

    /// `guard <args>`, for the log header.
    pub fn display(&self) -> String {
        format!("guard {}", self.args().join(" "))
    }
}

/// `words` plus the mode's flag.
fn with_mode(words: &[&str], mode: RunMode) -> Vec<String> {
    words
        .iter()
        .copied()
        .chain(mode.flag())
        .map(String::from)
        .collect()
}
