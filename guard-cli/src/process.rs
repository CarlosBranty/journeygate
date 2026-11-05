//! Platform shell, background processes and tool probes.

use std::process::{Command, Stdio};

/// `sh -c <command>` on Unix, `cmd /C <command>` on Windows.
pub fn shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").raw_arg(command);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd
    }
}

/// Puts the child in its own process group so it outlives guard and can be
/// terminated as a tree by [`kill_tree`].
pub fn detach(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
}

/// Terminates `pid` and its children; a process that is already gone is fine.
///
/// On Unix the whole process group is signalled only when `pid` still leads
/// its own group and that group is not guard's: a dead app whose PID was
/// reused (for example by the CI step shell running `guard down`) must never
/// turn `kill -TERM -<pid>` into a signal to guard itself.
pub fn kill_tree(pid: u32) {
    let quiet = |cmd: &mut Command| {
        let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status();
    };
    if cfg!(windows) {
        quiet(Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]));
    } else {
        if signal_group(pid, process_group(pid), process_group(std::process::id())) {
            quiet(Command::new("kill").args(["-TERM", "--", &format!("-{pid}")]));
        }
        if pid != std::process::id() {
            quiet(Command::new("kill").args(["-TERM", &pid.to_string()]));
        }
    }
}

/// Whether [`kill_tree`] may signal the group `pid` leads: only when `pid` is
/// its group's leader and guard's own group is known and different.
fn signal_group(pid: u32, target_group: Option<u32>, own_group: Option<u32>) -> bool {
    target_group == Some(pid) && own_group.is_some_and(|own| own != pid)
}

/// The process group of `pid`, or `None` when it is gone or `ps` is missing.
fn process_group(pid: u32) -> Option<u32> {
    if cfg!(windows) {
        return None;
    }
    probe("ps", &["-o", "pgid=", "-p", &pid.to_string()])?
        .trim()
        .parse()
        .ok()
}

/// An opaque fingerprint of the running process `pid` (its start time), or
/// `None` when it is gone or the platform tool is unavailable. Two different
/// processes that reuse one PID get different fingerprints.
pub fn start_identity(pid: u32) -> Option<String> {
    let found = if cfg!(windows) {
        let script =
            format!("(Get-Process -Id {pid} -ErrorAction Stop).StartTime.ToUniversalTime().Ticks");
        probe(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", &script],
        )
    } else {
        probe("ps", &["-o", "lstart=", "-p", &pid.to_string()])
    };
    found.filter(|s| !s.is_empty())
}

/// Whether `guard down` may terminate a recorded PID. Without a recorded
/// identity (it could not be read at `guard up`) the PID is trusted; with
/// one, the process must still be the one `guard up` started.
pub fn same_process(recorded: Option<&str>, current: Option<&str>) -> bool {
    match recorded {
        None => true,
        Some(recorded) => current == Some(recorded),
    }
}

/// Runs `program args` and returns the first stdout line when it succeeds.
pub fn probe(program: impl AsRef<std::ffi::OsStr>, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
    })
}

/// The last `n` lines of `text`.
pub fn tail(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\nc", 2), vec!["b", "c"]);
        assert_eq!(tail("a", 5), vec!["a"]);
        assert!(tail("", 3).is_empty());
    }

    #[test]
    fn same_process_requires_a_matching_identity_when_one_was_recorded() {
        assert!(same_process(None, None));
        assert!(same_process(None, Some("t1")));
        assert!(same_process(Some("t1"), Some("t1")));
        assert!(!same_process(Some("t1"), Some("t2")));
        assert!(!same_process(Some("t1"), None));
    }

    #[test]
    fn start_identity_is_stable_for_a_live_process_and_none_for_a_dead_one() {
        let me = start_identity(std::process::id()).expect("identity of this process");
        assert_eq!(start_identity(std::process::id()), Some(me));
        let pid = {
            let mut child = shell("exit 0").spawn().unwrap();
            child.wait().unwrap();
            child.id()
        };
        assert_eq!(start_identity(pid), None);
    }

    #[test]
    fn only_a_foreign_group_led_by_the_pid_is_signalled_as_a_group() {
        // The app leads its own group, guard is elsewhere: signal the group.
        assert!(signal_group(4242, Some(4242), Some(100)));
        // The PID is not a group leader (already gone, or reused): never signal a group.
        assert!(!signal_group(4242, Some(100), Some(100)));
        assert!(!signal_group(4242, None, Some(100)));
        // The group is guard's own (the PID was reused by guard's step shell): never.
        assert!(!signal_group(4242, Some(4242), Some(4242)));
        // guard's own group unknown: do not risk signalling ourselves.
        assert!(!signal_group(4242, Some(4242), None));
    }

    // Live tests only ever signal processes they spawned themselves. Never
    // pass kill_tree a PID taken from this process's group: on CI runners
    // that group's leader is the runner itself.
    #[cfg(unix)]
    fn wait_gone(child: &mut std::process::Child) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn kill_tree_ends_a_detached_app_and_its_children() {
        // Like `guard up`: a shell leading its own group, with a grandchild.
        let mut cmd = shell("sleep 60 & wait");
        detach(&mut cmd);
        let mut app = cmd.spawn().unwrap();
        assert_eq!(process_group(app.id()), Some(app.id()));
        kill_tree(app.id());
        assert!(wait_gone(&mut app), "the detached app survived kill_tree");
        assert_eq!(process_group(app.id()), None, "its group is gone too");
    }

    #[cfg(unix)]
    #[test]
    fn kill_tree_on_a_pid_in_the_callers_group_signals_only_that_pid() {
        // Not a group leader: only the PID itself may be signalled, never the
        // group it shares with this test (and, on CI, with the runner).
        let mut child = shell("sleep 60").spawn().unwrap();
        assert_ne!(process_group(child.id()), Some(child.id()));
        kill_tree(child.id());
        assert!(wait_gone(&mut child), "the child survived kill_tree");
    }

    #[test]
    fn shell_runs_a_command_and_reports_its_exit_code() {
        let status = shell("exit 3").status().unwrap();
        assert_eq!(status.code(), Some(3));
    }
}
