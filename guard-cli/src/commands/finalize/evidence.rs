//! Evidence of `guard gate finalize` (finalize-phase2.md, "Evidence"): what
//! the merged JSON report says about each failure, and the upload of its
//! screenshot / trace / video to the `EvidenceStore`.

use crate::context::slash;
use guard_core::ports::{AttemptSummary, EvidenceFile, EvidenceKind, EvidenceStore};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One `tests[]` entry of the raw report with the identity guard uses.
pub struct RawTest<'a> {
    pub file: String,
    pub title: String,
    pub project: String,
    pub test: &'a Value,
}

/// Walks the raw report like `guard_core::verdict::classify`: root suites are
/// files, nested suite titles and the spec title join with ` > `.
pub fn raw_tests(report: &Value) -> Vec<RawTest<'_>> {
    fn walk<'a>(suite: &'a Value, chain: &mut Vec<String>, out: &mut Vec<RawTest<'a>>) {
        for spec in suite["specs"].as_array().into_iter().flatten() {
            let mut title = chain.clone();
            title.push(spec["title"].as_str().unwrap_or_default().to_string());
            let title = title.join(" > ");
            for test in spec["tests"].as_array().into_iter().flatten() {
                out.push(RawTest {
                    file: spec["file"].as_str().unwrap_or_default().to_string(),
                    title: title.clone(),
                    project: test["projectName"].as_str().unwrap_or_default().to_string(),
                    test,
                });
            }
        }
        for child in suite["suites"].as_array().into_iter().flatten() {
            chain.push(child["title"].as_str().unwrap_or_default().to_string());
            walk(child, chain, out);
            chain.pop();
        }
    }
    let mut out = Vec::new();
    for root in report["suites"].as_array().into_iter().flatten() {
        walk(root, &mut Vec::new(), &mut out);
    }
    out
}

fn results(test: &Value) -> &[Value] {
    test["results"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn failing(result: &Value) -> bool {
    !matches!(result["status"].as_str(), Some("passed") | Some("skipped"))
}

fn first_line(message: &str) -> Option<String> {
    message
        .lines()
        .map(str::trim_end)
        .find(|l| !l.trim().is_empty())
        .map(str::to_string)
}

pub fn attempts(test: &Value) -> Vec<AttemptSummary> {
    results(test)
        .iter()
        .map(|r| AttemptSummary {
            retry: r["retry"].as_u64().unwrap_or(0) as u32,
            status: r["status"].as_str().unwrap_or("unknown").to_string(),
            duration_ms: r["duration"].as_f64().unwrap_or(0.0).max(0.0).round() as u64,
            error: r["error"]["message"].as_str().and_then(first_line),
        })
        .collect()
}

/// `file:line` of the last failing attempt's error, relative to the Playwright dir.
pub fn location(test: &Value, e2e_dir: &Path) -> Option<String> {
    let result = results(test).iter().rev().find(|r| failing(r))?;
    let loc = &result["error"]["location"];
    let file = loc["file"].as_str()?;
    let line = loc["line"].as_u64()?;
    let path = Path::new(file);
    let shown = path
        .strip_prefix(e2e_dir)
        .map(slash)
        .unwrap_or_else(|_| slash(path));
    Some(format!("{shown}:{line}"))
}

/// The `seed` annotation (test level, else any attempt).
pub fn seed(test: &Value) -> Option<String> {
    let from = |annotations: &Value| {
        annotations.as_array()?.iter().find_map(|a| {
            (a["type"] == "seed").then(|| a["description"].as_str().map(str::to_string))?
        })
    };
    from(&test["annotations"])
        .or_else(|| results(test).iter().find_map(|r| from(&r["annotations"])))
}

/// A local evidence file to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub kind: EvidenceKind,
    pub content_type: String,
    pub path: PathBuf,
}

fn kind_of(name: &str) -> Option<EvidenceKind> {
    match name {
        "screenshot" => Some(EvidenceKind::Screenshot),
        "trace" => Some(EvidenceKind::Trace),
        "video" => Some(EvidenceKind::Video),
        _ => None,
    }
}

fn attachment(value: &Value, e2e_dir: &Path) -> Option<LocalFile> {
    let kind = kind_of(value["name"].as_str()?)?;
    let raw = PathBuf::from(value["path"].as_str()?);
    let path = if raw.is_absolute() {
        raw
    } else {
        e2e_dir.join(raw)
    };
    Some(LocalFile {
        kind,
        content_type: value["contentType"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_string(),
        path,
    })
}

/// Attachments of the first retry (`retry == 1`) plus the screenshot of the
/// last failing attempt; deduplicated by path, in that order.
pub fn local_files(test: &Value, e2e_dir: &Path) -> Vec<LocalFile> {
    let mut out: Vec<LocalFile> = Vec::new();
    let all = results(test);
    if let Some(first_retry) = all.iter().find(|r| r["retry"].as_u64() == Some(1)) {
        for a in first_retry["attachments"].as_array().into_iter().flatten() {
            if let Some(file) = attachment(a, e2e_dir) {
                out.push(file);
            }
        }
    }
    if let Some(last) = all.iter().rev().find(|r| failing(r)) {
        if let Some(shot) = last["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| attachment(a, e2e_dir))
            .rfind(|f| f.kind == EvidenceKind::Screenshot)
        {
            out.push(shot);
        }
    }
    let mut seen = BTreeSet::new();
    out.retain(|f| seen.insert(f.path.clone()));
    out
}

/// Gives each file a name unique in the run (`name`, `name-2`, ...).
pub fn unique_name(base: &str, taken: &mut BTreeSet<String>) -> String {
    if taken.insert(base.to_string()) {
        return base.to_string();
    }
    let (stem, ext) = match base.rfind('.') {
        Some(i) if i > 0 => (&base[..i], &base[i..]),
        _ => (base, ""),
    };
    (2..)
        .map(|n| format!("{stem}-{n}{ext}"))
        .find(|candidate| taken.insert(candidate.clone()))
        .expect("an unused name")
}

/// `https://trace.playwright.dev/?trace=<percent-encoded url>`.
pub fn viewer_url(url: &str) -> String {
    format!(
        "https://trace.playwright.dev/?trace={}",
        utf8_percent_encode(url, NON_ALPHANUMERIC)
    )
}

/// Presigned links live `min(retention_days, 7)` days (at least one).
pub fn link_ttl(retention_days: i64) -> Duration {
    let days = retention_days.clamp(1, 7) as u64;
    Duration::from_secs(days * 24 * 3600)
}

/// Why an upload failed: a warning text, and whether the store itself looks
/// down (transport error or 5xx), so the rest of the run skips it.
#[derive(Debug)]
pub struct UploadError {
    pub message: String,
    pub store_down: bool,
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn is_store_down(e: &guard_core::ports::EvidenceError) -> bool {
    use guard_core::ports::EvidenceError;
    match e {
        EvidenceError::Transport(_) => true,
        EvidenceError::Rejected { status, .. } => *status >= 500,
        _ => false,
    }
}

/// Uploads one file under `key`. A successful upload gets a presigned URL
/// (and a viewer URL for traces).
pub async fn upload(
    store: &dyn EvidenceStore,
    key: &str,
    file: &LocalFile,
    name: &str,
    ttl: Duration,
) -> Result<EvidenceFile, UploadError> {
    let bytes = tokio::fs::read(&file.path).await.map_err(|e| UploadError {
        message: format!("cannot read {}: {e}", file.path.display()),
        store_down: false,
    })?;
    store
        .put_object(key, bytes, &file.content_type)
        .await
        .map_err(|e| UploadError {
            message: format!("upload {key}: {e}"),
            store_down: is_store_down(&e),
        })?;
    let url = store.presign_get(key, ttl).map_err(|e| UploadError {
        message: format!("presign {key}: {e}"),
        store_down: false,
    })?;
    Ok(EvidenceFile {
        kind: file.kind,
        name: name.to_string(),
        content_type: file.content_type.clone(),
        key: Some(key.to_string()),
        viewer_url: (file.kind == EvidenceKind::Trace).then(|| viewer_url(&url)),
        url: Some(url),
    })
}

/// The not-uploaded form of a file (no storage, or the upload failed).
pub fn local_only(file: &LocalFile, name: &str) -> EvidenceFile {
    EvidenceFile {
        kind: file.kind,
        name: name.to_string(),
        content_type: file.content_type.clone(),
        key: None,
        url: None,
        viewer_url: None,
    }
}

/// Every file of `playwright-report/` (relative path, absolute path), or
/// `None` when it is over 500 files / 200 MiB.
pub fn report_files(dir: &Path) -> Option<Vec<(String, PathBuf)>> {
    const MAX_FILES: usize = 500;
    const MAX_BYTES: u64 = 200 * 1024 * 1024;
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>, bytes: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out, bytes);
            } else if let Ok(rel) = path.strip_prefix(base) {
                *bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
                out.push((slash(rel), path.clone()));
            }
        }
    }
    let mut files = Vec::new();
    let mut bytes = 0;
    walk(dir, dir, &mut files, &mut bytes);
    files.sort();
    (files.len() <= MAX_FILES && bytes <= MAX_BYTES).then_some(files)
}

pub fn content_type_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webm" => "video/webm",
        "zip" => "application/zip",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Value {
        serde_json::json!({"suites": [{
            "title": "tests/a.spec.ts", "file": "tests/a.spec.ts", "specs": [],
            "suites": [{"title": "Group", "specs": [{
                "title": "does it", "file": "tests/a.spec.ts", "line": 4,
                "tests": [{"projectName": "chromium", "status": "unexpected",
                  "annotations": [{"type": "seed", "description": "1234"}],
                  "results": [
                    {"retry": 0, "status": "failed", "duration": 10.4,
                     "error": {"message": "\nboom 1\nstack"},
                     "attachments": [{"name": "screenshot", "contentType": "image/png", "path": "test-results/a/r0.png"}]},
                    {"retry": 1, "status": "failed", "duration": 20,
                     "error": {"message": "boom 2"},
                     "attachments": [
                       {"name": "screenshot", "contentType": "image/png", "path": "test-results/a/r1.png"},
                       {"name": "trace", "contentType": "application/zip", "path": "test-results/a/trace.zip"},
                       {"name": "stdout", "contentType": "text/plain", "path": "x"}]},
                    {"retry": 2, "status": "timedOut", "duration": 30,
                     "error": {"message": "boom 3", "location": {"file": "/ci/e2e/tests/a.spec.ts", "line": 9, "column": 3}},
                     "attachments": [{"name": "screenshot", "contentType": "image/png", "path": "test-results/a/r2.png"}]}
                  ]}]}],
              "suites": []}]
        }]})
    }

    #[test]
    fn raw_tests_use_the_verdict_identity() {
        let report = report();
        let tests = raw_tests(&report);
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].title, "Group > does it");
        assert_eq!(tests[0].file, "tests/a.spec.ts");
        assert_eq!(tests[0].project, "chromium");
    }

    #[test]
    fn evidence_facts_come_from_the_attempts() {
        let report = report();
        let test = raw_tests(&report)[0].test;
        let e2e = Path::new("/ci/e2e");
        assert_eq!(location(test, e2e).as_deref(), Some("tests/a.spec.ts:9"));
        assert_eq!(seed(test).as_deref(), Some("1234"));
        let attempts = attempts(test);
        assert_eq!(attempts.len(), 3);
        assert_eq!(attempts[0].error.as_deref(), Some("boom 1"));
        assert_eq!(attempts[0].duration_ms, 10);
        assert_eq!(attempts[2].status, "timedOut");
        let files = local_files(test, e2e);
        let names: Vec<_> = files
            .iter()
            .map(|f| {
                (
                    f.kind,
                    f.path.file_name().unwrap().to_string_lossy().into_owned(),
                )
            })
            .collect();
        assert_eq!(
            names,
            vec![
                (EvidenceKind::Screenshot, "r1.png".to_string()),
                (EvidenceKind::Trace, "trace.zip".to_string()),
                (EvidenceKind::Screenshot, "r2.png".to_string()),
            ]
        );
        assert!(files[0].path.starts_with(e2e));
    }

    #[test]
    fn unique_names_get_a_counter_before_the_extension() {
        let mut taken = BTreeSet::new();
        assert_eq!(
            unique_name("test-failed-1.png", &mut taken),
            "test-failed-1.png"
        );
        assert_eq!(
            unique_name("test-failed-1.png", &mut taken),
            "test-failed-1-2.png"
        );
        assert_eq!(
            unique_name("test-failed-1.png", &mut taken),
            "test-failed-1-3.png"
        );
        assert_eq!(unique_name("trace", &mut taken), "trace");
        assert_eq!(unique_name("trace", &mut taken), "trace-2");
    }

    #[test]
    fn links_and_viewer_urls() {
        assert_eq!(link_ttl(30), Duration::from_secs(7 * 86_400));
        assert_eq!(link_ttl(2), Duration::from_secs(2 * 86_400));
        assert_eq!(link_ttl(0), Duration::from_secs(86_400));
        assert_eq!(
            viewer_url("https://s3/x.zip?a=1&b=2"),
            "https://trace.playwright.dev/?trace=https%3A%2F%2Fs3%2Fx%2Ezip%3Fa%3D1%26b%3D2"
        );
    }

    #[tokio::test]
    async fn upload_puts_and_presigns() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("trace.zip");
        std::fs::write(&path, b"zip").unwrap();
        let store = guard_core::ports::fakes::InMemoryEvidenceStore::new();
        let file = LocalFile {
            kind: EvidenceKind::Trace,
            content_type: "application/zip".into(),
            path,
        };
        let uploaded = upload(
            &store,
            "app/r1/fp/trace.zip",
            &file,
            "trace.zip",
            link_ttl(7),
        )
        .await
        .unwrap();
        assert_eq!(uploaded.key.as_deref(), Some("app/r1/fp/trace.zip"));
        assert!(
            uploaded
                .viewer_url
                .unwrap()
                .starts_with("https://trace.playwright.dev/?trace=")
        );
        assert_eq!(store.object("app/r1/fp/trace.zip").unwrap().0, b"zip");
        let missing = LocalFile {
            path: dir.path().join("nope"),
            ..file
        };
        let err = upload(&store, "k", &missing, "n", link_ttl(7))
            .await
            .unwrap_err();
        assert!(
            !err.store_down,
            "a local read error says nothing about the store"
        );
    }

    struct DownStore;

    #[async_trait::async_trait]
    impl EvidenceStore for DownStore {
        async fn put_object(
            &self,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> Result<guard_core::ports::ObjectRef, guard_core::ports::EvidenceError> {
            Err(guard_core::ports::EvidenceError::Transport(
                "refused".into(),
            ))
        }
        async fn get_object(&self, key: &str) -> Result<Vec<u8>, guard_core::ports::EvidenceError> {
            Err(guard_core::ports::EvidenceError::NotFound(key.into()))
        }
        fn presign_get(
            &self,
            _: &str,
            _: Duration,
        ) -> Result<String, guard_core::ports::EvidenceError> {
            Ok("u".into())
        }
    }

    #[tokio::test]
    async fn a_transport_error_marks_the_store_down() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, b"png").unwrap();
        let file = LocalFile {
            kind: EvidenceKind::Screenshot,
            content_type: "image/png".into(),
            path,
        };
        let err = upload(&DownStore, "k", &file, "a.png", link_ttl(7))
            .await
            .unwrap_err();
        assert!(err.store_down);
        assert!(err.message.contains("refused"), "{}", err.message);
    }

    #[test]
    fn report_files_lists_relative_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("data")).unwrap();
        std::fs::write(dir.path().join("index.html"), "x").unwrap();
        std::fs::write(dir.path().join("data/a.png"), "y").unwrap();
        let files = report_files(dir.path()).unwrap();
        let rel: Vec<_> = files.iter().map(|(r, _)| r.as_str()).collect();
        assert_eq!(rel, vec!["data/a.png", "index.html"]);
        assert_eq!(content_type_of("index.html"), "text/html; charset=utf-8");
    }
}
