//! Rust plugin against `tests/fixtures/rust/` (a two-crate workspace).
mod common;

use common::{index, sorted, strings};
use guard_core::ports::ImpactAnalyzer;
use guard_impact::Language;

#[test]
fn mod_declarations_and_use_paths_create_edges() {
    let idx = index("rust");
    // `mod money;`, `use crate::money::fmt`, `use crate::{money::{self, ..}}` and a
    // sibling workspace crate (`use shop_core::money::fmt`).
    assert_eq!(
        sorted(idx.importers_of("core/src/money.rs")),
        vec![
            "app/src/main.rs",
            "core/src/lib.rs",
            "core/src/orders/line.rs",
            "core/src/orders/mod.rs",
        ]
    );
    // `mod orders;` + `pub use orders::Order` and `crate::orders::Order`.
    assert_eq!(
        sorted(idx.importers_of("core/src/orders/mod.rs")),
        vec!["core/src/lib.rs", "core/src/orders/line.rs"]
    );
    // `mod line;` inside `orders/mod.rs` and `use self::line::Line`.
    assert_eq!(
        idx.importers_of("core/src/orders/line.rs"),
        vec!["core/src/orders/mod.rs"]
    );
    // An item of the crate root resolves to `lib.rs`.
    assert_eq!(
        idx.importers_of("core/src/lib.rs"),
        vec!["core/tests/it.rs"]
    );
}

#[test]
fn std_and_external_crates_are_ignored() {
    let idx = index("rust");
    assert_eq!(idx.imports_of("app/src/main.rs"), vec!["core/src/money.rs"]);
    assert_eq!(idx.language_of("core/src/lib.rs"), Some(Language::Rust));
}

#[test]
fn fan_out_and_transitive_fan_out() {
    let idx = index("rust");
    let facts = idx.fan_out(&strings(&["core/src/money.rs"])).unwrap();
    assert_eq!(facts.max_fan_out, 4);
    assert_eq!(idx.transitive_fan_out("core/src/money.rs"), 5);
}
