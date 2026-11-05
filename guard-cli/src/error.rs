//! Exit codes and the error type every command returns.

pub const EXIT_OK: u8 = 0;
pub const EXIT_FAILED: u8 = 1;
pub const EXIT_USAGE: u8 = 2;
pub const EXIT_ENV: u8 = 3;
/// Ctrl+C / Esc at a styled prompt (cli.md "Exit codes").
pub const EXIT_INTERRUPTED: u8 = 130;

/// A failure that ends the command with `code`, printed to stderr as
/// `error:` lines followed by `hint:` lines.
#[derive(Debug)]
pub struct CliError {
    pub code: u8,
    pub messages: Vec<String>,
    pub hints: Vec<String>,
}

pub type CliResult<T> = Result<T, CliError>;

impl CliError {
    fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            messages: vec![message.into()],
            hints: Vec::new(),
        }
    }

    /// Exit 2: usage or configuration error.
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(EXIT_USAGE, message)
    }

    /// Exit 3: environment or infrastructure error.
    pub fn env(message: impl Into<String>) -> Self {
        Self::new(EXIT_ENV, message)
    }

    /// Exit 130: the user cancelled a prompt. No `error:` line; the prompter
    /// already said so.
    pub fn interrupted() -> Self {
        Self {
            code: EXIT_INTERRUPTED,
            messages: Vec::new(),
            hints: Vec::new(),
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }

    pub fn with_messages(mut self, more: impl IntoIterator<Item = String>) -> Self {
        self.messages.extend(more);
        self
    }

    pub fn report(&self) {
        for message in &self.messages {
            for line in message.lines() {
                eprintln!("error: {line}");
            }
        }
        for hint in &self.hints {
            eprintln!("hint: {hint}");
        }
    }
}

pub fn warning(message: impl AsRef<str>) {
    eprintln!("warning: {}", message.as_ref());
}

pub fn hint(message: impl AsRef<str>) {
    eprintln!("hint: {}", message.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_the_exit_code() {
        assert_eq!(CliError::usage("x").code, EXIT_USAGE);
        assert_eq!(CliError::env("x").code, EXIT_ENV);
        let e = CliError::env("a")
            .with_hint("h")
            .with_messages(["b".to_string()]);
        assert_eq!(e.messages, vec!["a", "b"]);
        assert_eq!(e.hints, vec!["h"]);
    }
}
