use super::*;

#[test]
fn formats_the_epoch() {
    assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
}

#[test]
fn formats_known_instants() {
    assert_eq!(rfc3339_utc(1_700_000_000), "2023-11-14T22:13:20Z");
    assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(rfc3339_utc(1_793_448_000), "2026-10-31T12:00:00Z");
}
