//! Request bodies: size limits with the JSON `413`, JSON decoding with the
//! JSON `400`, and the raw `WebhookRequest` the verifiers need.

use crate::error::ApiError;
use axum::body::{Body, Bytes};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use guard_core::ports::WebhookRequest;
use http_body_util::{BodyExt, LengthLimitError, Limited};
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;

/// Webhooks and `/api/impact`.
pub const SMALL_LIMIT: usize = 1024 * 1024;
/// `/api/gate-result`.
pub const GATE_RESULT_LIMIT: usize = 8 * 1024 * 1024;

/// Reads the whole body, refusing more than `limit` bytes.
pub async fn read_limited(body: Body, limit: usize) -> Result<Bytes, ApiError> {
    match Limited::new(body, limit).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(err) if err.downcast_ref::<LengthLimitError>().is_some() => {
            Err(ApiError::payload_too_large(limit))
        }
        Err(err) => Err(ApiError::bad_request(format!(
            "cannot read request body: {err}"
        ))),
    }
}

pub fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|e| ApiError::bad_request(format!("invalid JSON: {e}")))
}

/// Header names lowercased; repeated headers joined with `, `; values that
/// are not visible ASCII are dropped (no provider signs with them).
pub fn header_map(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in headers {
        let Ok(value) = value.to_str() else { continue };
        out.entry(name.as_str().to_ascii_lowercase())
            .and_modify(|v| {
                v.push_str(", ");
                v.push_str(value);
            })
            .or_insert_with(|| value.to_string());
    }
    out
}

pub fn webhook_request(parts: &Parts, body: Bytes) -> WebhookRequest {
    WebhookRequest {
        headers: header_map(&parts.headers),
        query: parts.uri.query().map(str::to_string),
        body: body.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn limit_is_inclusive() {
        let ok = read_limited(Body::from(vec![1u8; 10]), 10).await.unwrap();
        assert_eq!(ok.len(), 10);
        let err = read_limited(Body::from(vec![1u8; 11]), 10)
            .await
            .unwrap_err();
        assert_eq!(err.code, "payload_too_large");
    }

    #[test]
    fn repeated_headers_are_joined_and_lowercased() {
        let mut headers = HeaderMap::new();
        headers.append("X-A", HeaderValue::from_static("1"));
        headers.append("x-a", HeaderValue::from_static("2"));
        let map = header_map(&headers);
        assert_eq!(map.get("x-a").map(String::as_str), Some("1, 2"));
    }

    #[test]
    fn bad_json_is_bad_request() {
        let err = decode_json::<serde_json::Value>(b"{").unwrap_err();
        assert_eq!(err.code, "bad_request");
    }
}
