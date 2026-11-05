//! Behavioral suite for `EvidenceStore`. Each function takes a key `prefix`
//! so live runs against a shared bucket never collide.

use guard_core::ports::{EvidenceError, EvidenceStore, MAX_PRESIGN_EXPIRY};
use std::time::Duration;

const DAY: Duration = Duration::from_secs(24 * 3600);

pub async fn put_then_get(s: &impl EvidenceStore, prefix: &str) {
    let key = format!("{prefix}/run-1/abcd1234/screenshot.png");
    let bytes = vec![0x89, b'P', b'N', b'G', 0, 1, 2, 255];
    let r = s
        .put_object(&key, bytes.clone(), "image/png")
        .await
        .unwrap();
    assert_eq!(r.key, key);
    assert_eq!(r.size, bytes.len() as u64);
    assert_eq!(s.get_object(&key).await.unwrap(), bytes);
}

pub async fn put_overwrites(s: &impl EvidenceStore, prefix: &str) {
    let key = format!("{prefix}/run-1/report/index.html");
    s.put_object(&key, b"old".to_vec(), "text/html")
        .await
        .unwrap();
    s.put_object(&key, b"new and longer".to_vec(), "text/html")
        .await
        .unwrap();
    assert_eq!(s.get_object(&key).await.unwrap(), b"new and longer");
}

pub async fn empty_object_round_trips(s: &impl EvidenceStore, prefix: &str) {
    let key = format!("{prefix}/empty.txt");
    let r = s.put_object(&key, vec![], "text/plain").await.unwrap();
    assert_eq!(r.size, 0);
    assert!(s.get_object(&key).await.unwrap().is_empty());
}

pub async fn keys_with_spaces_and_unicode(s: &impl EvidenceStore, prefix: &str) {
    let key = format!("{prefix}/run 1/pays with café + 100%.png");
    s.put_object(&key, b"x".to_vec(), "image/png")
        .await
        .unwrap();
    assert_eq!(s.get_object(&key).await.unwrap(), b"x");
}

pub async fn missing_object_is_not_found(s: &impl EvidenceStore, prefix: &str) {
    let key = format!("{prefix}/nope/trace.zip");
    match s.get_object(&key).await {
        Err(EvidenceError::NotFound(k)) => assert_eq!(k, key),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

pub async fn invalid_keys_are_rejected(s: &impl EvidenceStore) {
    for key in ["", "/abs/key.png", "a/../b.png", ".."] {
        assert!(
            matches!(
                s.put_object(key, vec![1], "image/png").await,
                Err(EvidenceError::InvalidKey(_))
            ),
            "put {key:?}"
        );
        assert!(
            matches!(s.get_object(key).await, Err(EvidenceError::InvalidKey(_))),
            "get {key:?}"
        );
        assert!(
            matches!(s.presign_get(key, DAY), Err(EvidenceError::InvalidKey(_))),
            "presign {key:?}"
        );
    }
}

pub async fn presign_validates_expiry(s: &impl EvidenceStore, prefix: &str) {
    let key = format!("{prefix}/x.png");
    assert!(matches!(
        s.presign_get(&key, Duration::ZERO),
        Err(EvidenceError::InvalidExpiry(_))
    ));
    assert!(matches!(
        s.presign_get(&key, MAX_PRESIGN_EXPIRY + Duration::from_secs(1)),
        Err(EvidenceError::InvalidExpiry(_))
    ));
    // The maximum itself is valid, and presigning never checks existence.
    let url = s.presign_get(&key, MAX_PRESIGN_EXPIRY).unwrap();
    assert!(!url.is_empty());
}
