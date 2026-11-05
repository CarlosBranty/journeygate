//! The prompter behind every `guard init` question and report line (cli.md
//! "Prompts"). [`Plain`] keeps the previous line prompts byte for byte;
//! [`Styled`] is the clack look (`cliclack`) and stays thin: every decision
//! lives in the plain functions below, which the tests cover.

use std::io::{self, BufRead, IsTerminal, Write};

/// What the user did with one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Keep the current value.
    Keep,
    /// Replace it.
    Set(String),
    /// Drop an optional item (`-`).
    Drop,
    /// End of input: accept everything left (plain only).
    Eof,
}

/// One line of the detected plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanLine {
    /// A fact (`infra ...`, `service ...`, `base_url ...`).
    Fact(String),
    /// Evidence for the fact above it.
    Evidence(String),
    /// A `note:` line, without the prefix.
    Note(String),
}

pub trait Prompter {
    /// Asks questions at all (`false`: accept everything silently).
    fn interactive(&self) -> bool;
    /// The styled prompter, which also asks the runner and the final confirm.
    fn styled(&self) -> bool;
    fn intro(&mut self, title: &str) -> io::Result<()>;
    fn spinner_start(&mut self, message: &str) -> io::Result<()>;
    fn spinner_stop(&mut self, message: &str) -> io::Result<()>;
    /// Ends the spinner on an error: the report lines held so far are shown
    /// first, so the user sees what was already written.
    fn spinner_fail(&mut self, message: &str) -> io::Result<()>;
    /// The detected plan.
    fn plan(&mut self, lines: &[PlanLine]) -> io::Result<()>;
    /// Report text written as is (it ends with its own newline).
    fn text(&mut self, text: &str) -> io::Result<()>;
    /// One report line.
    fn line(&mut self, line: &str) -> io::Result<()> {
        self.text(&format!("{line}\n"))
    }
    fn warning(&mut self, message: &str) -> io::Result<()>;
    /// An answer that was refused; the item is asked again.
    fn error(&mut self, message: &str) -> io::Result<()>;
    /// Index of the chosen option.
    fn select(&mut self, prompt: &str, options: &[&str], default: usize) -> io::Result<usize>;
    fn input(&mut self, label: &str, current: Option<&str>, optional: bool) -> io::Result<Answer>;
    fn confirm(&mut self, question: &str, default: bool) -> io::Result<bool>;
    fn next_steps(&mut self, steps: &[String]) -> io::Result<()>;
    /// The last word after Ctrl+C / Esc, or after declining to write.
    fn outro_cancel(&mut self, message: &str) -> io::Result<()>;
}

/// What the process is attached to, as the styled prompter needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminals {
    /// stdin delivers key presses: a terminal, and on Windows a real console
    /// input buffer (an msys pty such as plain mintty is a pipe that only
    /// passes `is_terminal`).
    pub stdin_keys: bool,
    pub stdout: bool,
    /// cliclack draws and reads its prompts on stderr.
    pub stderr: bool,
}

impl Terminals {
    /// The current process.
    pub fn detect() -> Self {
        Self {
            stdin_keys: stdin_reads_keys(),
            stdout: io::stdout().is_terminal(),
            stderr: io::stderr().is_terminal(),
        }
    }
}

#[cfg(windows)]
fn stdin_reads_keys() -> bool {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleMode(handle: *mut std::ffi::c_void, mode: *mut u32) -> i32;
    }
    let handle = io::stdin().as_raw_handle();
    let mut mode = 0u32;
    // SAFETY: GetConsoleMode only reads the handle and writes `mode`; a null,
    // invalid or non-console handle makes it fail, which means "no console".
    !handle.is_null() && unsafe { GetConsoleMode(handle.cast(), &mut mode) } != 0
}

#[cfg(not(windows))]
fn stdin_reads_keys() -> bool {
    io::stdin().is_terminal()
}

/// The styled prompter needs key presses on stdin, stdout and stderr on a
/// terminal, no `--yes`, no `NO_COLOR` and no `--dry-run` (which asks nothing).
pub fn use_styled(terminals: Terminals, yes: bool, no_color: bool, dry_run: bool) -> bool {
    terminals.stdin_keys && terminals.stdout && terminals.stderr && !yes && !no_color && !dry_run
}

/// A plain answer line: empty keeps, `-` drops, anything else replaces.
pub fn plain_answer(line: &str) -> Answer {
    match line.trim() {
        "" => Answer::Keep,
        "-" => Answer::Drop,
        other => Answer::Set(other.to_string()),
    }
}

/// A styled answer: the input submits the current value on Enter, so the
/// same value (or nothing) keeps it and `-` drops it.
pub fn styled_answer(text: &str, current: Option<&str>) -> Answer {
    match text.trim() {
        "" => Answer::Keep,
        "-" => Answer::Drop,
        same if Some(same) == current => Answer::Keep,
        other => Answer::Set(other.to_string()),
    }
}

/// The styled input label; optional items say how to drop them.
pub fn styled_label(label: &str, optional: bool) -> String {
    if optional {
        format!("{label}  (Enter keeps, - drops it)")
    } else {
        format!("{label}  (Enter keeps)")
    }
}

/// A yes/no answer: `y`/`yes`, `n`/`no`, anything else is the default.
pub fn yes_no_answer(line: &str, default: bool) -> bool {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

/// The plain `plan:` block.
pub fn plain_plan(lines: &[PlanLine]) -> String {
    let mut out = String::from("plan:\n");
    for line in lines {
        match line {
            PlanLine::Fact(text) => out.push_str(&format!("  {text}\n")),
            PlanLine::Evidence(text) => out.push_str(&format!("{EVIDENCE_INDENT}{text}\n")),
            PlanLine::Note(text) => out.push_str(&format!("note: {text}\n")),
        }
    }
    out
}

const EVIDENCE_INDENT: &str = "               ";

/// The body of the styled `plan` note: the facts only, one per line.
pub fn note_body(lines: &[PlanLine]) -> String {
    lines
        .iter()
        .filter_map(|line| match line {
            PlanLine::Fact(text) => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The plain `next steps:` block.
pub fn plain_next_steps(steps: &[String]) -> String {
    let mut out = String::from("next steps:\n");
    for (i, step) in steps.iter().enumerate() {
        out.push_str(&format!("  {}. {step}\n", i + 1));
    }
    out
}

/// Today's line prompts on any reader and writer.
pub struct Plain<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub interactive: bool,
}

impl Plain<'_> {
    fn read_line(&mut self) -> io::Result<Option<String>> {
        self.out.flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            writeln!(self.out)?;
            return Ok(None);
        }
        Ok(Some(line))
    }
}

impl Prompter for Plain<'_> {
    fn interactive(&self) -> bool {
        self.interactive
    }

    fn styled(&self) -> bool {
        false
    }

    fn intro(&mut self, _: &str) -> io::Result<()> {
        Ok(())
    }

    fn spinner_start(&mut self, _: &str) -> io::Result<()> {
        Ok(())
    }

    fn spinner_stop(&mut self, _: &str) -> io::Result<()> {
        Ok(())
    }

    fn spinner_fail(&mut self, _: &str) -> io::Result<()> {
        // Plain lines are never held: the `error:` line follows them.
        Ok(())
    }

    fn plan(&mut self, lines: &[PlanLine]) -> io::Result<()> {
        self.out.write_all(plain_plan(lines).as_bytes())
    }

    fn text(&mut self, text: &str) -> io::Result<()> {
        self.out.write_all(text.as_bytes())
    }

    fn warning(&mut self, message: &str) -> io::Result<()> {
        // A diagnostic: stderr, after what stdout already has.
        self.out.flush()?;
        crate::error::warning(message);
        Ok(())
    }

    fn error(&mut self, message: &str) -> io::Result<()> {
        writeln!(self.out, "error: {message}")
    }

    fn select(&mut self, _: &str, _: &[&str], default: usize) -> io::Result<usize> {
        // The plain prompter never asks a choice (cli.md "Runner").
        Ok(default)
    }

    fn input(&mut self, label: &str, current: Option<&str>, _: bool) -> io::Result<Answer> {
        write!(self.out, "{label} [{}]: ", current.unwrap_or("none"))?;
        Ok(match self.read_line()? {
            None => Answer::Eof,
            Some(line) => plain_answer(&line),
        })
    }

    fn confirm(&mut self, question: &str, default: bool) -> io::Result<bool> {
        let shown = if default { "[Y/n]" } else { "[y/N]" };
        write!(self.out, "{question} {shown}: ")?;
        Ok(match self.read_line()? {
            None => default,
            Some(line) => yes_no_answer(&line, default),
        })
    }

    fn next_steps(&mut self, steps: &[String]) -> io::Result<()> {
        self.out.write_all(plain_next_steps(steps).as_bytes())
    }

    fn outro_cancel(&mut self, message: &str) -> io::Result<()> {
        writeln!(self.out, "{message}")
    }
}

/// The clack-style prompter (`cliclack`). Report lines written while a
/// spinner runs are held back and shown once it stops.
#[derive(Default)]
pub struct Styled {
    spinner: Option<cliclack::ProgressBar>,
    held: Vec<String>,
}

impl Styled {
    fn show(&mut self, text: &str) -> io::Result<()> {
        if self.spinner.is_some() {
            self.held.push(text.to_string());
            return Ok(());
        }
        let text = text.trim_end_matches('\n');
        if text.is_empty() {
            return Ok(());
        }
        cliclack::log::remark(text)
    }
}

impl Prompter for Styled {
    fn interactive(&self) -> bool {
        true
    }

    fn styled(&self) -> bool {
        true
    }

    fn intro(&mut self, title: &str) -> io::Result<()> {
        cliclack::intro(title)
    }

    fn spinner_start(&mut self, message: &str) -> io::Result<()> {
        let spinner = cliclack::spinner();
        spinner.start(message);
        self.spinner = Some(spinner);
        Ok(())
    }

    fn spinner_stop(&mut self, message: &str) -> io::Result<()> {
        if let Some(spinner) = self.spinner.take() {
            spinner.stop(message);
        }
        for text in std::mem::take(&mut self.held) {
            self.show(&text)?;
        }
        Ok(())
    }

    fn spinner_fail(&mut self, message: &str) -> io::Result<()> {
        if let Some(spinner) = self.spinner.take() {
            spinner.error(message);
        }
        for text in std::mem::take(&mut self.held) {
            self.show(&text)?;
        }
        Ok(())
    }

    fn plan(&mut self, lines: &[PlanLine]) -> io::Result<()> {
        cliclack::note("plan", note_body(lines))?;
        for line in lines {
            if let PlanLine::Note(text) = line {
                cliclack::log::info(text)?;
            }
        }
        Ok(())
    }

    fn text(&mut self, text: &str) -> io::Result<()> {
        self.show(text)
    }

    fn warning(&mut self, message: &str) -> io::Result<()> {
        cliclack::log::warning(message)
    }

    fn error(&mut self, message: &str) -> io::Result<()> {
        cliclack::log::error(message)
    }

    fn select(&mut self, prompt: &str, options: &[&str], default: usize) -> io::Result<usize> {
        let items: Vec<(usize, &str, &str)> = options
            .iter()
            .enumerate()
            .map(|(i, o)| (i, *o, ""))
            .collect();
        cliclack::select(prompt)
            .items(&items)
            .initial_value(default)
            .interact()
    }

    fn input(&mut self, label: &str, current: Option<&str>, optional: bool) -> io::Result<Answer> {
        let mut input = cliclack::input(styled_label(label, optional)).required(false);
        input = match current {
            Some(value) => input.default_input(value),
            None => input.placeholder("none"),
        };
        let text: String = input.interact()?;
        Ok(styled_answer(&text, current))
    }

    fn confirm(&mut self, question: &str, default: bool) -> io::Result<bool> {
        cliclack::confirm(question)
            .initial_value(default)
            .interact()
    }

    fn next_steps(&mut self, steps: &[String]) -> io::Result<()> {
        let body: Vec<String> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect();
        cliclack::note("next steps", body.join("\n"))?;
        cliclack::outro("guard is set up")
    }

    fn outro_cancel(&mut self, message: &str) -> io::Result<()> {
        if let Some(spinner) = self.spinner.take() {
            spinner.cancel(message);
            return Ok(());
        }
        cliclack::outro_cancel(message)
    }
}

/// A scripted styled prompter for the flow tests.
#[cfg(test)]
pub mod scripted {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Debug, Clone)]
    pub enum Step {
        Select(usize),
        Input(&'static str),
        Confirm(bool),
        /// Ctrl+C at this prompt.
        Interrupt,
    }

    #[derive(Default)]
    pub struct Scripted {
        pub steps: VecDeque<Step>,
        /// Everything shown, one entry per call (`<kind>: <text>`).
        pub shown: Vec<String>,
    }

    impl Scripted {
        pub fn new(steps: impl IntoIterator<Item = Step>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                shown: Vec::new(),
            }
        }

        fn next(&mut self, asked: String) -> io::Result<Step> {
            self.shown.push(asked.clone());
            match self.steps.pop_front() {
                Some(Step::Interrupt) => Err(io::ErrorKind::Interrupted.into()),
                Some(step) => Ok(step),
                None => panic!("no scripted answer for {asked}"),
            }
        }

        pub fn shown(&self, prefix: &str) -> Vec<&str> {
            self.shown
                .iter()
                .filter_map(|s| s.strip_prefix(prefix))
                .collect()
        }
    }

    impl Prompter for Scripted {
        fn interactive(&self) -> bool {
            true
        }
        fn styled(&self) -> bool {
            true
        }
        fn intro(&mut self, title: &str) -> io::Result<()> {
            self.shown.push(format!("intro: {title}"));
            Ok(())
        }
        fn spinner_start(&mut self, message: &str) -> io::Result<()> {
            self.shown.push(format!("spinner: {message}"));
            Ok(())
        }
        fn spinner_stop(&mut self, message: &str) -> io::Result<()> {
            self.shown.push(format!("spinner done: {message}"));
            Ok(())
        }
        fn spinner_fail(&mut self, message: &str) -> io::Result<()> {
            self.shown.push(format!("spinner failed: {message}"));
            Ok(())
        }
        fn plan(&mut self, lines: &[PlanLine]) -> io::Result<()> {
            self.shown.push(format!("note: {}", note_body(lines)));
            Ok(())
        }
        fn text(&mut self, text: &str) -> io::Result<()> {
            self.shown.push(format!("line: {}", text.trim_end()));
            Ok(())
        }
        fn warning(&mut self, message: &str) -> io::Result<()> {
            self.shown.push(format!("warning: {message}"));
            Ok(())
        }
        fn error(&mut self, message: &str) -> io::Result<()> {
            self.shown.push(format!("error: {message}"));
            Ok(())
        }
        fn select(&mut self, prompt: &str, options: &[&str], default: usize) -> io::Result<usize> {
            let asked = format!("select: {prompt} {options:?} default={default}");
            match self.next(asked)? {
                Step::Select(i) => Ok(i),
                other => panic!("expected a select answer, got {other:?}"),
            }
        }
        fn input(&mut self, label: &str, current: Option<&str>, _: bool) -> io::Result<Answer> {
            let asked = format!("input: {label} [{}]", current.unwrap_or("none"));
            match self.next(asked)? {
                Step::Input(text) => Ok(styled_answer(text, current)),
                other => panic!("expected an input answer, got {other:?}"),
            }
        }
        fn confirm(&mut self, question: &str, default: bool) -> io::Result<bool> {
            match self.next(format!("confirm: {question} default={default}"))? {
                Step::Confirm(yes) => Ok(yes),
                other => panic!("expected a confirm answer, got {other:?}"),
            }
        }
        fn next_steps(&mut self, steps: &[String]) -> io::Result<()> {
            self.shown.push(format!("outro: {}", steps.join(" | ")));
            Ok(())
        }
        fn outro_cancel(&mut self, message: &str) -> io::Result<()> {
            self.shown.push(format!("cancel: {message}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Terminals = Terminals {
        stdin_keys: true,
        stdout: true,
        stderr: true,
    };

    #[test]
    fn styled_only_on_terminals_without_yes_no_color_or_dry_run() {
        assert!(use_styled(ALL, false, false, false));
        assert!(!use_styled(ALL, true, false, false));
        assert!(!use_styled(ALL, false, true, false));
        assert!(!use_styled(ALL, false, false, true));
    }

    #[test]
    fn styled_needs_keys_on_stdin_and_terminals_on_stdout_and_stderr() {
        // stdin a pipe, or an msys pty that reads no console keys (mintty).
        let no_keys = Terminals {
            stdin_keys: false,
            ..ALL
        };
        assert!(!use_styled(no_keys, false, false, false));
        let no_stdout = Terminals {
            stdout: false,
            ..ALL
        };
        assert!(!use_styled(no_stdout, false, false, false));
        // cliclack draws and reads on stderr: `guard init 2>log`.
        let no_stderr = Terminals {
            stderr: false,
            ..ALL
        };
        assert!(!use_styled(no_stderr, false, false, false));
    }

    #[test]
    fn plain_answers_keep_drop_or_replace() {
        assert_eq!(plain_answer("\n"), Answer::Keep);
        assert_eq!(plain_answer(" - \n"), Answer::Drop);
        assert_eq!(
            plain_answer("npm run dev\n"),
            Answer::Set("npm run dev".into())
        );
    }

    #[test]
    fn styled_answers_treat_the_submitted_default_as_keep() {
        assert_eq!(styled_answer("vite", Some("vite")), Answer::Keep);
        assert_eq!(styled_answer("", None), Answer::Keep);
        assert_eq!(styled_answer("-", Some("vite")), Answer::Drop);
        assert_eq!(
            styled_answer(" bun dev ", Some("vite")),
            Answer::Set("bun dev".into())
        );
    }

    #[test]
    fn styled_labels_show_how_to_drop_optional_items() {
        assert_eq!(styled_label("base_url", false), "base_url  (Enter keeps)");
        assert_eq!(
            styled_label("start command", true),
            "start command  (Enter keeps, - drops it)"
        );
    }

    #[test]
    fn yes_no_falls_back_to_the_default() {
        assert!(yes_no_answer("Y\n", false));
        assert!(!yes_no_answer("no", true));
        assert!(yes_no_answer("\n", true));
        assert!(!yes_no_answer("maybe", false));
    }

    fn lines() -> Vec<PlanLine> {
        vec![
            PlanLine::Fact("service    web  cwd=.".into()),
            PlanLine::Evidence("package.json: vite".into()),
            PlanLine::Fact("languages  typescript".into()),
            PlanLine::Note("skipped docs: no manifest".into()),
        ]
    }

    #[test]
    fn the_plain_plan_keeps_the_previous_block() {
        assert_eq!(
            plain_plan(&lines()),
            "plan:\n  service    web  cwd=.\n               package.json: vite\n  languages  typescript\nnote: skipped docs: no manifest\n"
        );
    }

    #[test]
    fn the_note_shows_only_the_facts() {
        assert_eq!(
            note_body(&lines()),
            "service    web  cwd=.\nlanguages  typescript"
        );
    }

    #[test]
    fn plain_next_steps_are_numbered() {
        assert_eq!(
            plain_next_steps(&["a".into(), "b".into()]),
            "next steps:\n  1. a\n  2. b\n"
        );
    }

    fn plain(answers: &str, interactive: bool) -> (Vec<Answer>, String) {
        let mut input = answers.as_bytes();
        let mut out = Vec::new();
        let mut got = Vec::new();
        {
            let mut p = Plain {
                input: &mut input,
                out: &mut out,
                interactive,
            };
            got.push(p.input("start command", Some("vite"), true).unwrap());
            got.push(p.input("ticket project", None, true).unwrap());
            got.push(p.input("base_url", Some("http://x"), false).unwrap());
        }
        (got, String::from_utf8(out).unwrap())
    }

    #[test]
    fn plain_input_prints_the_previous_prompt() {
        let (got, out) = plain("\n-\n", true);
        assert_eq!(got, vec![Answer::Keep, Answer::Drop, Answer::Eof]);
        assert_eq!(
            out,
            "start command [vite]: ticket project [none]: base_url [http://x]: \n"
        );
    }

    #[test]
    fn plain_confirm_shows_the_default() {
        let mut input = "\n\n".as_bytes();
        let mut out = Vec::new();
        let mut p = Plain {
            input: &mut input,
            out: &mut out,
            interactive: true,
        };
        assert!(!p.confirm("Install dependencies now?", false).unwrap());
        assert!(p.confirm("Write?", true).unwrap());
        assert!(p.confirm("Again?", true).unwrap(), "EOF is the default");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Install dependencies now? [y/N]: Write? [Y/n]: Again? [Y/n]: \n"
        );
    }
}
