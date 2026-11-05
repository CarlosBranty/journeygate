//! `run_playwright_check` against a scripted in-memory provider.

use super::*;
use guard_core::ports::{ExecOutput, ExtractedDiff, SandboxId, Secret};
use std::sync::Mutex;

const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// What the fake answers to the driver exec.
#[derive(Clone)]
enum Driver {
    Ran { pw_exit: i32 },
    AppNotReady,
    Setup,
    TimedOut,
}

struct Fake {
    driver: Driver,
    /// `None`: no report file in the sandbox.
    report: Option<Vec<u8>>,
    stderr: Vec<u8>,
    screenshots: Vec<u8>,
    created: Mutex<Vec<CheckSpec>>,
    execs: Mutex<Vec<ExecRequest>>,
    destroyed: Mutex<u32>,
    fail_create: bool,
}

impl Fake {
    fn new(driver: Driver, report: Option<&str>) -> Self {
        Self {
            driver,
            report: report.map(|r| r.as_bytes().to_vec()),
            stderr: b"Error: no tests found\n".to_vec(),
            screenshots: tar::write(&[]).unwrap(),
            created: Mutex::new(vec![]),
            execs: Mutex::new(vec![]),
            destroyed: Mutex::new(0),
            fail_create: false,
        }
    }

    fn execs(&self) -> Vec<ExecRequest> {
        self.execs.lock().unwrap().clone()
    }
}

fn sandbox() -> Sandbox {
    Sandbox {
        id: SandboxId("guard-sbx-check".into()),
        role: Role::Coder,
        run_id: "run-1".into(),
        ticket: "SHOP-1".into(),
        attempt: 0,
    }
}

#[async_trait::async_trait]
impl SandboxProvider for Fake {
    async fn create(&self, _: &guard_core::ports::SandboxSpec) -> Result<Sandbox, SandboxError> {
        unreachable!("checks use create_check")
    }
    async fn put_file(&self, _: &Sandbox, _: &str, _: Vec<u8>) -> Result<(), SandboxError> {
        unreachable!("checks copy trees in with tar")
    }
    async fn read_file(&self, _: &Sandbox, path: &str) -> Result<Vec<u8>, SandboxError> {
        match path {
            REPORT_PATH => self
                .report
                .clone()
                .ok_or_else(|| SandboxError::NotFound(path.into())),
            STDERR_PATH => Ok(self.stderr.clone()),
            APP_LOG_PATH => Ok(b"demo-shop listening\n".to_vec()),
            _ => Err(SandboxError::NotFound(path.into())),
        }
    }
    async fn exec(&self, _: &Sandbox, r: &ExecRequest) -> Result<ExecOutput, SandboxError> {
        self.execs.lock().unwrap().push(r.clone());
        let ok = |stdout: &[u8]| ExecOutput {
            exit_code: 0,
            stdout: stdout.to_vec(),
            ..ExecOutput::default()
        };
        Ok(match r.argv.first().map(String::as_str) {
            Some("tar") => ok(b""),
            Some("sh") if r.argv.get(3).map(String::as_str) == Some(DRIVER_NAME) => {
                match self.driver {
                    Driver::Ran { pw_exit } => {
                        ok(format!("guard-check: playwright-exit={pw_exit}\n").as_bytes())
                    }
                    Driver::AppNotReady => ExecOutput {
                        exit_code: 90,
                        stdout: b"guard-check: app-not-ready\nError: listen EADDRINUSE\n".to_vec(),
                        ..ExecOutput::default()
                    },
                    Driver::Setup => ExecOutput {
                        exit_code: 91,
                        stderr: b"guard-check: the image has no global @playwright/test".to_vec(),
                        ..ExecOutput::default()
                    },
                    Driver::TimedOut => ExecOutput {
                        exit_code: -1,
                        timed_out: true,
                        ..ExecOutput::default()
                    },
                }
            }
            Some("sh") if r.argv.get(3).map(String::as_str) == Some(COPY_OUT_NAME) => {
                ok(&self.screenshots)
            }
            other => panic!("unexpected exec {other:?}: {:?}", r.argv),
        })
    }
    async fn extract_diff(&self, _: &Sandbox, _: &str) -> Result<ExtractedDiff, SandboxError> {
        unreachable!()
    }
    async fn destroy(&self, _: &Sandbox) -> Result<(), SandboxError> {
        *self.destroyed.lock().unwrap() += 1;
        Ok(())
    }
}

#[async_trait::async_trait]
impl CheckSandboxProvider for Fake {
    async fn create_check(&self, spec: &CheckSpec) -> Result<Sandbox, SandboxError> {
        if self.fail_create {
            return Err(SandboxError::Unavailable("daemon down".into()));
        }
        self.created.lock().unwrap().push(spec.clone());
        Ok(sandbox())
    }
}

fn spec() -> CheckSpec {
    let mut s = CheckSpec::new(
        "run-1",
        "SHOP-1",
        CheckKind::Gate,
        &format!("guard/sandbox@sha256:{DIGEST}"),
    );
    s.secrets
        .insert("GUARD_TEST_TOKEN".into(), Secret::new("tok-123"));
    s
}

fn check(results: Option<std::path::PathBuf>) -> PlaywrightCheck {
    PlaywrightCheck {
        tree: tar::write(&[tar::Entry::File {
            path: "e2e/tests/features/SHOP-1.spec.ts".into(),
            mode: 0o644,
            bytes: b"test()".to_vec(),
        }])
        .unwrap(),
        e2e_dir: "e2e".into(),
        app_start: "node server.mjs".into(),
        ready_path: "/healthz".into(),
        args: vec![
            "tests/features/SHOP-1.spec.ts".into(),
            "--project=chromium".into(),
        ],
        env: [("RUN_ID".to_string(), "r1".to_string())].into(),
        app_ready_timeout: Duration::from_secs(30),
        run_timeout: Duration::from_secs(300),
        results_dir: results,
    }
}

const REPORT: &str = r#"{"suites":[],"errors":[],"stats":{"expected":1}}"#;

#[tokio::test]
async fn a_check_copies_the_tree_in_runs_the_driver_and_reads_the_report() {
    let fake = Fake::new(Driver::Ran { pw_exit: 1 }, Some(REPORT));
    let run = run_playwright_check(&fake, &spec(), &check(None))
        .await
        .unwrap();
    let CheckOutcome::Report { json, timed_out } = &run.outcome else {
        panic!("{:?}", run.outcome)
    };
    assert_eq!(json, REPORT.as_bytes());
    assert!(!timed_out);
    assert_eq!(*fake.destroyed.lock().unwrap(), 1, "always destroyed");

    let execs = fake.execs();
    // 1. The tree goes in through tar on stdin, into the tmpfs workspace.
    assert_eq!(
        execs[0].argv,
        ["tar", "-x", "-f", "-", "-C", CHECK_WORKSPACE]
    );
    assert_eq!(execs[0].stdin.as_deref(), Some(check(None).tree.as_slice()));
    // 2. The driver: the e2e dir and the Playwright args are positional.
    let d = &execs[1];
    assert_eq!(&d.argv[..2], ["sh", "-c"]);
    assert_eq!(d.argv[2], DRIVER_SCRIPT);
    assert_eq!(
        &d.argv[3..],
        [
            DRIVER_NAME,
            "e2e",
            "tests/features/SHOP-1.spec.ts",
            "--project=chromium"
        ]
    );
    assert_eq!(d.env["GUARD_CHECK_APP_START"], "node server.mjs");
    assert_eq!(d.env["GUARD_CHECK_READY_PATH"], "/healthz");
    assert_eq!(d.env["GUARD_CHECK_READY_SECS"], "30");
    assert_eq!(d.env["GUARD_CHECK_RUN_SECS"], "300");
    assert_eq!(d.env["PORT"], CHECK_APP_PORT.to_string());
    assert_eq!(
        d.env["TARGET_BASE_URL"],
        format!("http://127.0.0.1:{CHECK_APP_PORT}")
    );
    assert_eq!(d.env["RUN_ID"], "r1");
    // Secrets travel with the sandbox (forwarded by name), never in the request.
    assert!(!d.env.contains_key("GUARD_TEST_TOKEN"));
    assert!(d.timeout.unwrap() > Duration::from_secs(330));
    let created = fake.created.lock().unwrap();
    assert_eq!(created[0].secrets["GUARD_TEST_TOKEN"].expose(), "tok-123");
}

#[tokio::test]
async fn results_are_copied_out_and_nothing_escapes_the_results_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let results = tmp.path().join("results");
    let mut fake = Fake::new(Driver::Ran { pw_exit: 1 }, Some(REPORT));
    // An escaping entry the container could produce (tar::write refuses it).
    let mut evil = tar::write(&[tar::Entry::File {
        path: "test-results/t-chromium/test-failed-1.png".into(),
        mode: 0o644,
        bytes: b"\x89PNG".to_vec(),
    }])
    .unwrap();
    evil.truncate(evil.len() - 1024);
    let mut escape = tar::write(&[tar::Entry::File {
        path: "test-results/zz.png".into(),
        mode: 0o644,
        bytes: b"x".to_vec(),
    }])
    .unwrap();
    escape[..19].copy_from_slice(b"../../../escape.png");
    // Fix the checksum of the edited header.
    escape[148..156].copy_from_slice(b"        ");
    let sum: u64 = escape[..512].iter().map(|b| u64::from(*b)).sum();
    escape[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    evil.extend(escape);
    fake.screenshots = evil;
    let run = run_playwright_check(&fake, &spec(), &check(Some(results.clone())))
        .await
        .unwrap();
    assert!(matches!(run.outcome, CheckOutcome::Report { .. }));
    assert_eq!(
        std::fs::read(results.join("report.json")).unwrap(),
        REPORT.as_bytes()
    );
    assert!(results.join("app.log").is_file());
    assert!(results.join("playwright.stderr").is_file());
    let shot = results.join("test-results/t-chromium/test-failed-1.png");
    assert_eq!(std::fs::read(&shot).unwrap(), b"\x89PNG");
    assert!(run.results.contains(&shot), "{:?}", run.results);
    assert!(!tmp.path().join("escape.png").exists());
    assert!(
        !results
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("escape.png")
            .exists()
    );
    let copy_out = fake
        .execs()
        .into_iter()
        .find(|e| e.argv.get(3).map(String::as_str) == Some(COPY_OUT_NAME))
        .unwrap();
    assert_eq!(copy_out.argv[4], "e2e");
}

#[tokio::test]
async fn an_app_that_never_answers_is_app_not_ready_with_its_log() {
    let fake = Fake::new(Driver::AppNotReady, None);
    let run = run_playwright_check(&fake, &spec(), &check(None))
        .await
        .unwrap();
    let CheckOutcome::AppNotReady(why) = &run.outcome else {
        panic!("{:?}", run.outcome)
    };
    assert!(why.contains("did not answer /healthz within 30s"), "{why}");
    assert!(why.contains("EADDRINUSE"), "{why}");
    assert_eq!(*fake.destroyed.lock().unwrap(), 1);
}

#[tokio::test]
async fn no_report_is_a_candidate_failure_with_playwright_stderr() {
    let fake = Fake::new(Driver::Ran { pw_exit: 1 }, None);
    let run = run_playwright_check(&fake, &spec(), &check(None))
        .await
        .unwrap();
    let CheckOutcome::NoReport(why) = &run.outcome else {
        panic!("{:?}", run.outcome)
    };
    assert!(why.contains("no JSON report"), "{why}");
    assert!(why.contains("no tests found"), "{why}");
}

#[tokio::test]
async fn a_playwright_timeout_is_reported_with_the_report() {
    for driver in [Driver::Ran { pw_exit: 124 }, Driver::TimedOut] {
        let fake = Fake::new(driver, Some(REPORT));
        let run = run_playwright_check(&fake, &spec(), &check(None))
            .await
            .unwrap();
        assert!(
            matches!(
                run.outcome,
                CheckOutcome::Report {
                    timed_out: true,
                    ..
                }
            ),
            "{:?}",
            run.outcome
        );
    }
}

#[tokio::test]
async fn an_image_without_playwright_or_a_dead_daemon_is_an_infra_error() {
    let fake = Fake::new(Driver::Setup, None);
    let err = run_playwright_check(&fake, &spec(), &check(None))
        .await
        .unwrap_err();
    assert!(matches!(err, SandboxError::Unavailable(_)), "{err:?}");
    assert_eq!(
        *fake.destroyed.lock().unwrap(),
        1,
        "destroyed on errors too"
    );

    let mut fake = Fake::new(Driver::Ran { pw_exit: 0 }, Some(REPORT));
    fake.fail_create = true;
    assert!(
        run_playwright_check(&fake, &spec(), &check(None))
            .await
            .is_err()
    );
    assert_eq!(*fake.destroyed.lock().unwrap(), 0);
}

#[test]
fn check_specs_are_validated() {
    assert!(spec().validate().is_ok());
    let mut s = spec();
    s.image = "guard/sandbox:latest".into();
    assert!(s.validate().unwrap_err().contains("digest"));
    let mut s = spec();
    s.secrets.insert("BAD-NAME".into(), Secret::new("x"));
    assert!(s.validate().is_err());
    let mut s = spec();
    s.workspace_size_mb = 0;
    assert!(s.validate().is_err());
    let mut s = spec();
    s.run_id = "run 1,x".into();
    assert!(s.validate().is_err());
}

#[test]
fn the_driver_never_trusts_a_playwright_package_from_the_candidate_tree() {
    // A tree may carry its own e2e/node_modules/@playwright/test: it is
    // removed and replaced by the image's package before anything runs.
    let rm = DRIVER_SCRIPT
        .find("rm -rf \"$e2e/node_modules/@playwright/test\"")
        .unwrap();
    let ln = DRIVER_SCRIPT.find("ln -s \"$pw\"").unwrap();
    let app = DRIVER_SCRIPT.find("$GUARD_CHECK_APP_START").unwrap();
    assert!(rm < ln && ln < app);
    assert!(DRIVER_SCRIPT.contains("--reporter=json"));
}
