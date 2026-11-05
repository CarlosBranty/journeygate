use super::*;
use std::ffi::OsString;

fn keys(v: &[&str]) -> Vec<OsString> {
    v.iter().map(OsString::from).collect()
}

#[test]
fn lists_only_e2e_names_sorted_and_unique() {
    let names = e2e_names(keys(&[
        "PATH",
        "E2E_PASSWORD",
        "E2E_USER",
        "E2E_USER",
        "XE2E_A",
    ]));
    assert_eq!(names, vec!["E2E_PASSWORD", "E2E_USER"]);
}

#[test]
fn the_bare_prefix_is_not_a_variable_name() {
    assert!(e2e_names(keys(&["E2E_"])).is_empty());
}

#[test]
fn the_process_snapshot_never_holds_values() {
    // The returned type is names only; a value cannot leak through it.
    let names: Vec<String> = from_process();
    assert!(names.iter().all(|n| n.starts_with(PREFIX)));
}
