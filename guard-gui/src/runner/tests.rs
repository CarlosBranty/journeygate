use super::*;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A shell running `script`: the runner is generic over the program, so these
/// tests never need the guard binary or any real service.
fn shell(script: &str) -> (PathBuf, Vec<String>) {
    if cfg!(windows) {
        ("cmd".into(), vec!["/C".into(), script.into()])
    } else {
        ("sh".into(), vec!["-c".into(), script.into()])
    }
}

fn collect(run: &Run, limit: Duration) -> (Vec<String>, Option<Exit>) {
    let deadline = Instant::now() + limit;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        match run.events().recv_timeout(Duration::from_millis(50)) {
            Ok(RunEvent::Line(line)) => lines.push(line),
            Ok(RunEvent::Exited(exit)) => return (lines, Some(exit)),
            Err(_) => {}
        }
    }
    (lines, None)
}

#[test]
fn streams_stdout_and_stderr_lines_and_reports_the_exit_code() {
    let (program, args) = shell("echo out-line && echo err-line 1>&2 && exit 3");
    let run = Run::spawn(&program, &args, &std::env::temp_dir(), || {}).unwrap();
    let (lines, exit) = collect(&run, Duration::from_secs(20));
    let lines: Vec<String> = lines.iter().map(|l| l.trim().to_string()).collect();
    assert!(lines.contains(&"out-line".to_string()), "{lines:?}");
    assert!(lines.contains(&"err-line".to_string()), "{lines:?}");
    assert_eq!(exit, Some(Exit::Code(3)));
}

#[test]
fn notifies_the_ui_for_new_events() {
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    let (program, args) = shell("echo hi");
    let run = Run::spawn(&program, &args, &std::env::temp_dir(), move || {
        seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })
    .unwrap();
    let (_, exit) = collect(&run, Duration::from_secs(20));
    assert_eq!(exit, Some(Exit::Code(0)));
    // notify() runs right after each send, so the last one may trail the
    // event this thread already received.
    let deadline = Instant::now() + Duration::from_secs(5);
    while count.load(std::sync::atomic::Ordering::SeqCst) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(count.load(std::sync::atomic::Ordering::SeqCst) >= 2);
}

#[test]
fn cancel_kills_a_running_process() {
    let script = if cfg!(windows) {
        "ping -n 30 127.0.0.1 >NUL"
    } else {
        "sleep 30"
    };
    let (program, args) = shell(script);
    let run = Run::spawn(&program, &args, &std::env::temp_dir(), || {}).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    run.cancel();
    let started = Instant::now();
    let (_, exit) = collect(&run, Duration::from_secs(20));
    // Not "exit 1": taskkill's exit code is not the run's result.
    assert_eq!(exit, Some(Exit::Cancelled), "the process survived cancel");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_missing_program_fails_to_spawn() {
    let err = Run::spawn(
        std::path::Path::new("definitely-not-a-program-xyz"),
        &[],
        &std::env::temp_dir(),
        || {},
    );
    assert!(err.is_err());
}

#[test]
fn capture_returns_code_and_both_streams() {
    let (program, args) = shell("echo captured && exit 2");
    let out = capture(
        &program,
        &args,
        &std::env::temp_dir(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(out.code, Some(2));
    assert!(out.stdout.contains("captured"));
}

#[test]
fn capture_with_env_reaches_the_child_through_its_environment_only() {
    let script = if cfg!(windows) {
        "echo value=%GUARD_GUI_TEST_SECRET%"
    } else {
        "echo value=$GUARD_GUI_TEST_SECRET"
    };
    let (program, args) = shell(script);
    let env = vec![("GUARD_GUI_TEST_SECRET".to_string(), "dummy-123".to_string())];
    let out = capture_with_env(
        &program,
        &args,
        &env,
        &std::env::temp_dir(),
        Duration::from_secs(20),
    )
    .unwrap();
    assert!(out.stdout.contains("value=dummy-123"), "{}", out.stdout);
    // The value was never part of the argv.
    assert!(args.iter().all(|a| !a.contains("dummy-123")));
}

#[test]
fn capture_kills_a_command_past_its_timeout() {
    let script = if cfg!(windows) {
        "ping -n 30 127.0.0.1 >NUL"
    } else {
        "sleep 30"
    };
    let (program, args) = shell(script);
    let started = Instant::now();
    let out = capture(
        &program,
        &args,
        &std::env::temp_dir(),
        Duration::from_millis(500),
    );
    assert!(matches!(out, Err(CaptureError::TimedOut(_))), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[test]
fn a_timeout_says_how_long_it_waited() {
    let e = CaptureError::TimedOut(Duration::from_secs(60));
    assert_eq!(e.to_string(), "timed out after 60 s and was stopped");
}

#[test]
fn exit_describes_itself() {
    assert_eq!(Exit::Code(0).to_string(), "exit 0");
    assert_eq!(Exit::Killed.to_string(), "killed");
    assert_eq!(Exit::Cancelled.to_string(), "cancelled");
    assert_eq!(Exit::Cancelled.label(), "Cancelled");
    assert_eq!(Exit::Code(2).label(), "Exit 2");
    assert!(Exit::Code(0).success());
    assert!(!Exit::Cancelled.success());
}

#[test]
fn a_cancelled_run_is_cancelled_whatever_its_exit_code() {
    assert_eq!(classify(Some(1), true), Exit::Cancelled);
    assert_eq!(classify(None, true), Exit::Cancelled);
    assert_eq!(classify(Some(1), false), Exit::Code(1));
    assert_eq!(classify(None, false), Exit::Killed);
}
