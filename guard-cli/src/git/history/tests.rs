use super::*;
use std::path::PathBuf;

/// git with a fixed identity and dates, local file transport allowed.
fn g(dir: &Path, args: &[&str]) -> String {
    g_as(dir, "t", "1700000000 +0000", args)
}

fn g_as(dir: &Path, author: &str, date: &str, args: &[&str]) -> String {
    let name = format!("user.name={author}");
    let mut all = vec![
        "-c",
        &name,
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
    let mut cmd = super::super::git_command(Some(dir));
    cmd.args(&all)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date);
    let out = super::super::finish(cmd, std::time::Duration::from_secs(60)).unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn guard_base_from_the_environment_is_honoured() {
    let u = umbrella();
    let env = |name: &str| (name == "GUARD_BASE").then(|| "HEAD".to_string());
    let base = branch_base(&u.root, None, &env).unwrap();
    assert_eq!(base.reference, "HEAD");
}

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn commit(dir: &Path, author: &str, at: u64, message: &str) {
    let date = format!("{at} +0000");
    g_as(dir, author, &date, &["add", "-A"]);
    g_as(dir, author, &date, &["commit", "-q", "-m", message]);
}

#[test]
fn log_records_are_parsed_with_their_files() {
    let bytes = b"\x1eaaaa1111\x1faaaa111\x1fJane Doe\x1f1700000000\x1ffix: total\n\nweb/a.ts\nweb/b.ts\n\x1ebbbb\x1fbbb\x1fJo\x1f1600000000\x1fempty\n";
    let commits = parse_log(bytes);
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, "aaaa1111");
    assert_eq!(commits[0].short, "aaaa111");
    assert_eq!(commits[0].author, "Jane Doe");
    assert_eq!(commits[0].timestamp, 1_700_000_000);
    assert_eq!(commits[0].subject, "fix: total");
    assert_eq!(commits[0].files, ["web/a.ts", "web/b.ts"]);
    assert!(commits[1].files.is_empty());
}

#[test]
fn quoted_log_paths_are_unquoted() {
    let bytes = b"\x1ea\x1fa\x1fx\x1f1\x1fs\n\n\"web/a\\\"b.ts\"\n";
    assert_eq!(parse_log(bytes)[0].files, ["web/a\"b.ts"]);
}

#[test]
fn status_z_lists_changed_untracked_and_renamed_paths() {
    let bytes = b" M web/a.ts\0?? web/new.ts\0R  web/to.ts\0web/from.ts\0A  web/c.ts\0";
    assert_eq!(
        parse_status_z(bytes),
        [
            "web/a.ts",
            "web/new.ts",
            "web/to.ts",
            "web/from.ts",
            "web/c.ts"
        ]
    );
}

#[test]
fn blame_porcelain_gives_author_time_subject_and_content() {
    let text = "abcdef1234567890abcdef1234567890abcdef12 3 157 1\nauthor Jane Doe\nauthor-mail <j@x>\nauthor-time 1700000000\nauthor-tz +0000\nsummary test: protect Mecanica\nfilename tests/a.spec.ts\n\texpect(errors).toContain('x');\n";
    let line = parse_blame(text).unwrap();
    assert_eq!(line.short, "abcdef1");
    assert_eq!(line.author.as_deref(), Some("Jane Doe"));
    assert_eq!(line.timestamp, 1_700_000_000);
    assert_eq!(line.subject, "test: protect Mecanica");
    assert_eq!(line.content, "expect(errors).toContain('x');");
    assert!(!line.uncommitted);
}

#[test]
fn blame_of_an_uncommitted_line_has_no_author() {
    let text = "0000000000000000000000000000000000000000 1 1 1\nauthor Not Committed Yet\nauthor-time 1700000000\nsummary Version of a.ts from a.ts\nfilename a.ts\n\tx\n";
    let line = parse_blame(text).unwrap();
    assert!(line.uncommitted);
    assert_eq!(line.author, None);
    assert!(parse_blame("").is_none());
}

/// An umbrella `main` with `web/app.ts` and the submodule `api` (its own
/// `main` with `src/a.cs`); both then branch to `feature`.
struct Umbrella {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
}

fn umbrella() -> Umbrella {
    let source = tempfile::TempDir::new().unwrap();
    g(source.path(), &["init", "-q", "-b", "main"]);
    write(source.path(), "src/a.cs", "a\n");
    commit(source.path(), "Ana", 1_600_000_000, "api: init");
    let top = tempfile::TempDir::new().unwrap();
    let root = top.path().to_path_buf();
    g(&root, &["init", "-q", "-b", "main"]);
    write(&root, "web/app.ts", "w\n");
    commit(&root, "Ana", 1_600_000_000, "init");
    let url = source.path().to_string_lossy().replace('\\', "/");
    g(&root, &["submodule", "add", "-q", &url, "api"]);
    commit(&root, "Ana", 1_600_000_100, "add submodule");
    g(&root, &["checkout", "-q", "-b", "feature"]);
    g(&root.join("api"), &["checkout", "-q", "-b", "feature"]);
    Umbrella {
        _dirs: vec![source, top],
        root,
    }
}

#[test]
fn repositories_lists_the_superproject_then_initialized_submodules() {
    let u = umbrella();
    let repos = repositories(&u.root);
    let prefixes: Vec<&str> = repos.iter().map(|r| r.prefix.as_str()).collect();
    assert_eq!(prefixes, ["", "api/"]);
    assert_eq!(repos[1].dir, u.root.join("api"));
}

#[test]
fn branch_history_reads_each_repository_against_its_own_base() {
    let u = umbrella();
    let api = u.root.join("api");
    write(&api, "src/a.cs", "changed\n");
    commit(&api, "Bob", 1_700_000_000, "api: change a");
    write(&u.root, "web/app.ts", "changed\n");
    commit(&u.root, "Cara", 1_700_000_500, "web: change app");

    let root_base = branch_base(&u.root, None, &no_env).unwrap();
    assert_eq!(root_base.reference, "main");
    let commits = log(&u.root, Some(&root_base.merge_base), None, &[]).unwrap();
    assert_eq!(commits.len(), 1, "{commits:?}");
    assert_eq!(commits[0].author, "Cara");
    assert_eq!(commits[0].files, ["web/app.ts"]);

    let api_base = branch_base(&api, None, &no_env).unwrap();
    let commits = log(&api, Some(&api_base.merge_base), None, &[]).unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].subject, "api: change a");
    assert_eq!(commits[0].files, ["src/a.cs"]);
}

#[test]
fn an_explicit_base_wins_and_an_unknown_one_is_an_error() {
    let u = umbrella();
    let base = branch_base(&u.root, Some("main"), &no_env).unwrap();
    assert_eq!(base.reference, "main");
    let error = branch_base(&u.root, Some("nope"), &no_env).unwrap_err();
    assert!(error.contains("nope"), "{error}");
}

#[test]
fn recent_history_follows_pathspecs_and_a_limit() {
    let u = umbrella();
    write(&u.root, "web/other.ts", "o\n");
    commit(&u.root, "Dan", 1_700_000_000, "web: other");
    let commits = log(&u.root, None, Some(5), &[":(glob)web/app.ts".to_string()]).unwrap();
    let subjects: Vec<&str> = commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["init"]);
}

#[test]
fn uncommitted_lists_worktree_staged_and_untracked_paths_of_one_repository() {
    let u = umbrella();
    write(&u.root, "web/app.ts", "dirty\n");
    write(&u.root, "web/new.ts", "n\n");
    write(&u.root.join("api"), "src/a.cs", "dirty\n");
    let mut root = uncommitted(&u.root).unwrap();
    root.sort();
    // The submodule's own change is not the superproject's.
    assert_eq!(root, ["web/app.ts", "web/new.ts"]);
    assert_eq!(uncommitted(&u.root.join("api")).unwrap(), ["src/a.cs"]);
}

#[test]
fn blame_names_who_last_changed_a_line() {
    let u = umbrella();
    write(&u.root, "web/app.ts", "one\ntwo\n");
    commit(&u.root, "Eve", 1_700_000_000, "web: second line");
    let line = blame_line(&u.root, "web/app.ts", 2).unwrap();
    assert_eq!(line.author.as_deref(), Some("Eve"));
    assert_eq!(line.subject, "web: second line");
    assert_eq!(line.content, "two");
    write(&u.root, "web/app.ts", "one\nthree\n");
    let dirty = blame_line(&u.root, "web/app.ts", 2).unwrap();
    assert!(dirty.uncommitted);
    // A line past the end, or a file git does not know, is no blame.
    assert!(blame_line(&u.root, "web/app.ts", 99).is_err());
    write(&u.root, "web/untracked.ts", "x\n");
    assert!(blame_line(&u.root, "web/untracked.ts", 1).is_err());
}
