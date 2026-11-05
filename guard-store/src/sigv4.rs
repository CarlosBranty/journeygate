//! AWS Signature Version 4 for S3 (header-signed requests and presigned
//! query strings). Checked against the examples of the AWS S3 docs
//! ("Signature Calculations for the Authorization Header" and "Authenticating
//! Requests: Using Query Parameters").

use hmac::{Hmac, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

pub(crate) const ALGORITHM: &str = "AWS4-HMAC-SHA256";
pub(crate) const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";
const SERVICE: &str = "s3";

/// RFC 3986 unreserved characters stay; everything else is `%XX` (uppercase).
const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

#[derive(Clone)]
pub(crate) struct Credentials {
    pub access_key: String,
    pub secret_key: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key", &self.access_key)
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

/// Encodes one value (query names/values, path segments): `/` is encoded too.
pub(crate) fn uri_encode(s: &str) -> String {
    utf8_percent_encode(s, UNRESERVED).to_string()
}

/// `/<bucket>/<key>` with every segment encoded and `/` kept (path style).
pub(crate) fn canonical_path(segments: &[&str]) -> String {
    let mut out = String::new();
    for seg in segments {
        for part in seg.split('/') {
            out.push('/');
            out.push_str(&uri_encode(part));
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `YYYYMMDDTHHMMSSZ`.
pub(crate) fn amz_date(ts: OffsetDateTime) -> String {
    let t = ts.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

fn scope(date: &str, region: &str) -> String {
    format!("{}/{region}/{SERVICE}/aws4_request", &date[..8])
}

fn signature(creds: &Credentials, amz_date: &str, region: &str, canonical_request: &str) -> String {
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{}\n{}",
        scope(amz_date, region),
        sha256_hex(canonical_request.as_bytes())
    );
    let k_date = hmac(
        format!("AWS4{}", creds.secret_key).as_bytes(),
        &amz_date.as_bytes()[..8],
    );
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, SERVICE.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    hex::encode(hmac(&k_signing, string_to_sign.as_bytes()))
}

/// Sorted, encoded `name=value&...` from already-decoded pairs.
fn canonical_query(params: &[(String, String)]) -> String {
    let mut encoded: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (uri_encode(k), uri_encode(v)))
        .collect();
    encoded.sort();
    encoded
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// A header-signed request.
pub(crate) struct HeaderRequest<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [(String, String)],
    /// Every header to sign, including `host`, `x-amz-date` and
    /// `x-amz-content-sha256`. Names are lowercased here.
    pub headers: &'a [(&'a str, String)],
    pub payload_hash: &'a str,
    pub amz_date: &'a str,
    pub region: &'a str,
}

/// The `Authorization` header value.
pub(crate) fn authorization(creds: &Credentials, req: &HeaderRequest<'_>) -> String {
    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_request = format!(
        "{}\n{}\n{}\n{canonical_headers}\n{signed_headers}\n{}",
        req.method,
        req.path,
        canonical_query(req.query),
        req.payload_hash
    );
    format!(
        "{ALGORITHM} Credential={}/{}, SignedHeaders={signed_headers}, Signature={}",
        creds.access_key,
        scope(req.amz_date, req.region),
        signature(creds, req.amz_date, req.region, &canonical_request)
    )
}

/// The full query string (encoded, signature last) of a presigned `GET`.
pub(crate) fn presigned_query(
    creds: &Credentials,
    host: &str,
    path: &str,
    amz_date: &str,
    region: &str,
    expires_secs: u64,
) -> String {
    let params = vec![
        ("X-Amz-Algorithm".to_string(), ALGORITHM.to_string()),
        (
            "X-Amz-Credential".to_string(),
            format!("{}/{}", creds.access_key, scope(amz_date, region)),
        ),
        ("X-Amz-Date".to_string(), amz_date.to_string()),
        ("X-Amz-Expires".to_string(), expires_secs.to_string()),
        ("X-Amz-SignedHeaders".to_string(), "host".to_string()),
    ];
    let query = canonical_query(&params);
    let canonical_request =
        format!("GET\n{path}\n{query}\nhost:{host}\n\nhost\n{UNSIGNED_PAYLOAD}");
    let sig = signature(creds, amz_date, region, &canonical_request);
    format!("{query}&X-Amz-Signature={sig}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Credentials {
        Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        }
    }

    const EMPTY_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn aws_example_get_object_with_range() {
        let headers = [
            ("Host", "examplebucket.s3.amazonaws.com".to_string()),
            ("Range", "bytes=0-9".to_string()),
            ("x-amz-content-sha256", EMPTY_SHA.to_string()),
            ("x-amz-date", "20130524T000000Z".to_string()),
        ];
        let auth = authorization(
            &example(),
            &HeaderRequest {
                method: "GET",
                path: "/test.txt",
                query: &[],
                headers: &headers,
                payload_hash: EMPTY_SHA,
                amz_date: "20130524T000000Z",
                region: "us-east-1",
            },
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn aws_example_put_object() {
        let body = b"Welcome to Amazon S3.";
        let hash = sha256_hex(body);
        assert_eq!(
            hash,
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072"
        );
        let headers = [
            ("date", "Fri, 24 May 2013 00:00:00 GMT".to_string()),
            ("host", "examplebucket.s3.amazonaws.com".to_string()),
            ("x-amz-content-sha256", hash.clone()),
            ("x-amz-date", "20130524T000000Z".to_string()),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY".to_string()),
        ];
        let path = canonical_path(&["test$file.text"]);
        assert_eq!(path, "/test%24file.text");
        let auth = authorization(
            &example(),
            &HeaderRequest {
                method: "PUT",
                path: &path,
                query: &[],
                headers: &headers,
                payload_hash: &hash,
                amz_date: "20130524T000000Z",
                region: "us-east-1",
            },
        );
        assert!(
            auth.ends_with(
                "Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"
            ),
            "{auth}"
        );
    }

    #[test]
    fn aws_example_presigned_get() {
        let query = presigned_query(
            &example(),
            "examplebucket.s3.amazonaws.com",
            "/test.txt",
            "20130524T000000Z",
            "us-east-1",
            86400,
        );
        assert_eq!(
            query,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    #[test]
    fn path_encoding_keeps_slashes_and_encodes_the_rest() {
        assert_eq!(
            canonical_path(&["bucket", "a b/c+d/café~_-.png"]),
            "/bucket/a%20b/c%2Bd/caf%C3%A9~_-.png"
        );
        assert_eq!(canonical_path(&["bucket"]), "/bucket");
    }

    #[test]
    fn amz_date_format() {
        let ts = OffsetDateTime::from_unix_timestamp(1_369_353_600).unwrap();
        assert_eq!(amz_date(ts), "20130524T000000Z");
    }

    #[test]
    fn credentials_debug_hides_the_secret() {
        let dbg = format!("{:?}", example());
        assert!(!dbg.contains("wJalr"), "{dbg}");
    }
}
