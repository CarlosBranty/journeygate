//! Go plugin against `tests/fixtures/go/`.
mod common;

use common::{index, sorted, strings};
use guard_core::ports::ImpactAnalyzer;
use guard_impact::Language;

#[test]
fn a_package_import_reaches_every_non_test_file_of_the_package() {
    let idx = index("go");
    let expected = vec![
        "cmd/report/main.go",
        "internal/orders/orders.go",
        "internal/orders/orders_test.go",
    ];
    assert_eq!(
        sorted(idx.importers_of("internal/money/money.go")),
        expected
    );
    assert_eq!(
        sorted(idx.importers_of("internal/money/format.go")),
        expected
    );
    assert!(idx.importers_of("internal/money/money_test.go").is_empty());
    assert_eq!(
        idx.importers_of("internal/orders/orders.go"),
        vec!["main.go"]
    );
}

#[test]
fn stdlib_imports_are_ignored() {
    let idx = index("go");
    assert!(idx.imports_of("internal/money/format.go").is_empty());
    assert_eq!(idx.language_of("main.go"), Some(Language::Go));
}

#[test]
fn fan_out_and_transitive_fan_out() {
    let idx = index("go");
    let facts = idx.fan_out(&strings(&["internal/money/money.go"])).unwrap();
    assert_eq!(facts.max_fan_out, 3);
    assert_eq!(idx.transitive_fan_out("internal/money/money.go"), 4);
}
