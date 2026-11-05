//! `EvidenceStore`: S3-compatible object storage (MinIO in `deploy/`).

use super::types::ObjectRef;
use std::time::Duration;

/// Longest validity of a presigned URL (SigV4 maximum: 7 days).
pub const MAX_PRESIGN_EXPIRY: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceError {
    /// `GUARD_STORAGE_URL` (or the configured env var) missing or malformed.
    #[error("evidence store not configured: {0}")]
    Unconfigured(String),
    /// Empty key, a key starting with `/`, or containing `..` segments.
    #[error("invalid object key: {0}")]
    InvalidKey(String),
    #[error("object {0} not found")]
    NotFound(String),
    /// `expires_in` is zero or longer than [`MAX_PRESIGN_EXPIRY`].
    #[error("invalid presign expiry: {0:?}")]
    InvalidExpiry(Duration),
    #[error("evidence store rejected the request (HTTP {status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("evidence store transport error: {0}")]
    Transport(String),
}

#[async_trait::async_trait]
pub trait EvidenceStore: Send + Sync {
    /// Uploads one object (overwrites an existing key).
    async fn put_object(
        &self,
        key: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<ObjectRef, EvidenceError>;

    /// Downloads one object (the server reads screenshots back to attach them
    /// to Jira). A missing key is `EvidenceError::NotFound`.
    async fn get_object(&self, key: &str) -> Result<Vec<u8>, EvidenceError>;

    /// A presigned `GET` URL valid for `expires_in`. Pure computation: does
    /// not check that the object exists.
    fn presign_get(&self, key: &str, expires_in: Duration) -> Result<String, EvidenceError>;
}

/// Key rule shared by every adapter: non-empty, no leading `/`, no `..` segment.
pub fn validate_key(key: &str) -> Result<(), EvidenceError> {
    if key.is_empty() || key.starts_with('/') || key.split('/').any(|seg| seg == "..") {
        return Err(EvidenceError::InvalidKey(key.to_string()));
    }
    Ok(())
}

/// Expiry rule shared by every adapter: `0 < expires_in <= MAX_PRESIGN_EXPIRY`.
pub fn validate_expiry(expires_in: Duration) -> Result<(), EvidenceError> {
    if expires_in.is_zero() || expires_in > MAX_PRESIGN_EXPIRY {
        return Err(EvidenceError::InvalidExpiry(expires_in));
    }
    Ok(())
}
