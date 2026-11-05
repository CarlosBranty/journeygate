//! `FsEvidenceStore`: evidence on the local filesystem, for CI-native mode
//! (no MinIO: the CI uploads the directory as a job artifact).
//!
//! Object `<key>` lives at `<root>/<key>`. "Presigned" URLs are `file://`
//! URLs of that path (the expiry is validated like S3's but not encoded:
//! local files do not expire). Writes go to a temporary file in the same
//! directory and are renamed into place, so readers never see half a file.
//!
//! On top of the shared key rule (`validate_key`), keys must not contain `\`,
//! `:`, NUL, or empty / `.` segments: those would alias or escape the root on
//! some filesystem (Windows drive letters and separators in particular).

use guard_core::ports::evidence_store::{validate_expiry, validate_key};
use guard_core::ports::{EvidenceError, EvidenceStore, ObjectRef};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct FsEvidenceStore {
    root: PathBuf,
}

fn io_error(key: &str, e: std::io::Error) -> EvidenceError {
    if e.kind() == std::io::ErrorKind::NotFound {
        EvidenceError::NotFound(key.to_string())
    } else {
        EvidenceError::Transport(format!("filesystem: {key}: {e}"))
    }
}

impl FsEvidenceStore {
    /// `root` is made absolute against the current directory; it is created
    /// on the first write.
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let root = if root.is_absolute() {
            root.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(root))
                .unwrap_or_else(|_| root.to_path_buf())
        };
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The file of `key`, after validating it.
    pub fn path_for(&self, key: &str) -> Result<PathBuf, EvidenceError> {
        validate_key(key)?;
        let bad_segment = key.split('/').any(|s| s.is_empty() || s == ".");
        if bad_segment || key.contains(['\\', ':', '\0']) {
            return Err(EvidenceError::InvalidKey(key.to_string()));
        }
        Ok(key.split('/').fold(self.root.clone(), |p, seg| p.join(seg)))
    }
}

#[async_trait::async_trait]
impl EvidenceStore for FsEvidenceStore {
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        _content_type: &str,
    ) -> Result<ObjectRef, EvidenceError> {
        static TMP: AtomicU64 = AtomicU64::new(0);
        let path = self.path_for(key)?;
        let dir = path.parent().unwrap_or(&self.root).to_path_buf();
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| io_error(key, e))?;
        let tmp = dir.join(format!(
            ".guard-tmp-{}-{}",
            std::process::id(),
            TMP.fetch_add(1, Ordering::Relaxed)
        ));
        let size = bytes.len() as u64;
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| io_error(key, e))?;
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(io_error(key, e));
        }
        Ok(ObjectRef {
            key: key.to_string(),
            size,
        })
    }

    async fn get_object(&self, key: &str) -> Result<Vec<u8>, EvidenceError> {
        let path = self.path_for(key)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(bytes),
            // A directory at that path is not an object.
            Err(e) if path.is_dir() => Err(io_error(
                key,
                std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()),
            )),
            Err(e) => Err(io_error(key, e)),
        }
    }

    fn presign_get(&self, key: &str, expires_in: Duration) -> Result<String, EvidenceError> {
        let path = self.path_for(key)?;
        validate_expiry(expires_in)?;
        url::Url::from_file_path(&path)
            .map(String::from)
            .map_err(|_| EvidenceError::Unconfigured(format!("{} is not absolute", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_keys_that_alias_or_escape_the_root() {
        let s = FsEvidenceStore::new("/tmp/guard-root");
        for key in ["a\\..\\b", "c:/x", "a//b", "a/./b", "a/", "nul\0"] {
            assert!(
                matches!(s.path_for(key), Err(EvidenceError::InvalidKey(_))),
                "{key:?}"
            );
        }
        assert!(s.path_for("repo/run-1/fp/screenshot.png").is_ok());
    }

    #[test]
    fn relative_root_is_made_absolute() {
        let s = FsEvidenceStore::new("evidence");
        assert!(s.root().is_absolute());
        assert!(s.root().ends_with("evidence"));
    }
}
