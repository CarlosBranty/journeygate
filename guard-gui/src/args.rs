//! Command-line options of `guard-gui`: `--repo <path>`, plus the hidden
//! screenshot harness used to review the window
//! (`--screenshot <png> [--view tests|run|flow|ai-settings|problems] [--size WxH] [--flow <state>]`).

use crate::flow::ShotSpec;
use std::path::PathBuf;

#[cfg(test)]
mod tests;

pub const USAGE: &str = "usage: guard-gui [--repo <path>]\n\n  --repo <path>  project to open (a folder with target.toml); default: the most recent project";

/// The default window size of a screenshot.
pub const SHOT_SIZE: [u32; 2] = [1600, 1000];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// The project every guard command runs in; overrides the recent list.
    pub repo: Option<PathBuf>,
    pub screenshot: Option<Screenshot>,
}

/// Render a few frames, save them as a PNG and exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screenshot {
    pub path: PathBuf,
    pub view: ShotView,
    /// Window inner size in points.
    pub size: [u32; 2],
    /// The Flow view's starting state (`--flow`, see [`ShotSpec::parse`]).
    pub flow: ShotSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShotView {
    Tests,
    Run,
    Flow,
    /// The Tests view with the AI settings window open.
    AiSettings,
    /// The Tests view with an "Explain failures" summary in the Problems tab.
    Problems,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgsError {
    #[error("{0} needs a value")]
    MissingValue(&'static str),
    #[error("invalid {0} value: {1}")]
    Invalid(&'static str, String),
    #[error("{0} only works with --screenshot")]
    NeedsScreenshot(&'static str),
    #[error("unknown argument {0}")]
    Unknown(String),
    #[error("{USAGE}")]
    Help,
}

const FLAGS: [&str; 5] = ["--repo", "--screenshot", "--view", "--size", "--flow"];

/// Parses the arguments after the program name.
pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Options, ArgsError> {
    let mut repo = None;
    let mut shot_path = None;
    let mut view = None;
    let mut size = None;
    let mut flow = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "-h" || arg == "--help" {
            return Err(ArgsError::Help);
        }
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) => (flag.to_string(), Some(value.to_string())),
            None => (arg.clone(), None),
        };
        let Some(flag) = FLAGS.into_iter().find(|f| *f == flag) else {
            return Err(ArgsError::Unknown(arg));
        };
        let value = match inline.or_else(|| args.next()) {
            Some(v) if !v.is_empty() => v,
            _ => return Err(ArgsError::MissingValue(flag)),
        };
        match flag {
            "--repo" => repo = Some(PathBuf::from(value)),
            "--screenshot" => shot_path = Some(PathBuf::from(value)),
            "--view" => {
                view = Some(match value.as_str() {
                    "tests" => ShotView::Tests,
                    "run" => ShotView::Run,
                    "flow" => ShotView::Flow,
                    "ai-settings" => ShotView::AiSettings,
                    "problems" => ShotView::Problems,
                    _ => return Err(ArgsError::Invalid(flag, value)),
                })
            }
            "--flow" => match ShotSpec::parse(&value) {
                Ok(spec) => flow = Some(spec),
                Err(_) => return Err(ArgsError::Invalid(flag, value)),
            },
            _ => match parse_size(&value) {
                Some(s) => size = Some(s),
                None => return Err(ArgsError::Invalid(flag, value)),
            },
        }
    }
    let screenshot = match shot_path {
        Some(path) => Some(Screenshot {
            path,
            view: view.unwrap_or(ShotView::Tests),
            size: size.unwrap_or(SHOT_SIZE),
            flow: flow.unwrap_or_default(),
        }),
        None if view.is_some() => return Err(ArgsError::NeedsScreenshot("--view")),
        None if size.is_some() => return Err(ArgsError::NeedsScreenshot("--size")),
        None if flow.is_some() => return Err(ArgsError::NeedsScreenshot("--flow")),
        None => None,
    };
    Ok(Options { repo, screenshot })
}

/// `1600x1000` (or `1600X1000`) as `[1600, 1000]`; both must be positive.
pub fn parse_size(text: &str) -> Option<[u32; 2]> {
    let (w, h) = text.split_once(['x', 'X'])?;
    let w: u32 = w.trim().parse().ok()?;
    let h: u32 = h.trim().parse().ok()?;
    (w > 0 && h > 0).then_some([w, h])
}
