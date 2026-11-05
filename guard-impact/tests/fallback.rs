//! A language without a plugin falls back to the `target.toml` globs.
mod common;

use common::{index, strings};
use guard_core::ports::{Area, ImpactAnalyzer};
use guard_impact::AnalysisMethod;

#[test]
fn files_without_a_plugin_get_glob_fallback() {
    let idx = index("fallback");
    assert_eq!(idx.fallback_languages(), ["java".to_string()]);
    let changed = strings(&["src/main/java/shop/Money.java"]);
    let facts = idx.fan_out(&changed).unwrap();
    assert_eq!(facts.max_fan_out, 0);
    assert_eq!(facts.fan_out["src/main/java/shop/Money.java"], 0);
    assert_eq!(idx.areas_for(&changed), vec![Area::new("shop")]);
    let impacts = idx.file_impacts(&changed);
    assert_eq!(impacts[0].method, AnalysisMethod::GlobFallback);
    assert_eq!(impacts[0].language, None);
    assert_eq!(impacts[0].areas, vec![Area::new("shop")]);
}
