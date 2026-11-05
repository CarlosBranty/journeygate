//! TypeScript / JavaScript plugin against `tests/fixtures/typescript/`.
mod common;

use common::{index, sorted, strings};
use guard_core::ports::{Area, ImpactAnalyzer};
use guard_impact::{AnalysisMethod, Language};

#[test]
fn every_relative_import_form_creates_an_edge() {
    let idx = index("typescript");
    // static import, `import x = require()`, ESM `.js` specifier and CommonJS `require`.
    assert_eq!(
        sorted(idx.importers_of("src/lib/money.ts")),
        vec![
            "src/checkout/cart.ts",
            "src/checkout/pay.tsx",
            "src/components/Price.tsx",
            "src/legacy.js",
        ]
    );
    // `import type` and `export * from`.
    assert_eq!(
        sorted(idx.importers_of("src/components/Price.tsx")),
        vec!["src/checkout/cart.ts", "src/components/index.ts"]
    );
    // Directory import resolves to `index.ts`; dynamic `import()` counts.
    assert_eq!(
        idx.importers_of("src/components/index.ts"),
        vec!["src/app.ts"]
    );
    assert_eq!(idx.importers_of("src/checkout/pay.tsx"), vec!["src/app.ts"]);
    assert!(idx.importers_of("src/lib/unused.ts").is_empty());
}

#[test]
fn bare_package_imports_are_ignored() {
    let idx = index("typescript");
    assert_eq!(
        idx.imports_of("src/checkout/cart.ts"),
        vec!["src/components/Price.tsx", "src/lib/money.ts"]
    );
}

#[test]
fn languages_are_detected_per_extension() {
    let idx = index("typescript");
    assert_eq!(idx.language_of("src/app.ts"), Some(Language::TypeScript));
    assert_eq!(
        idx.language_of("src/checkout/pay.tsx"),
        Some(Language::TypeScript)
    );
    assert_eq!(idx.language_of("src/legacy.js"), Some(Language::JavaScript));
    assert_eq!(idx.language_of("target.toml"), None);
}

#[test]
fn transitive_fan_out_follows_importers_of_importers() {
    let idx = index("typescript");
    assert_eq!(idx.direct_fan_out("src/lib/money.ts"), 4);
    assert_eq!(
        sorted(idx.transitive_importers("src/lib/money.ts")),
        vec![
            "src/app.ts",
            "src/checkout/cart.ts",
            "src/checkout/pay.tsx",
            "src/components/Price.tsx",
            "src/components/index.ts",
            "src/legacy.js",
        ]
    );
    assert_eq!(idx.transitive_fan_out("src/lib/money.ts"), 6);
    assert_eq!(idx.transitive_fan_out("src/app.ts"), 0);
}

#[test]
fn fan_out_reports_direct_counts_hard_rules_and_migrations() {
    let idx = index("typescript");
    let facts = idx
        .fan_out(&strings(&[
            "src/lib/money.ts",
            "src/checkout/cart.ts",
            "infra/deploy.sh",
            "db/migrations/001_init.sql",
        ]))
        .unwrap();
    assert_eq!(facts.max_fan_out, 4);
    assert_eq!(facts.fan_out["src/lib/money.ts"], 4);
    assert_eq!(facts.fan_out["src/checkout/cart.ts"], 1);
    assert_eq!(facts.fan_out["infra/deploy.sh"], 0);
    assert_eq!(facts.fan_out["db/migrations/001_init.sql"], 0);
    assert!(facts.human_only_path_hit);
    assert_eq!(facts.human_only_paths, vec!["infra/deploy.sh"]);
    assert!(facts.schema_migration);
}

#[test]
fn areas_and_mentions_resolve_against_the_fixture() {
    let idx = index("typescript");
    assert_eq!(
        idx.areas_for(&strings(&[
            "src/checkout/pay.tsx",
            "src/components/Price.tsx"
        ])),
        vec![Area::new("checkout"), Area::new("ui_general")]
    );
    assert_eq!(
        idx.resolve_mentions(&strings(&[
            "Price",
            "checkout/pay.tsx",
            "src/app.ts",
            "Nope"
        ])),
        vec![
            "src/app.ts",
            "src/checkout/pay.tsx",
            "src/components/Price.tsx"
        ]
    );
}

#[test]
fn file_impacts_carry_transitive_fan_out_and_areas() {
    let idx = index("typescript");
    let impacts = idx.file_impacts(&strings(&["src/lib/money.ts", "src/checkout/cart.ts"]));
    assert_eq!(impacts.len(), 2);
    let cart = &impacts[0];
    assert_eq!(cart.path, "src/checkout/cart.ts");
    assert_eq!(cart.language, Some(Language::TypeScript));
    assert_eq!(cart.method, AnalysisMethod::Graph);
    assert_eq!(cart.direct_fan_out, 1);
    assert_eq!(cart.transitive_fan_out, 2); // pay.tsx, app.ts
    assert_eq!(cart.areas, vec![Area::new("checkout")]);
    let money = &impacts[1];
    assert_eq!(money.transitive_fan_out, 6);
    assert!(money.areas.is_empty());
}
