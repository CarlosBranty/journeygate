//! The few git queries guard needs, run through the `git` binary.

pub mod base;
pub mod changes;
pub mod history;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Upper bound for one git call; `worktree add` of a large repo is the slowest.
const GIT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitError {
    /// `git` is not on PATH.
    Missing,
    /// git ran and failed.
    Failed(String),
    /// git did not finish in time and was terminated.
    TimedOut(Duration),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Missing => write!(f, "git is not on PATH"),
            GitError::Failed(stderr) => write!(f, "{stderr}"),
            GitError::TimedOut(after) => write!(f, "git timed out after {}s", after.as_secs()),
        }
    }
}

/// A non-interactive `git`: stdin is null, no terminal or credential prompt,
/// no pager, no optional index lock, and no fsmonitor daemon (the Git for
/// Windows system config enables `core.fsmonitor`, which starts a long-lived
/// background git per repository and worktree guard touches).
fn git_command(dir: Option<&Path>) -> Command {
    crate::app::seal_std_handles();
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-pager",
        "-c",
        "core.quotepath=off",
        "-c",
        "core.fsmonitor=false",
    ])
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GCM_INTERACTIVE", "never")
    .env("GIT_PAGER", "cat")
    .env("PAGER", "cat")
    .env("GIT_OPTIONAL_LOCKS", "0")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    cmd
}

/// Runs `cmd`; past `timeout` its process tree is terminated.
fn finish(mut cmd: Command, timeout: Duration) -> Result<std::process::Output, GitError> {
    let mut child = cmd.spawn().map_err(|_| GitError::Missing)?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                crate::process::kill_tree(child.id());
                let _ = child.kill();
                let _ = child.wait();
                // The readers end once the killed tree closes its pipes.
                return Err(GitError::TimedOut(timeout));
            }
        }
    };
    Ok(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
thread_local! {
    static SPAWNS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many git processes this thread has started (tests only).
#[cfg(test)]
pub(crate) fn spawn_count() -> usize {
    SPAWNS.with(|c| c.get())
}

fn run_with(dir: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>, GitError> {
    run_with_env(dir, args, &[], timeout)
}

/// [`run_with`] with extra environment variables for this one call.
fn run_with_env(
    dir: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    timeout: Duration,
) -> Result<Vec<u8>, GitError> {
    #[cfg(test)]
    SPAWNS.with(|c| c.set(c.get() + 1));
    let mut cmd = git_command(Some(dir));
    cmd.args(args).envs(env.iter().copied());
    let output = finish(cmd, timeout)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(GitError::Failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    run_with(dir, args, GIT_TIMEOUT)
}

/// `git <args>` run in `dir` (a submodule, a nested repository): the same
/// non-interactive git, `core.quotepath=off` included.
pub fn git_in(dir: &Path, args: &[&str]) -> Result<Vec<u8>, GitError> {
    run(dir, args)
}

fn text(dir: &Path, args: &[&str]) -> Result<String, GitError> {
    run(dir, args).map(|out| String::from_utf8_lossy(&out).trim().to_string())
}

/// Splits `-z` output.
fn nul_list(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

pub fn version() -> Option<String> {
    let mut cmd = git_command(None);
    cmd.arg("--version");
    let out = finish(cmd, Duration::from_secs(30)).ok()?;
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    out.status.success().then(|| {
        line.strip_prefix("git version ")
            .unwrap_or(&line)
            .to_string()
    })
}

pub fn toplevel(cwd: &Path) -> Option<PathBuf> {
    text(cwd, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(PathBuf::from)
}

/// Tracked files, initialized submodules expanded (cli.md "Changed files").
pub fn tracked_files(root: &Path) -> Result<Vec<String>, GitError> {
    changes::tracked_paths(root).map(|set| set.paths)
}

/// Tracked plus untracked, not ignored, files (`guard init` detection), at
/// every repository level.
pub fn listed_files(root: &Path) -> Result<Vec<String>, GitError> {
    changes::listed_paths(root).map(|set| set.paths)
}

/// Prints each walker warning once on stderr.
pub fn print_warnings(set: &changes::PathSet) {
    for warning in &set.warnings {
        crate::error::warning(warning);
    }
}

/// The hooks directory (`git rev-parse --git-path hooks`), absolute.
pub fn hooks_dir(root: &Path) -> Result<PathBuf, GitError> {
    let path = PathBuf::from(text(root, &["rev-parse", "--git-path", "hooks"])?);
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

/// Full sha of the commit `reference` names, or `None` when it does not exist.
pub fn resolve_commit(root: &Path, reference: &str) -> Result<Option<String>, GitError> {
    let spec = format!("{reference}^{{commit}}");
    match text(root, &["rev-parse", "--verify", "--quiet", &spec]) {
        Ok(sha) => Ok(Some(sha)),
        Err(GitError::Failed(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn merge_base(root: &Path, base: &str) -> Result<String, GitError> {
    text(root, &["merge-base", base, "HEAD"])
}

/// `None` for a detached HEAD; an unborn branch has its name.
pub fn current_branch(root: &Path) -> Result<Option<String>, GitError> {
    match text(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(branch) => Ok((!branch.is_empty()).then_some(branch)),
        // Exit 1 without output: HEAD is detached.
        Err(GitError::Failed(stderr)) if stderr.is_empty() => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether HEAD names a commit (`false` in a repository with no commits).
pub fn has_commits(root: &Path) -> bool {
    resolve_commit(root, "HEAD").is_ok_and(|sha| sha.is_some())
}

/// `git rev-parse --is-shallow-repository` is `true`.
pub fn is_shallow(root: &Path) -> bool {
    text(root, &["rev-parse", "--is-shallow-repository"]).is_ok_and(|out| out == "true")
}

/// The default branch for "commits since": the default base (cli.md "Base
/// resolution" steps 4-6), else the current branch, else `HEAD`.
pub fn default_branch(root: &Path) -> String {
    if let Some(base) = base::detect_repo_default(root) {
        return base.reference;
    }
    current_branch(root)
        .ok()
        .flatten()
        .unwrap_or_else(|| "HEAD".into())
}

/// `git diff --no-renames <base>` (committed + working tree) as text, plain
/// whatever the user config says (`color.ui`, `diff.external`, textconv).
/// Submodule changes appear as prefixed `diff --git a/<sub>/...` entries.
pub fn diff_text(root: &Path, base: &str) -> Result<String, GitError> {
    run(
        root,
        &[
            "diff",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--submodule=diff",
            "--ignore-submodules=none",
            base,
        ],
    )
    .map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// `git rev-parse HEAD`.
pub fn head_sha(root: &Path) -> Result<String, GitError> {
    text(root, &["rev-parse", "HEAD"])
}

/// `git merge-base <a> <b>`.
pub fn merge_base_of(root: &Path, a: &str, b: &str) -> Result<String, GitError> {
    text(root, &["merge-base", a, b])
}

/// Committed changes since the merge base of `base` and HEAD, submodule
/// pointer changes expanded (cli.md "Changed files", step 1).
pub fn committed_changes(root: &Path, base: &str) -> Result<Vec<String>, GitError> {
    let from = merge_base(root, base)?;
    let set = changes::changed_paths(root, &from, changes::Scope::Committed)?;
    print_warnings(&set);
    Ok(set.paths)
}

/// `git show <rev>:<path>`; `None` when the file does not exist at `rev`.
pub fn show_file(root: &Path, rev: &str, path: &str) -> Option<String> {
    run(root, &["show", &format!("{rev}:{path}")])
        .ok()
        .map(|out| String::from_utf8_lossy(&out).into_owned())
}

/// `git worktree add --detach <dir> <rev>`.
pub fn worktree_add(root: &Path, dir: &Path, rev: &str) -> Result<(), GitError> {
    let dir = dir.to_string_lossy();
    run(root, &["worktree", "add", "--detach", &dir, rev]).map(|_| ())
}

/// `git worktree remove --force --force <dir>` (also when it holds
/// submodules or is locked), errors ignored (plus `prune`).
pub fn worktree_remove(root: &Path, dir: &Path) {
    let dir = dir.to_string_lossy();
    let _ = run(root, &["worktree", "remove", "--force", "--force", &dir]);
    let _ = run(root, &["worktree", "prune"]);
}

pub fn origin_url(root: &Path) -> Option<String> {
    text(root, &["remote", "get-url", "origin"]).ok()
}

/// Local branches, then remote-tracking branches.
pub fn branches(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for refs in ["refs/heads", "refs/remotes"] {
        if let Ok(out) = text(root, &["for-each-ref", "--format=%(refname:short)", refs]) {
            names.extend(out.lines().map(str::to_string));
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_repo() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        run(dir.path(), &["init", "-q"]).unwrap();
        dir
    }

    #[test]
    fn default_branch_uses_detection() {
        let dir = scratch_repo();
        let root = dir.path();
        run(root, &["checkout", "-q", "-b", "feature"]).unwrap();
        run(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        )
        .unwrap();
        run(root, &["config", "init.defaultBranch", "main"]).unwrap();
        // No candidate: the current branch.
        assert_eq!(default_branch(root), "feature");
        // A dangling origin/HEAD is skipped; a non-origin sole remote counts.
        run(
            root,
            &["remote", "add", "upstream", "https://example.invalid/u.git"],
        )
        .unwrap();
        run(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/upstream/HEAD",
                "refs/remotes/upstream/gone",
            ],
        )
        .unwrap();
        assert_eq!(default_branch(root), "feature");
        run(
            root,
            &["update-ref", "refs/remotes/upstream/master", "HEAD"],
        )
        .unwrap();
        assert_eq!(default_branch(root), "upstream/master");
    }

    #[test]
    fn the_default_branch_is_origin_head_else_the_current_branch() {
        let dir = scratch_repo();
        let root = dir.path();
        run(root, &["checkout", "-q", "-b", "trunk"]).unwrap();
        run(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        )
        .unwrap();
        assert_eq!(default_branch(root), "trunk");
        run(
            root,
            &["remote", "add", "origin", "https://example.invalid/o.git"],
        )
        .unwrap();
        run(root, &["update-ref", "refs/remotes/origin/main", "HEAD"]).unwrap();
        run(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .unwrap();
        assert_eq!(default_branch(root), "origin/main");
        let bare = tempfile::TempDir::new().unwrap();
        assert_eq!(default_branch(bare.path()), "HEAD");
    }

    #[test]
    fn git_never_prompts_pages_or_starts_an_fsmonitor_daemon() {
        let dir = scratch_repo();
        let root = dir.path();
        let get = |args: &[&str]| text(root, args).unwrap();
        assert_eq!(get(&["config", "core.fsmonitor"]), "false");
        assert_eq!(get(&["var", "GIT_PAGER"]), "cat");
        assert_eq!(
            get(&[
                "-c",
                "alias.prompt=!echo \"$GIT_TERMINAL_PROMPT\"",
                "prompt"
            ]),
            "0"
        );
        // stdin is null: a git that reads it sees EOF at once (the empty blob).
        assert_eq!(
            get(&["hash-object", "--stdin"]),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn a_stuck_git_is_terminated_at_its_deadline() {
        let dir = scratch_repo();
        let begin = Instant::now();
        let result = run_with(
            dir.path(),
            &["-c", "alias.stuck=!sleep 30", "stuck"],
            Duration::from_millis(500),
        );
        assert_eq!(result, Err(GitError::TimedOut(Duration::from_millis(500))));
        assert!(
            begin.elapsed() < Duration::from_secs(10),
            "{:?}",
            begin.elapsed()
        );
    }

    #[test]
    fn git_failures_carry_stderr() {
        let dir = scratch_repo();
        match run(dir.path(), &["show", "nope"]) {
            Err(GitError::Failed(stderr)) => assert!(stderr.contains("nope"), "{stderr}"),
            other => panic!("{other:?}"),
        }
        assert!(version().is_some());
    }

    #[test]
    fn nul_lists_skip_empty_entries() {
        assert_eq!(nul_list(b"a\0b c\0\0"), vec!["a", "b c"]);
        assert!(nul_list(b"").is_empty());
    }
}
