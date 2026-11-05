use super::*;
use std::path::PathBuf;

/// git with a fixed identity, local file transport allowed for submodules.
fn g(dir: &Path, args: &[&str]) -> String {
    let mut all = vec![
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "protocol.file.allow=always",
        "-c",
        "init.defaultBranch=main",
    ];
    all.extend_from_slice(args);
    super::super::text(dir, &all).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
}

impl Fixture {
    fn head(&self) -> String {
        g(&self.root, &["rev-parse", "HEAD"])
    }

    fn sub(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn commit_all(&self, dir: &Path, message: &str) {
        g(dir, &["add", "-A"]);
        g(dir, &["commit", "-q", "-m", message]);
    }
}

/// A repository with `files`, one commit on `main`.
fn source_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    g(dir.path(), &["init", "-q", "-b", "main"]);
    for (rel, body) in files {
        write(dir.path(), rel, body);
    }
    g(dir.path(), &["add", "-A"]);
    g(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

/// An umbrella with `web/app.ts` and the submodule `frontend` holding
/// `README.md` and `src/a.ts`.
fn umbrella() -> Fixture {
    umbrella_with("frontend", &[("README.md", "r\n"), ("src/a.ts", "a\n")])
}

fn umbrella_with(path: &str, files: &[(&str, &str)]) -> Fixture {
    let source = source_repo(files);
    let top = tempfile::TempDir::new().unwrap();
    let root = top.path().to_path_buf();
    g(&root, &["init", "-q", "-b", "main"]);
    write(&root, "web/app.ts", "w\n");
    g(&root, &["add", "-A"]);
    g(&root, &["commit", "-q", "-m", "init"]);
    g(
        &root,
        &["submodule", "add", "-q", &slash(source.path()), path],
    );
    g(&root, &["commit", "-q", "-m", "add submodule"]);
    Fixture {
        _dirs: vec![source, top],
        root,
    }
}

fn changed(f: &Fixture, from: &str, scope: Scope) -> PathSet {
    changed_paths(&f.root, from, scope).unwrap()
}

fn paths(set: &PathSet) -> Vec<&str> {
    set.paths.iter().map(String::as_str).collect()
}

#[test]
fn worktree_edit_inside_submodule_is_prefixed() {
    let f = umbrella();
    write(&f.sub("frontend"), "src/a.ts", "changed\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/src/a.ts"]);
    assert!(set.warnings.is_empty(), "{:?}", set.warnings);
}

#[test]
fn staged_edit_inside_submodule_is_prefixed() {
    let f = umbrella();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "changed\n");
    g(&sub, &["add", "-A"]);
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/src/a.ts"]);
}

#[test]
fn untracked_file_inside_submodule_is_listed() {
    let f = umbrella();
    write(&f.sub("frontend"), "src/new.ts", "n\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/src/new.ts"]);
}

#[test]
fn inner_commit_not_added_to_superproject_is_listed() {
    let f = umbrella();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "changed\n");
    f.commit_all(&sub, "inner");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/src/a.ts"]);
    // Committed scope does not see the working tree.
    assert!(changed(&f, &f.head(), Scope::Committed).paths.is_empty());
}

#[test]
fn committed_pointer_bump_lists_inner_range() {
    let f = umbrella();
    let base = f.head();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "changed\n");
    write(&sub, "src/b.ts", "b\n");
    f.commit_all(&sub, "inner");
    write(&f.root, "web/app.ts", "w2\n");
    f.commit_all(&f.root, "bump");
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(
        paths(&set),
        ["frontend/src/a.ts", "frontend/src/b.ts", "web/app.ts"]
    );
}

#[test]
fn pointer_rollback_and_diverged_pointer_list_two_dot_diff() {
    let f = umbrella();
    let sub = f.sub("frontend");
    let first = g(&sub, &["rev-parse", "HEAD"]);
    write(&sub, "src/a.ts", "v2\n");
    f.commit_all(&sub, "v2");
    f.commit_all(&f.root, "bump to v2");
    let at_v2 = f.head();
    // Rollback: the pointer goes back to the first commit.
    g(&sub, &["checkout", "-q", &first]);
    f.commit_all(&f.root, "rollback");
    assert_eq!(
        paths(&changed(&f, &at_v2, Scope::Committed)),
        ["frontend/src/a.ts"]
    );
    // Diverged: a sibling of v2 that touches another file.
    write(&sub, "src/c.ts", "c\n");
    f.commit_all(&sub, "sibling");
    f.commit_all(&f.root, "diverge");
    assert_eq!(
        paths(&changed(&f, &at_v2, Scope::Committed)),
        ["frontend/src/a.ts", "frontend/src/c.ts"]
    );
}

#[test]
fn added_submodule_lists_all_its_files_and_gitmodules() {
    let f = umbrella();
    let before_add = g(&f.root, &["rev-parse", "HEAD~1"]);
    let set = changed(&f, &before_add, Scope::Committed);
    assert_eq!(
        paths(&set),
        [".gitmodules", "frontend/README.md", "frontend/src/a.ts"]
    );
}

#[test]
fn removed_submodule_lists_old_files_from_modules_store() {
    let f = umbrella();
    let base = f.head();
    g(&f.root, &["rm", "-q", "frontend"]);
    g(&f.root, &["commit", "-q", "-m", "remove"]);
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(
        paths(&set),
        [".gitmodules", "frontend/README.md", "frontend/src/a.ts"]
    );
    assert!(set.warnings.is_empty(), "{:?}", set.warnings);
}

#[test]
fn removed_submodule_without_its_store_is_a_subtree_entry() {
    let f = umbrella();
    let base = f.head();
    g(&f.root, &["rm", "-q", "frontend"]);
    g(&f.root, &["commit", "-q", "-m", "remove"]);
    let modules = PathBuf::from(g(&f.root, &["rev-parse", "--git-common-dir"]));
    let modules = if modules.is_absolute() {
        modules
    } else {
        f.root.join(modules)
    };
    std::fs::remove_dir_all(modules.join("modules")).unwrap();
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(paths(&set), [".gitmodules", "frontend/"]);
    assert_eq!(set.warnings.len(), 1, "{:?}", set.warnings);
    assert!(
        set.warnings[0].starts_with("submodule frontend:"),
        "{:?}",
        set.warnings
    );
}

#[test]
fn uninitialized_changed_submodule_is_subtree_entry_with_warning() {
    let f = umbrella();
    let base = f.head();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "changed\n");
    f.commit_all(&sub, "inner");
    f.commit_all(&f.root, "bump");
    g(&f.root, &["submodule", "deinit", "-q", "-f", "frontend"]);
    let set = changed(&f, &base, Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/"]);
    assert_eq!(
        set.warnings,
        ["submodule frontend is not initialized; treating it as entirely changed"]
    );
}

#[test]
fn unchanged_uninitialized_submodule_contributes_nothing() {
    let f = umbrella();
    g(&f.root, &["submodule", "deinit", "-q", "-f", "frontend"]);
    write(&f.root, "web/app.ts", "w2\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["web/app.ts"]);
    assert!(set.warnings.is_empty(), "{:?}", set.warnings);
}

#[test]
fn missing_pointer_commit_is_subtree_entry_with_warning() {
    let f = umbrella();
    let base = f.head();
    let fake = "1234567890123456789012345678901234567890";
    g(
        &f.root,
        &[
            "update-index",
            "--cacheinfo",
            &format!("160000,{fake},frontend"),
        ],
    );
    g(&f.root, &["commit", "-q", "-m", "dangling pointer"]);
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(paths(&set), ["frontend/"]);
    assert_eq!(
        set.warnings,
        ["submodule frontend: commit 1234567 not present; treating it as entirely changed"]
    );
}

#[test]
fn nested_submodule_paths_carry_full_prefix() {
    let lib = source_repo(&[("x.ts", "x\n")]);
    let front = source_repo(&[("src/a.ts", "a\n")]);
    g(
        front.path(),
        &["submodule", "add", "-q", &slash(lib.path()), "lib"],
    );
    g(front.path(), &["commit", "-q", "-m", "add lib"]);
    let top = tempfile::TempDir::new().unwrap();
    let root = top.path().to_path_buf();
    g(&root, &["init", "-q", "-b", "main"]);
    g(
        &root,
        &["submodule", "add", "-q", &slash(front.path()), "frontend"],
    );
    g(
        &root,
        &["submodule", "update", "-q", "--init", "--recursive"],
    );
    g(&root, &["commit", "-q", "-m", "add frontend"]);
    let f = Fixture {
        _dirs: vec![lib, front, top],
        root,
    };
    write(&f.sub("frontend/lib"), "x.ts", "changed\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/lib/x.ts"]);
    // tracked paths walk both levels.
    let tracked = tracked_paths(&f.root).unwrap();
    assert_eq!(
        paths(&tracked),
        [
            ".gitmodules",
            "frontend/.gitmodules",
            "frontend/lib/x.ts",
            "frontend/src/a.ts"
        ]
    );
}

#[test]
fn paths_with_spaces_and_unicode_survive() {
    let f = umbrella_with("deep mod", &[("ñ sp/a b.ts", "a\n")]);
    write(&f.sub("deep mod"), "ñ sp/a b.ts", "changed\n");
    write(&f.root, "top ñ.md", "t\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["deep mod/ñ sp/a b.ts", "top ñ.md"]);
}

#[test]
fn moved_submodule_reports_old_and_new_trees() {
    let f = umbrella();
    let base = f.head();
    std::fs::create_dir_all(f.root.join("apps")).unwrap();
    g(&f.root, &["mv", "frontend", "apps/frontend"]);
    g(&f.root, &["commit", "-q", "-m", "move"]);
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(
        paths(&set),
        [
            ".gitmodules",
            "apps/frontend/README.md",
            "apps/frontend/src/a.ts",
            "frontend/README.md",
            "frontend/src/a.ts"
        ]
    );
}

#[test]
fn gitlink_without_gitmodules_entry_is_expanded_or_subtree() {
    let top = tempfile::TempDir::new().unwrap();
    let root = top.path().to_path_buf();
    g(&root, &["init", "-q", "-b", "main"]);
    let inner = root.join("vendor");
    std::fs::create_dir_all(&inner).unwrap();
    g(&inner, &["init", "-q", "-b", "main"]);
    write(&inner, "lib.ts", "l\n");
    g(&inner, &["add", "-A"]);
    g(&inner, &["commit", "-q", "-m", "inner"]);
    g(&root, &["add", "vendor"]);
    g(&root, &["commit", "-q", "-m", "embedded"]);
    let f = Fixture {
        _dirs: vec![top],
        root,
    };
    write(&inner, "lib.ts", "changed\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["vendor/lib.ts"]);
    // Removed with no `.gitmodules` name and no modules store: subtree entry.
    let base = f.head();
    g(&f.root, &["rm", "-q", "--cached", "vendor"]);
    g(&f.root, &["commit", "-q", "-m", "drop"]);
    std::fs::remove_dir_all(&inner).unwrap();
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(paths(&set), ["vendor/"]);
    assert_eq!(set.warnings.len(), 1, "{:?}", set.warnings);
}

#[test]
fn untracked_nested_repo_is_expanded() {
    let f = umbrella();
    let tools = f.root.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    g(&tools, &["init", "-q", "-b", "main"]);
    write(&tools, "run.sh", "r\n");
    write(&tools, "lib/x.sh", "x\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["tools/lib/x.sh", "tools/run.sh"]);
}

#[test]
fn untracked_plain_dir_still_lists_files() {
    let f = umbrella();
    write(&f.root, "notes/a.md", "a\n");
    write(&f.root, "notes/deep/b.md", "b\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["notes/a.md", "notes/deep/b.md"]);
}

#[test]
fn monorepo_without_gitlinks_runs_only_base_commands() {
    let dir = source_repo(&[("web/a.ts", "a\n"), ("svc/b.go", "b\n")]);
    let root = dir.path();
    let base = g(root, &["rev-parse", "HEAD"]);
    write(root, "svc/b.go", "b2\n");
    g(root, &["commit", "-q", "-am", "b2"]);
    write(root, "web/a.ts", "a2\n");
    write(root, "new.md", "n\n");
    let before = super::super::spawn_count();
    let set = changed_paths(root, &base, Scope::CommittedAndWorktree).unwrap();
    assert_eq!(super::super::spawn_count() - before, 3);
    assert_eq!(paths(&set), ["svc/b.go", "web/a.ts", "new.md"]);
}

#[test]
fn ignore_all_in_gitmodules_does_not_hide_changes() {
    let f = umbrella();
    g(
        &f.root,
        &[
            "config",
            "-f",
            ".gitmodules",
            "submodule.frontend.ignore",
            "all",
        ],
    );
    g(&f.root, &["config", "diff.ignoreSubmodules", "all"]);
    f.commit_all(&f.root, "ignore all");
    write(&f.sub("frontend"), "src/a.ts", "changed\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/src/a.ts"]);
}

#[test]
fn deleted_files_count() {
    let f = umbrella();
    std::fs::remove_file(f.sub("frontend").join("src/a.ts")).unwrap();
    std::fs::remove_file(f.root.join("web/app.ts")).unwrap();
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(paths(&set), ["frontend/src/a.ts", "web/app.ts"]);
}

#[test]
fn tracked_paths_expand_initialized_and_mark_uninitialized() {
    let f = umbrella();
    let tracked = tracked_paths(&f.root).unwrap();
    assert_eq!(
        paths(&tracked),
        [
            ".gitmodules",
            "frontend/README.md",
            "frontend/src/a.ts",
            "web/app.ts"
        ]
    );
    g(&f.root, &["submodule", "deinit", "-q", "-f", "frontend"]);
    let tracked = tracked_paths(&f.root).unwrap();
    assert_eq!(paths(&tracked), [".gitmodules", "frontend/", "web/app.ts"]);
}

#[test]
fn listed_paths_include_untracked_and_submodule_files() {
    let f = umbrella();
    write(&f.root, "new.md", "n\n");
    write(&f.sub("frontend"), "src/new.ts", "n\n");
    let listed = listed_paths(&f.root).unwrap();
    assert_eq!(
        paths(&listed),
        [
            ".gitmodules",
            "frontend/README.md",
            "frontend/src/a.ts",
            "frontend/src/new.ts",
            "web/app.ts",
            "new.md"
        ]
    );
}

#[test]
fn expand_gitlinks_replaces_pointer_entries() {
    let f = umbrella();
    let base = f.head();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "changed\n");
    f.commit_all(&sub, "inner");
    write(&f.root, "web/app.ts", "w2\n");
    f.commit_all(&f.root, "bump");
    let list = vec!["frontend".to_string(), "web/app.ts".to_string()];
    let set = expand_gitlinks(&f.root, Some(&base), &list).unwrap();
    assert_eq!(paths(&set), ["frontend/src/a.ts", "web/app.ts"]);
    let set = expand_gitlinks(&f.root, None, &list).unwrap();
    assert_eq!(paths(&set), ["frontend/", "web/app.ts"]);
}

#[test]
fn too_deep_nesting_is_a_subtree_entry() {
    let f = umbrella();
    write(&f.sub("frontend"), "src/a.ts", "changed\n");
    let mut set = PathSet::default();
    let walk = Walk { max_depth: 0 };
    walk.worktree_gitlink(
        &f.root,
        "",
        &RawEntry {
            old_mode: "160000".into(),
            new_mode: "160000".into(),
            old_sha: g(&f.root, &["rev-parse", "HEAD:frontend"]),
            new_sha: ZERO.into(),
            path: "frontend".into(),
        },
        1,
        &mut set,
    )
    .unwrap();
    assert_eq!(paths(&set), ["frontend/"]);
    assert_eq!(set.warnings.len(), 1, "{:?}", set.warnings);
    assert!(
        set.warnings[0].contains("nested deeper than"),
        "{:?}",
        set.warnings
    );
}

#[test]
fn diff_text_includes_prefixed_submodule_hunks() {
    let f = umbrella();
    let base = f.head();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "committed\n");
    f.commit_all(&sub, "inner");
    f.commit_all(&f.root, "bump");
    write(&sub, "README.md", "dirty\n");
    let text = super::super::diff_text(&f.root, &base).unwrap();
    assert!(
        text.contains("diff --git a/frontend/src/a.ts b/frontend/src/a.ts"),
        "{text}"
    );
    assert!(
        text.contains("diff --git a/frontend/README.md b/frontend/README.md"),
        "{text}"
    );
}

#[test]
fn wrappers_go_through_the_walker() {
    let f = umbrella();
    let base = f.head();
    let sub = f.sub("frontend");
    write(&sub, "src/a.ts", "committed\n");
    f.commit_all(&sub, "inner");
    f.commit_all(&f.root, "bump");
    write(&sub, "README.md", "dirty\n");
    assert_eq!(
        super::super::committed_changes(&f.root, &base).unwrap(),
        ["frontend/src/a.ts"]
    );
    let mb = super::super::merge_base(&f.root, &base).unwrap();
    assert_eq!(
        paths(&changed(&f, &mb, Scope::CommittedAndWorktree)),
        ["frontend/src/a.ts", "frontend/README.md"]
    );
    assert_eq!(
        super::super::tracked_files(&f.root).unwrap(),
        [
            ".gitmodules",
            "frontend/README.md",
            "frontend/src/a.ts",
            "web/app.ts"
        ]
    );
}

/// `front` holding `src/a.ts` and the submodule `libs/core` (`core/x.ts`).
fn front_with_core() -> (tempfile::TempDir, tempfile::TempDir) {
    let core = source_repo(&[("core/x.ts", "x\n")]);
    let front = source_repo(&[("src/a.ts", "a\n")]);
    g(
        front.path(),
        &["submodule", "add", "-q", &slash(core.path()), "libs/core"],
    );
    g(front.path(), &["commit", "-q", "-m", "add core"]);
    (core, front)
}

fn bare_umbrella() -> (tempfile::TempDir, PathBuf) {
    let top = tempfile::TempDir::new().unwrap();
    let root = top.path().to_path_buf();
    g(&root, &["init", "-q", "-b", "main"]);
    write(&root, "web/app.ts", "w\n");
    g(&root, &["add", "-A"]);
    g(&root, &["commit", "-q", "-m", "init"]);
    (top, root)
}

#[test]
fn staged_new_submodule_expands_its_nested_submodules() {
    let (core, front) = front_with_core();
    let (top, root) = bare_umbrella();
    g(
        &root,
        &["submodule", "add", "-q", &slash(front.path()), "frontend"],
    );
    let f = Fixture {
        _dirs: vec![core, front, top],
        root,
    };
    // Not initialized yet: the nested submodule is one subtree entry.
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(
        paths(&set),
        [
            ".gitmodules",
            "frontend/.gitmodules",
            "frontend/libs/core/",
            "frontend/src/a.ts"
        ]
    );
    assert_eq!(
        set.warnings,
        ["submodule frontend/libs/core is not initialized; treating it as entirely changed"]
    );
    g(&f.sub("frontend"), &["submodule", "update", "-q", "--init"]);
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(
        paths(&set),
        [
            ".gitmodules",
            "frontend/.gitmodules",
            "frontend/libs/core/core/x.ts",
            "frontend/src/a.ts"
        ]
    );
    assert!(set.warnings.is_empty(), "{:?}", set.warnings);
    // Committed, the same tree reports the same paths.
    let base = f.head();
    g(&f.root, &["commit", "-q", "-m", "add frontend"]);
    let set = changed(&f, &base, Scope::Committed);
    assert_eq!(
        paths(&set),
        [
            ".gitmodules",
            "frontend/.gitmodules",
            "frontend/libs/core/core/x.ts",
            "frontend/src/a.ts"
        ]
    );
}

#[test]
fn untracked_nested_repo_expands_its_submodules() {
    let (core, front) = front_with_core();
    let (top, root) = bare_umbrella();
    g(
        &root,
        &[
            "clone",
            "-q",
            "--recurse-submodules",
            &slash(front.path()),
            "tools",
        ],
    );
    let f = Fixture {
        _dirs: vec![core, front, top],
        root,
    };
    write(&f.sub("tools/libs/core"), "core/x.ts", "changed\n");
    let set = changed(&f, &f.head(), Scope::CommittedAndWorktree);
    assert_eq!(
        paths(&set),
        [
            "tools/.gitmodules",
            "tools/libs/core/core/x.ts",
            "tools/src/a.ts"
        ]
    );
    assert!(set.warnings.is_empty(), "{:?}", set.warnings);
    let listed = listed_paths(&f.root).unwrap();
    assert_eq!(
        paths(&listed),
        [
            "web/app.ts",
            "tools/.gitmodules",
            "tools/libs/core/core/x.ts",
            "tools/src/a.ts"
        ]
    );
}
