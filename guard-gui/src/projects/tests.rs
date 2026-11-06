use super::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn files(paths: &[&str]) -> impl Fn(&Path) -> bool {
    let set: BTreeSet<PathBuf> = paths.iter().map(PathBuf::from).collect();
    move |p: &Path| set.contains(p)
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn a_complete_project_has_nothing_missing() {
    let root = p("/w/shop");
    let v = validate(
        &root,
        files(&[
            "/w/shop/target.toml",
            "/w/shop/e2e-playwright/catalog/journeys.json",
        ]),
    );
    assert!(v.is_project());
    assert!(v.is_complete());
    assert_eq!(v.summary(), None);
}

#[test]
fn a_project_without_a_catalog_is_still_a_project() {
    let v = validate(&p("/w/shop"), files(&["/w/shop/target.toml"]));
    assert!(v.is_project());
    assert!(!v.is_complete());
    assert_eq!(v.missing, vec![Missing::Catalog]);
    assert_eq!(
        v.summary().as_deref(),
        Some("missing e2e-playwright/catalog/journeys.json")
    );
}

#[test]
fn a_folder_without_target_toml_is_not_a_project() {
    let v = validate(&p("/w/other"), files(&[]));
    assert!(!v.is_project());
    assert_eq!(v.missing, vec![Missing::TargetToml, Missing::Catalog]);
    assert_eq!(
        v.summary().as_deref(),
        Some("missing target.toml, e2e-playwright/catalog/journeys.json")
    );
}

#[test]
fn touch_puts_the_project_first_and_dedups() {
    let mut r = RecentProjects::default();
    r.touch(p("/a"));
    r.touch(p("/b"));
    r.touch(p("/a"));
    assert_eq!(r.paths(), &[p("/a"), p("/b")]);
    assert_eq!(r.most_recent(), Some(p("/a").as_path()));
}

#[test]
fn the_recent_list_keeps_at_most_ten() {
    let mut r = RecentProjects::default();
    for i in 0..15 {
        r.touch(p(&format!("/p{i}")));
    }
    assert_eq!(r.paths().len(), MAX_RECENT);
    assert_eq!(r.paths()[0], p("/p14"));
    assert_eq!(r.paths()[9], p("/p5"));
}

#[test]
fn remove_drops_a_project() {
    let mut r = RecentProjects::default();
    r.touch(p("/a"));
    r.touch(p("/b"));
    assert!(r.remove(&p("/a")));
    assert!(!r.remove(&p("/zzz")));
    assert_eq!(r.paths(), &[p("/b")]);
}

#[test]
fn from_paths_cleans_a_loaded_list() {
    let r = RecentProjects::from_paths(
        ["/a", "/b", "/a", "", "/c"]
            .iter()
            .map(PathBuf::from)
            .chain((0..20).map(|i| PathBuf::from(format!("/x{i}")))),
    );
    assert_eq!(&r.paths()[..3], &[p("/a"), p("/b"), p("/c")]);
    assert_eq!(r.paths().len(), MAX_RECENT);
}

#[test]
fn path_keys_fold_case_and_separators_when_asked() {
    assert_eq!(
        path_key(Path::new(r"C:\Work\Shop\"), true),
        path_key(Path::new("c:/work/shop"), true)
    );
    assert_ne!(
        path_key(Path::new("/Work/Shop"), false),
        path_key(Path::new("/work/shop"), false)
    );
}

#[test]
fn strip_verbatim_turns_extended_paths_into_plain_ones() {
    assert_eq!(strip_verbatim(p(r"\\?\C:\work\shop")), p(r"C:\work\shop"));
    assert_eq!(
        strip_verbatim(p(r"\\?\UNC\server\share\x")),
        p(r"\\server\share\x")
    );
    assert_eq!(strip_verbatim(p("/home/me/shop")), p("/home/me/shop"));
}

#[test]
fn display_name_is_the_folder_name() {
    assert_eq!(display_name(Path::new("/w/demo-shop")), "demo-shop");
    // A drive path is only a path on Windows; elsewhere it is one file name.
    #[cfg(windows)]
    assert_eq!(display_name(Path::new(r"C:\work\Shop\")), "Shop");
    assert_eq!(display_name(Path::new("/")), "/");
}

#[test]
fn initials_follow_words_and_camel_case() {
    assert_eq!(initials("ShopApi"), "SA");
    assert_eq!(initials("e2e-agentdev"), "EA");
    assert_eq!(initials("shop"), "SH");
    assert_eq!(initials("x"), "X");
    assert_eq!(initials("my_cool app"), "MC");
    assert_eq!(initials(""), "?");
}

#[test]
fn badge_index_is_stable_and_in_range() {
    let a = badge_index("demo-shop", 6);
    assert_eq!(a, badge_index("demo-shop", 6));
    assert!(a < 6);
    assert_eq!(badge_index("anything", 0), 0);
}

#[test]
fn the_repo_flag_wins_then_recent_projects_then_a_project_cwd() {
    let mut recents = RecentProjects::default();
    recents.touch(p("/gone"));
    recents.touch(p("/recent"));
    let is_project = |path: &Path| path == Path::new("/recent") || path == Path::new("/cwd");

    assert_eq!(
        initial_project(Some(p("/flag")), &recents, Path::new("/cwd"), is_project),
        Some(p("/flag"))
    );
    assert_eq!(
        initial_project(None, &recents, Path::new("/cwd"), is_project),
        Some(p("/recent"))
    );
    let empty = RecentProjects::default();
    assert_eq!(
        initial_project(None, &empty, Path::new("/cwd"), is_project),
        Some(p("/cwd"))
    );
    // A cwd that is not a project is never loaded.
    assert_eq!(
        initial_project(None, &empty, Path::new("/elsewhere"), is_project),
        None
    );
}

#[test]
fn parse_branch_reads_git_output() {
    assert_eq!(
        parse_branch("feat/guard-gui\n").as_deref(),
        Some("feat/guard-gui")
    );
    assert_eq!(parse_branch("HEAD\n").as_deref(), Some("detached HEAD"));
    assert_eq!(parse_branch("  \n"), None);
}

#[test]
fn the_current_project_is_matched_by_its_path_key() {
    let current = p(r"C:\Work\Shop");
    assert!(is_current(Some(&current), &p(r"C:\Work\Shop"), true));
    assert!(is_current(Some(&current), &p(r"c:/work/shop/"), true));
    assert!(!is_current(Some(&current), &p(r"c:/work/shop"), false));
    assert!(!is_current(Some(&current), &p(r"C:\Work\Other"), true));
    assert!(!is_current(None, &p(r"C:\Work\Shop"), true));
}
