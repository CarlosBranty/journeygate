//! `guard repro <test-id> --pr <n>` (mcp-tools.md, "guard repro"): reruns a
//! remote guard failure locally and compares fingerprints.

use super::test::{Execution, execute};
use crate::cli::ReproArgs;
use crate::context::{Context, slash};
use crate::error::{CliError, CliResult, EXIT_FAILED, EXIT_OK, warning};
use crate::git;
use crate::playwright;
use crate::process;
use crate::runtime::block_on;
use crate::server_client::{ServerClient, ServerError};
use guard_core::fingerprint::{fingerprint, test_id};
use guard_core::ports::{EvidenceFile, EvidenceKind, GateTest, TestStatus};
use guard_core::verdict::{Classification, RunOutcome, classify, interpret_exit};
use guard_mcp::types::{FailureRef, ReproOutput, RunStatus};
use std::fs;
use std::path::{Path, PathBuf};

fn remote_status(status: TestStatus) -> RunStatus {
    match status {
        TestStatus::Failed => RunStatus::Failed,
        TestStatus::Flaky => RunStatus::Flaky,
        TestStatus::Passed | TestStatus::Skipped => RunStatus::Passed,
    }
}

/// Where evidence may be downloaded from: the origins of `[server].url` and
/// of an S3 storage URL, and the root of a `file://` storage URL.
#[derive(Debug, Default)]
struct Sources {
    http_origins: Vec<String>,
    fs_root: Option<PathBuf>,
}

impl Sources {
    fn from_urls(server: Option<&str>, storage: Option<&str>) -> Self {
        let origin = |url: &str| {
            reqwest::Url::parse(url)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .map(|u| u.origin().ascii_serialization())
        };
        let mut sources = Self::default();
        sources.http_origins.extend(server.and_then(origin));
        match storage {
            Some(url) if url.starts_with("s3+http://") || url.starts_with("s3+https://") => {
                if let Ok(s3) = guard_store::S3Config::from_url(url) {
                    sources.http_origins.extend(origin(&s3.endpoint));
                }
            }
            Some(url) if url.starts_with("file://") => {
                sources.fs_root = reqwest::Url::parse(url)
                    .ok()
                    .filter(|u| matches!(u.host_str(), None | Some("") | Some("localhost")))
                    .and_then(|u| u.to_file_path().ok());
            }
            _ => {}
        }
        sources
    }

    fn of(config: &guard_core::config::TargetConfig) -> Self {
        let storage = std::env::var(&config.evidence.storage_url_env)
            .ok()
            .filter(|v| !v.trim().is_empty());
        Self::from_urls(
            config.server.as_ref().map(|s| s.url.as_str()),
            storage.as_deref(),
        )
    }
}

/// An evidence file name is one plain path component: `[A-Za-z0-9._-]+`,
/// not `.`/`..` and not a dotfile (no `.git`).
fn safe_name(name: &str) -> Result<&str, String> {
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if plain {
        Ok(name)
    } else {
        Err(format!("unsafe evidence name {name:?}"))
    }
}

/// The evidence folder: the remote fingerprint when it is 8 lowercase hex,
/// else `unknown` (a stored report is not trusted to name a directory).
fn evidence_folder(fingerprint: Option<&str>) -> String {
    let is_hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    match fingerprint {
        Some(fp) if fp.len() == 8 && fp.chars().all(is_hex) => fp.to_string(),
        _ => "unknown".into(),
    }
}

/// Downloads one evidence file into `dir`: `http(s)://` URLs from an allowed
/// origin (no redirects), and `file://` URLs under the filesystem store root.
async fn download(file: &EvidenceFile, dir: &Path, sources: &Sources) -> Result<(), String> {
    let name = safe_name(&file.name)?;
    let url = file.url.as_deref().ok_or("not uploaded")?;
    let target = dir.join(name);
    if target.parent() != Some(dir) {
        return Err(format!("unsafe evidence name {name:?}"));
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("cannot download {url}: {e}"))?;
    match parsed.scheme() {
        "file" => {
            let root = sources
                .fs_root
                .as_ref()
                .ok_or("file:// evidence needs a file:// storage URL")?;
            let path = parsed
                .to_file_path()
                .map_err(|()| format!("cannot download {url}"))?;
            let root = fs::canonicalize(root).map_err(|e| format!("store root: {e}"))?;
            let path = fs::canonicalize(&path).map_err(|e| format!("copy {url}: {e}"))?;
            if !path.starts_with(&root) {
                return Err(format!("{url} is outside the evidence store root"));
            }
            fs::copy(&path, &target).map_err(|e| format!("copy {url}: {e}"))?;
            Ok(())
        }
        "http" | "https" => {
            let origin = parsed.origin().ascii_serialization();
            if !sources.http_origins.contains(&origin) {
                return Err(format!("{origin} is not an allowed evidence origin"));
            }
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|e| e.to_string())?;
            let response = client.get(parsed).send().await.map_err(|e| e.to_string())?;
            if !response.status().is_success() {
                return Err(format!("GET {name} -> HTTP {}", response.status()));
            }
            let bytes = response.bytes().await.map_err(|e| e.to_string())?;
            fs::write(&target, &bytes).map_err(|e| e.to_string())
        }
        scheme => Err(format!(
            "cannot download {url}: {scheme}: is not an allowed evidence origin"
        )),
    }
}

/// The Playwright arguments for the remote test: its file, which must be a
/// relative path to an existing spec inside the e2e dir, and `--project=`.
/// Both come from a stored report, so neither may look like an option.
fn playwright_args(e2e_dir: &Path, file: &str, project: &str) -> Result<Vec<String>, String> {
    let rel = Path::new(file);
    let plain = !file.is_empty()
        && !file.starts_with('-')
        && rel.is_relative()
        && rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !plain {
        return Err(format!("unsafe test file {file:?}"));
    }
    let root = fs::canonicalize(e2e_dir).map_err(|e| format!("{}: {e}", e2e_dir.display()))?;
    let spec = fs::canonicalize(root.join(rel))
        .map_err(|_| format!("test file {file} does not exist locally"))?;
    if !spec.starts_with(&root) || !spec.is_file() {
        return Err(format!("test file {file} is not inside the e2e dir"));
    }
    let mut args = vec![file.to_string()];
    if !project.is_empty() {
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-');
        if project.starts_with(['-', ' ']) || !project.chars().all(allowed) {
            return Err(format!("unsafe project name {project:?}"));
        }
        args.push(format!("--project={project}"));
    }
    Ok(args)
}

fn docker_available(e2e_dir: &Path) -> bool {
    process::probe("docker", &["info", "--format", "{{.ServerVersion}}"]).is_some()
        && playwright::playwright_version(e2e_dir).is_some()
}

fn server_error(e: ServerError) -> CliError {
    match e {
        ServerError::MissingToken => CliError::usage(e.to_string()),
        ServerError::Status { status: 404, .. } => CliError::usage(format!("pr context: {e}")),
        other => CliError::env(format!("pr context: {other}")),
    }
}

pub fn run(ctx: &Context, args: &ReproArgs) -> CliResult<u8> {
    let config = ctx.load_config()?;
    let catalog = ctx.load_catalog(&config)?;
    let client = ServerClient::from_config(&config)
        .ok_or_else(|| CliError::usage("guard repro needs [server].url in target.toml"))?;
    let context = block_on(client.pr_context(args.pr)).map_err(server_error)?;
    let gate = context
        .latest_gate
        .ok_or_else(|| CliError::usage(format!("no gate result for PR {}", args.pr)))?;
    let remote: GateTest = gate
        .report
        .tests
        .iter()
        .find(|t| t.test_id == args.test_id)
        .cloned()
        .ok_or_else(|| {
            CliError::usage(format!(
                "test {} is not in the latest gate result of PR {}",
                args.test_id, args.pr
            ))
        })?;
    let remote_fp = remote.fingerprint.as_ref().map(|f| f.as_str().to_string());
    let folder = evidence_folder(remote_fp.as_deref());
    let evidence_dir = ctx.guard_dir().join("repro").join(&folder);
    fs::create_dir_all(&evidence_dir)
        .map_err(|e| CliError::env(format!("cannot create {}: {e}", evidence_dir.display())))?;
    let sources = Sources::of(&config);
    if let Some(bundle) = &remote.evidence {
        for file in bundle
            .files
            .iter()
            .filter(|f| matches!(f.kind, EvidenceKind::Screenshot | EvidenceKind::Trace))
        {
            if let Err(e) = block_on(download(file, &evidence_dir, &sources)) {
                warning(format!("evidence {}: {e}", file.name));
            }
        }
    }
    if let Ok(head) = git::head_sha(&ctx.root) {
        if !gate.report.head_sha.is_empty() && head != gate.report.head_sha {
            warning(format!(
                "HEAD {head} differs from the gate's head {}",
                gate.report.head_sha
            ));
        }
    }

    let docker = docker_available(&ctx.e2e_dir);
    if !docker {
        warning("docker is not available; reproducing with the local browsers");
    }
    let extra_args = playwright_args(&ctx.e2e_dir, &remote.file, &remote.project)
        .map_err(|e| CliError::usage(format!("remote test {}: {e}", remote.test_id)))?;
    let e2e_tags = match remote
        .journey
        .as_deref()
        .filter(|j| catalog.find(j).is_some())
    {
        Some(journey) => guard_core::catalog::journey_tag(journey),
        None => String::new(),
    };
    let run = Execution {
        e2e_tags,
        extra_args,
        docker,
        interactive: false,
        run_id: format!("repro-{folder}"),
    };
    let (exit, report) = execute(ctx, &config, &run)?;
    if let RunOutcome::Infrastructure(reason) = interpret_exit(exit, report.as_ref()) {
        return Err(CliError::env(reason));
    }
    let report = report.expect("interpret_exit requires a report");
    let local = classify(&report)
        .into_iter()
        .find(|t| test_id(&t.project, &t.file, &t.title) == remote.test_id);
    let local_ref = local.as_ref().map(|t| FailureRef {
        status: match t.status {
            Classification::Failed => RunStatus::Failed,
            Classification::Flaky => RunStatus::Flaky,
            Classification::Passed | Classification::Skipped => RunStatus::Passed,
        },
        fingerprint: matches!(t.status, Classification::Failed | Classification::Flaky)
            .then(|| fingerprint(&remote.test_id, t.error.as_deref().unwrap_or("")).0),
        error: t.error.clone(),
    });
    let reproduced = local_ref.as_ref().is_some_and(|l| {
        l.status == RunStatus::Failed && l.fingerprint.is_some() && l.fingerprint == remote_fp
    });
    let output = ReproOutput {
        reproduced,
        exit_code: if reproduced { 0 } else { 1 },
        evidence_dir: slash(
            evidence_dir
                .strip_prefix(&ctx.root)
                .unwrap_or(&evidence_dir),
        ),
        remote: FailureRef {
            status: remote_status(remote.status),
            fingerprint: remote_fp,
            error: remote.error.clone(),
        },
        local: local_ref,
    };

    let show = |r: &FailureRef| {
        let status = serde_json::to_value(r.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let fp = r
            .fingerprint
            .as_ref()
            .map(|f| format!("  guard-fp-{f}"))
            .unwrap_or_default();
        let error = r
            .error
            .as_ref()
            .map(|e| format!("  {e}"))
            .unwrap_or_default();
        format!("{status}{fp}{error}")
    };
    println!("remote: {}", show(&output.remote));
    match &output.local {
        Some(local) => println!("local:  {}", show(local)),
        None => println!("local:  not run (the test was not found in the local report)"),
    }
    println!(
        "reproduced: {}",
        if reproduced {
            "yes (same fingerprint)"
        } else {
            "no"
        }
    );
    println!("evidence: {}/", output.evidence_dir);
    if let Some(path) = &args.result_file {
        fs::write(
            path,
            serde_json::to_string_pretty(&output).expect("repro output serialises"),
        )
        .map_err(|e| CliError::env(format!("cannot write {}: {e}", path.display())))?;
    }
    Ok(if reproduced { EXIT_OK } else { EXIT_FAILED })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_at(url: &str, name: &str) -> EvidenceFile {
        EvidenceFile {
            kind: EvidenceKind::Screenshot,
            name: name.into(),
            content_type: "image/png".into(),
            key: Some("k".into()),
            url: Some(url.into()),
            viewer_url: None,
        }
    }

    fn file_url(path: &Path) -> String {
        format!("file:///{}", slash(path).trim_start_matches('/'))
    }

    #[tokio::test]
    async fn file_urls_under_the_store_root_are_copied() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = dir.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let src = store.join("shot.png");
        fs::write(&src, b"png").unwrap();
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let sources = Sources {
            http_origins: vec![],
            fs_root: Some(store.clone()),
        };
        download(&file_at(&file_url(&src), "s.png"), &out, &sources)
            .await
            .unwrap();
        assert_eq!(fs::read(out.join("s.png")).unwrap(), b"png");
        let memory = file_at("memory://evidence/k", "s.png");
        assert!(download(&memory, &out, &sources).await.is_err());
        // A file outside the store root is refused.
        let outside = dir.path().join("secret.txt");
        fs::write(&outside, b"secret").unwrap();
        let err = download(&file_at(&file_url(&outside), "x.png"), &out, &sources)
            .await
            .unwrap_err();
        assert!(err.contains("outside"), "{err}");
        assert!(!out.join("x.png").exists());
        // No file store configured: every file:// URL is refused.
        let none = Sources::default();
        assert!(
            download(&file_at(&file_url(&src), "y.png"), &out, &none)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn evidence_names_cannot_escape_the_evidence_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = dir.path().join("store");
        fs::create_dir_all(&store).unwrap();
        let src = store.join("shot.png");
        fs::write(&src, b"png").unwrap();
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let sources = Sources {
            http_origins: vec![],
            fs_root: Some(store),
        };
        let absolute = slash(&dir.path().join("abs.png"));
        for name in [
            "../evil.png",
            "..",
            ".",
            "",
            "a/b.png",
            "a\\b.png",
            ".git",
            absolute.as_str(),
            "C:evil.png",
        ] {
            let err = download(&file_at(&file_url(&src), name), &out, &sources)
                .await
                .unwrap_err();
            assert!(err.contains("unsafe evidence name"), "{name}: {err}");
        }
        assert!(!dir.path().join("evil.png").exists());
        assert!(!dir.path().join("abs.png").exists());
    }

    #[tokio::test]
    async fn http_urls_must_come_from_a_configured_origin() {
        let dir = tempfile::TempDir::new().unwrap();
        let sources = Sources {
            http_origins: vec!["https://minio.acme.test:9000".into()],
            fs_root: None,
        };
        for url in [
            "https://attacker.test/x.png",
            "https://minio.acme.test:9000@attacker.test/x.png",
            "http://minio.acme.test:9000/x.png",
            "ftp://minio.acme.test:9000/x.png",
        ] {
            let err = download(&file_at(url, "x.png"), dir.path(), &sources)
                .await
                .unwrap_err();
            assert!(
                err.contains("not an allowed evidence origin"),
                "{url}: {err}"
            );
        }
    }

    #[test]
    fn sources_come_from_the_server_and_the_storage_url() {
        let s = Sources::from_urls(
            Some("https://guard.acme.test/"),
            Some("s3+https://AK:SK@minio.acme.test:9000/evidence"),
        );
        assert_eq!(
            s.http_origins,
            vec!["https://guard.acme.test", "https://minio.acme.test:9000"]
        );
        assert_eq!(s.fs_root, None);
        let dir = tempfile::TempDir::new().unwrap();
        let url = reqwest::Url::from_directory_path(dir.path()).unwrap();
        let s = Sources::from_urls(None, Some(url.as_str()));
        assert!(s.http_origins.is_empty());
        assert!(s.fs_root.is_some());
    }

    #[test]
    fn only_eight_hex_fingerprints_name_the_evidence_dir() {
        assert_eq!(evidence_folder(Some("1a2b3c4d")), "1a2b3c4d");
        for bad in ["../../.git", "1A2B3C4D", "1a2b3c4", "1a2b3c4d5", "/tmp/x"] {
            assert_eq!(evidence_folder(Some(bad)), "unknown", "{bad}");
        }
        assert_eq!(evidence_folder(None), "unknown");
    }

    #[test]
    fn remote_test_files_and_projects_are_validated() {
        let dir = tempfile::TempDir::new().unwrap();
        let e2e = dir.path().join("e2e");
        fs::create_dir_all(e2e.join("tests/journeys")).unwrap();
        fs::write(e2e.join("tests/journeys/a.spec.ts"), "x").unwrap();
        fs::write(dir.path().join("outside.spec.ts"), "x").unwrap();
        assert_eq!(
            playwright_args(&e2e, "tests/journeys/a.spec.ts", "chromium").unwrap(),
            vec!["tests/journeys/a.spec.ts", "--project=chromium"]
        );
        assert_eq!(
            playwright_args(&e2e, "tests/journeys/a.spec.ts", "").unwrap(),
            vec!["tests/journeys/a.spec.ts"]
        );
        let absolute = slash(&e2e.join("tests/journeys/a.spec.ts"));
        for file in [
            "--config=evil.js",
            "-c",
            "../outside.spec.ts",
            "tests/../../outside.spec.ts",
            absolute.as_str(),
            "tests/journeys/missing.spec.ts",
            "",
        ] {
            assert!(
                playwright_args(&e2e, file, "chromium").is_err(),
                "{file} accepted"
            );
        }
        for project in ["--config=x", "-x", "a b\nc", "a;b"] {
            assert!(
                playwright_args(&e2e, "tests/journeys/a.spec.ts", project).is_err(),
                "{project} accepted"
            );
        }
        assert!(playwright_args(&e2e, "tests/journeys/a.spec.ts", "Mobile Chrome").is_ok());
    }
}
