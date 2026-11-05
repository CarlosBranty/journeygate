use super::*;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn exists_in(files: &'static [&'static str]) -> impl Fn(&Path) -> bool {
    move |p: &Path| files.iter().any(|f| Path::new(f) == p)
}

#[test]
fn guard_bin_wins_when_it_points_at_a_file() {
    let found = locate(
        Some(OsString::from("/opt/guard")),
        Some(OsString::from("/usr/bin")),
        None,
        exists_in(&["/opt/guard", "/usr/bin/guard"]),
    );
    assert_eq!(found, Ok(PathBuf::from("/opt/guard")));
}

#[test]
fn a_guard_bin_that_does_not_exist_is_reported_not_skipped() {
    let found = locate(
        Some(OsString::from("/nope/guard")),
        Some(OsString::from("/usr/bin")),
        None,
        exists_in(&["/usr/bin/guard"]),
    );
    assert_eq!(found, Err(NotFound::GuardBin(PathBuf::from("/nope/guard"))));
}

#[test]
fn an_empty_guard_bin_falls_back_to_path() {
    let path = std::env::join_paths(["/a", "/b"]).unwrap();
    let found = locate(
        Some(OsString::new()),
        Some(path),
        None,
        exists_in(&["/b/guard", "/b/guard.exe"]),
    );
    assert_eq!(found, Ok(Path::new("/b").join(EXE_NAME)));
}

#[test]
fn falls_back_to_the_directory_of_this_executable() {
    let found = locate(
        None,
        None,
        Some(PathBuf::from("/release")),
        exists_in(&["/release/guard", "/release/guard.exe"]),
    );
    assert_eq!(found, Ok(Path::new("/release").join(EXE_NAME)));
}

#[test]
fn nothing_found_is_reported() {
    let found = locate(None, Some(OsString::from("/a")), None, |_| false);
    assert_eq!(found, Err(NotFound::Nowhere));
}
