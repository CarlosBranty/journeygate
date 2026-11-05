use super::*;

#[test]
fn label_and_test_id_format() {
    assert_eq!(Fingerprint("1a2b3c4d".into()).label(), "guard-fp-1a2b3c4d");
    assert_eq!(
        test_id("chromium", "tests/a.spec.ts", "A > b"),
        "chromium::tests/a.spec.ts::A > b"
    );
}

#[test]
fn contract_example() {
    assert_eq!(
        normalize_error("\x1b[31mTimeout 5000ms exceeded waiting for order 9f8e7d6c5b4a\x1b[0m"),
        "Timeout <n>ms exceeded waiting for order <id>"
    );
}

#[test]
fn normalize_error_table() {
    let cases: &[(&str, &str)] = &[
        // 1. ANSI escapes, including multi-parameter and non-`m` finals.
        ("\x1b[1;31mError\x1b[0m: boom", "Error: boom"),
        ("\x1b[2KError", "Error"),
        // 2. First non-empty line.
        ("\n\n  \nfirst line\nsecond line 42", "first line"),
        ("\r\nfirst\r\nsecond", "first"),
        ("\x1b[0m\nreal", "real"),
        ("", ""),
        ("   \n \t ", ""),
        // 3. UUIDs, both cases.
        (
            "order 123e4567-e89b-12d3-a456-426614174000 missing",
            "order <id> missing",
        ),
        (
            "order 123E4567-E89B-12D3-A456-426614174000 missing",
            "order <id> missing",
        ),
        // 4. Hex runs of 8+ with at least one digit.
        ("session deadbeef1 expired", "session <id> expired"),
        ("commit a1b2c3d4", "commit <id>"),
        ("exactly 8 0123abcd", "exactly <n> <id>"),
        ("12345678", "<id>"),
        // Seven hex characters are not an id.
        ("seven a1b2c3d", "seven a<n>b<n>c<n>d"),
        // A run of hex letters only is a word, not an id.
        ("deadbeefcafe", "deadbeefcafe"),
        ("Unexpected facade", "Unexpected facade"),
        // No word boundary: part of a longer token.
        ("id_a1b2c3d4e5", "id_a<n>b<n>c<n>d<n>e<n>"),
        // 5. Digits with an optional decimal part.
        ("expected 3 got 4", "expected <n> got <n>"),
        ("took 12.5s", "took <n>s"),
        ("version 1.2.3", "version <n>.<n>"),
        ("at file.ts:12:5", "at file.ts:<n>:<n>"),
        ("port :3000", "port :<n>"),
        // 6. Whitespace collapsed and trimmed.
        ("  a \t  b   c  ", "a b c"),
    ];
    for (input, expected) in cases {
        assert_eq!(normalize_error(input), *expected, "input: {input:?}");
    }
}

#[test]
fn normalization_is_idempotent() {
    for input in [
        "Timeout 5000ms exceeded waiting for order 9f8e7d6c5b4a",
        "order 123e4567-e89b-12d3-a456-426614174000",
        "plain",
    ] {
        let once = normalize_error(input);
        assert_eq!(normalize_error(&once), once);
    }
}

#[test]
fn sha1_prefix_known_vectors() {
    // sha1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
    assert_eq!(sha1_prefix("abc").as_str(), "a9993e36");
    // sha1("") = da39a3ee5e6b4b0d3255bfef95601890afd80709
    assert_eq!(sha1_prefix("").as_str(), "da39a3ee");
}

#[test]
fn fingerprint_hashes_test_id_newline_normalized_error() {
    let fp = fingerprint("chromium::a.spec.ts::A > b", "\x1b[31mTimeout 5000ms");
    assert_eq!(fp, sha1_prefix("chromium::a.spec.ts::A > b\nTimeout <n>ms"));
    assert_eq!(fp.as_str().len(), 8);
    assert!(
        fp.as_str()
            .chars()
            .all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    );
}

#[test]
fn run_noise_does_not_change_the_fingerprint() {
    let id = test_id(
        "chromium",
        "tests/journeys/checkout.spec.ts",
        "Checkout > pays",
    );
    let a = fingerprint(
        &id,
        "\x1b[31mTimeout 5000ms exceeded waiting for order 9f8e7d6c5b4a\x1b[0m\n  at line 3",
    );
    let b = fingerprint(
        &id,
        "Timeout   7000ms exceeded waiting for order 0a1b2c3d4e5f\n  at line 99",
    );
    assert_eq!(a, b);
}

#[test]
fn different_tests_or_errors_give_different_fingerprints() {
    let a = fingerprint("chromium::a.spec.ts::A", "boom");
    let b = fingerprint("firefox::a.spec.ts::A", "boom");
    let c = fingerprint("chromium::a.spec.ts::A", "bang");
    assert_ne!(a, b);
    assert_ne!(a, c);
}

#[test]
fn from_label_table() {
    let cases: &[(&str, Option<&str>)] = &[
        ("guard-fp-1a2b3c4d", Some("1a2b3c4d")),
        ("guard-fp-00000000", Some("00000000")),
        ("guard-fp-1A2B3C4D", None),
        ("guard-fp-1a2b3c4", None),
        ("guard-fp-1a2b3c4d5", None),
        ("guard-fp-1a2b3c4g", None),
        ("guard-fp-", None),
        ("guard-1a2b3c4d", None),
        ("1a2b3c4d", None),
        (" guard-fp-1a2b3c4d", None),
        ("guard-fp-1a2b3c4d ", None),
        ("", None),
        ("guard-fp-1a2b3c\u{e9}", None),
    ];
    for (label, expected) in cases {
        assert_eq!(
            from_label(label),
            expected.map(|s| Fingerprint(s.to_string())),
            "label: {label:?}"
        );
    }
}

#[test]
fn label_round_trip() {
    let fp = fingerprint("chromium::a::b", "error 1");
    assert_eq!(from_label(&fp.label()), Some(fp));
}
