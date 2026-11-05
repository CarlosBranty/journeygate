//! The shared `EvidenceStore` suite against the filesystem store, the core
//! in-memory fake (always run) and a live S3/MinIO (`GUARD_S3_TEST_ENDPOINT`).

mod common;

use common::evidence_suite;
use guard_core::ports::fakes::InMemoryEvidenceStore;
use guard_store::FsEvidenceStore;

fn temp_root() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(common::unique("guard-store-fs"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

macro_rules! suite {
    ($($name:ident),* $(,)?) => {
        mod fs { use super::*; $(
            #[tokio::test]
            async fn $name() {
                let root = temp_root();
                let store = FsEvidenceStore::new(&root);
                evidence_suite::$name(&store, "acme-shop").await;
                std::fs::remove_dir_all(root).unwrap();
            }
        )* }
        mod memory { use super::*; $(
            #[tokio::test]
            async fn $name() {
                evidence_suite::$name(&InMemoryEvidenceStore::new(), "acme-shop").await;
            }
        )* }
        mod s3_live { use super::*; $(
            #[tokio::test]
            async fn $name() {
                let Some((store, prefix)) = common::s3().await else { return };
                evidence_suite::$name(&store, &prefix).await;
            }
        )* }
    };
}

suite!(
    put_then_get,
    put_overwrites,
    empty_object_round_trips,
    keys_with_spaces_and_unicode,
    missing_object_is_not_found,
    presign_validates_expiry,
);

#[tokio::test]
async fn invalid_keys_are_rejected_everywhere() {
    let root = temp_root();
    evidence_suite::invalid_keys_are_rejected(&FsEvidenceStore::new(&root)).await;
    evidence_suite::invalid_keys_are_rejected(&InMemoryEvidenceStore::new()).await;
    if let Some((store, _)) = common::s3().await {
        evidence_suite::invalid_keys_are_rejected(&store).await;
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn s3_live_presigned_url_downloads_the_object() {
    let Some((store, prefix)) = common::s3().await else {
        return;
    };
    use guard_core::ports::EvidenceStore;
    let key = format!("{prefix}/run 1/trace.zip");
    store
        .put_object(&key, b"PK\x03\x04trace".to_vec(), "application/zip")
        .await
        .unwrap();
    let url = store
        .presign_get(&key, std::time::Duration::from_secs(600))
        .unwrap();
    let resp = reqwest::get(&url).await.unwrap();
    assert_eq!(resp.status(), 200, "{url}");
    assert_eq!(
        resp.headers()["content-type"].to_str().unwrap(),
        "application/zip"
    );
    assert_eq!(resp.bytes().await.unwrap().as_ref(), b"PK\x03\x04trace");
}

#[tokio::test]
async fn fs_presigned_url_points_at_the_file() {
    use guard_core::ports::EvidenceStore;
    let root = temp_root();
    let store = FsEvidenceStore::new(&root);
    let key = "acme-shop/run-1/abcd1234/screenshot.png";
    store
        .put_object(key, b"png".to_vec(), "image/png")
        .await
        .unwrap();
    let url = store
        .presign_get(key, std::time::Duration::from_secs(60))
        .unwrap();
    assert!(url.starts_with("file://"), "{url}");
    let path = url::Url::parse(&url).unwrap().to_file_path().unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"png");
    std::fs::remove_dir_all(root).unwrap();
}
