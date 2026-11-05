//! Constant-time webhook checks.

use guard_core::ports::WebhookError;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// GitHub: `header` must be `sha256=<64 lowercase hex>` equal to
/// HMAC-SHA256(`body`, `secret`). Compared in constant time by `hmac`.
pub(crate) fn verify_hub_signature(
    secret: &[u8],
    body: &[u8],
    header: &str,
) -> Result<(), WebhookError> {
    let hex_digest = header
        .strip_prefix("sha256=")
        .ok_or(WebhookError::InvalidSignature)?;
    // GitHub sends lowercase hex; anything else is not a signature it produced.
    if hex_digest.len() != 64
        || !hex_digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(WebhookError::InvalidSignature);
    }
    let expected = hex::decode(hex_digest).map_err(|_| WebhookError::InvalidSignature)?;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret).map_err(|_| WebhookError::InvalidSignature)?;
    mac.update(body);
    mac.verify_slice(&expected)
        .map_err(|_| WebhookError::InvalidSignature)
}

/// GitLab: `token` must equal `secret`. Both are hashed first so the
/// constant-time comparison does not leak the secret's length either.
pub(crate) fn verify_token(secret: &[u8], token: &[u8]) -> Result<(), WebhookError> {
    let a = Sha256::digest(secret);
    let b = Sha256::digest(token);
    if bool::from(a.ct_eq(&b)) {
        Ok(())
    } else {
        Err(WebhookError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_signature_round_trip() {
        let mut mac = Hmac::<Sha256>::new_from_slice(b"k").unwrap();
        mac.update(b"body");
        let header = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
        assert_eq!(verify_hub_signature(b"k", b"body", &header), Ok(()));
        assert_eq!(
            verify_hub_signature(b"k", b"other", &header),
            Err(WebhookError::InvalidSignature)
        );
        assert_eq!(
            verify_hub_signature(b"k", b"body", &header[..header.len() - 2]),
            Err(WebhookError::InvalidSignature)
        );
    }

    #[test]
    fn token_comparison() {
        assert_eq!(verify_token(b"s", b"s"), Ok(()));
        assert_eq!(
            verify_token(b"s", b"S"),
            Err(WebhookError::InvalidSignature)
        );
        assert_eq!(verify_token(b"s", b""), Err(WebhookError::InvalidSignature));
    }
}
