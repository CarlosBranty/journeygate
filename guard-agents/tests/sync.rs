//! `guard agents sync`: one source rendered per runtime, idempotent, golden-tested.
//! Regenerate the goldens with `UPDATE_GOLDEN=1 cargo test -p guard-agents --test sync`.

use guard_agents::config::RuntimeKind;
use guard_agents::sync::{BLOCK_BEGIN, Source, SyncError, SyncOptions, sync, sync_mcp};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_tree(&manifest().join("tests/fixtures/sync"), dir.path());
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&dest).unwrap();
            copy_tree(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// Every file under `root` except the source and `.gitattributes`, by relative path.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if rel.starts_with("agents/") || rel == ".gitattributes" {
                    continue;
                }
                out.insert(rel, fs::read_to_string(&p).unwrap().replace("\r\n", "\n"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn all_runtimes() -> SyncOptions {
    SyncOptions {
        runtimes: [
            RuntimeKind::ClaudeCode,
            RuntimeKind::Codex,
            RuntimeKind::CopilotCli,
            RuntimeKind::MiniSweAgent,
        ]
        .into_iter()
        .collect(),
        check: false,
    }
}

#[test]
fn renders_the_golden_tree() {
    let repo = fixture_repo();
    let report = sync(repo.path(), &all_runtimes()).unwrap();
    assert!(!report.written.is_empty());
    let actual = tree(repo.path());
    let golden_dir = manifest().join("tests/golden/sync");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        let _ = fs::remove_dir_all(&golden_dir);
        for (rel, body) in &actual {
            let p = golden_dir.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
    }
    let golden = tree(&golden_dir);
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        golden.keys().collect::<Vec<_>>(),
        "rendered file set"
    );
    for (rel, body) in &golden {
        assert_eq!(&actual[rel], body, "{rel} differs from its golden");
    }
}

#[test]
fn second_sync_changes_nothing_and_check_is_clean() {
    let repo = fixture_repo();
    sync(repo.path(), &all_runtimes()).unwrap();
    let before = tree(repo.path());
    let again = sync(repo.path(), &all_runtimes()).unwrap();
    assert!(again.written.is_empty(), "{:?}", again.written);
    assert!(again.removed.is_empty());
    assert!(again.is_clean());
    assert_eq!(tree(repo.path()), before);
    let check = sync(
        repo.path(),
        &SyncOptions {
            check: true,
            ..all_runtimes()
        },
    )
    .unwrap();
    assert!(check.is_clean());
}

#[test]
fn check_mode_reports_drift_and_writes_nothing() {
    let repo = fixture_repo();
    let opts = SyncOptions {
        check: true,
        ..all_runtimes()
    };
    let drift = sync(repo.path(), &opts).unwrap();
    assert!(!drift.is_clean());
    assert!(drift.written.contains(&"AGENTS.md".to_string()));
    assert!(tree(repo.path()).is_empty(), "check mode never writes");

    sync(repo.path(), &all_runtimes()).unwrap();
    let skill = repo.path().join(".claude/skills/alpha/SKILL.md");
    fs::write(&skill, "hand edited").unwrap();
    let drift = sync(repo.path(), &opts).unwrap();
    assert_eq!(
        drift.written,
        vec![".claude/skills/alpha/SKILL.md".to_string()]
    );
    assert_eq!(fs::read_to_string(&skill).unwrap(), "hand edited");
}

#[test]
fn crlf_checkouts_are_not_drift() {
    let repo = fixture_repo();
    sync(repo.path(), &all_runtimes()).unwrap();
    // What git autocrlf does on Windows to generated and source files.
    for rel in [
        "AGENTS.md",
        ".claude/skills/alpha/SKILL.md",
        "agents/AGENTS.md",
    ] {
        let p = repo.path().join(rel);
        let lf = fs::read_to_string(&p).unwrap();
        fs::write(&p, lf.replace('\n', "\r\n")).unwrap();
    }
    let check = sync(
        repo.path(),
        &SyncOptions {
            check: true,
            ..all_runtimes()
        },
    )
    .unwrap();
    assert!(check.is_clean(), "{:?}", check.written);
}

#[test]
fn user_content_outside_the_managed_parts_is_preserved() {
    let repo = fixture_repo();
    let root = repo.path();
    fs::write(root.join("CLAUDE.md"), "# My notes\n\nKeep me.\n").unwrap();
    fs::write(root.join("AGENTS.md"), "# Team rules\n\nAlso keep me.\n").unwrap();
    fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers": {"other": {"command": "other-mcp"}}}"#,
    )
    .unwrap();
    fs::create_dir_all(root.join(".claude/skills/mine")).unwrap();
    fs::write(root.join(".claude/skills/mine/SKILL.md"), "user skill").unwrap();
    sync(root, &all_runtimes()).unwrap();

    let claude = fs::read_to_string(root.join("CLAUDE.md")).unwrap();
    assert!(claude.starts_with("# My notes\n\nKeep me.\n"));
    assert!(claude.contains(BLOCK_BEGIN));
    assert!(claude.contains("@AGENTS.md"));
    let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with("# Team rules\n\nAlso keep me.\n"));
    assert!(agents.contains("Rule one."));
    let mcp: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["other"]["command"], "other-mcp");
    assert_eq!(mcp["mcpServers"]["guard"]["command"], "guard");
    assert_eq!(mcp["mcpServers"]["guard"]["args"][0], "mcp");
    assert_eq!(
        fs::read_to_string(root.join(".claude/skills/mine/SKILL.md")).unwrap(),
        "user skill"
    );

    // The managed block is replaced in place, not appended twice.
    fs::write(
        root.join("agents/AGENTS.md"),
        "# Fixture conventions\n\n- Rule three.\n",
    )
    .unwrap();
    sync(root, &all_runtimes()).unwrap();
    let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert_eq!(agents.matches(BLOCK_BEGIN).count(), 1);
    assert!(agents.contains("Rule three.") && !agents.contains("Rule one."));
    assert!(agents.starts_with("# Team rules"));
}

#[test]
fn removed_skills_and_runtimes_are_cleaned_up() {
    let repo = fixture_repo();
    let root = repo.path();
    sync(root, &all_runtimes()).unwrap();
    fs::remove_dir_all(root.join("agents/skills/beta")).unwrap();
    let only_claude = SyncOptions {
        runtimes: BTreeSet::from([RuntimeKind::ClaudeCode]),
        check: false,
    };
    let report = sync(root, &only_claude).unwrap();
    assert!(!root.join(".claude/skills/beta").exists());
    assert!(root.join(".claude/skills/alpha/SKILL.md").exists());
    assert!(!root.join(".guard/agents/rendered/codex").exists());
    assert!(
        !root
            .join(".guard/agents/rendered/claude-code/skills/beta")
            .exists()
    );
    assert!(
        report
            .removed
            .contains(&".claude/skills/beta/SKILL.md".to_string())
    );
    assert!(
        report
            .removed
            .contains(&".guard/agents/rendered/codex/AGENTS.md".to_string())
    );
}

#[test]
fn invalid_sources_are_rejected() {
    let cases: &[(&str, &str, &str)] = &[
        ("alpha", "no frontmatter\n", "frontmatter"),
        (
            "alpha",
            "---\nname: other\ndescription: x\n---\nbody\n",
            "does not match",
        ),
        ("alpha", "---\nname: alpha\n---\nbody\n", "description"),
        (
            "alpha",
            "---\nname: alpha\ndescription: x\n---\n",
            "empty body",
        ),
    ];
    for (skill, body, needle) in cases {
        let repo = fixture_repo();
        fs::write(
            repo.path().join(format!("agents/skills/{skill}/SKILL.md")),
            body,
        )
        .unwrap();
        match sync(repo.path(), &all_runtimes()) {
            Err(SyncError::Source(m)) => assert!(m.contains(needle), "{m}"),
            other => panic!("{needle}: {other:?}"),
        }
    }
    let repo = tempfile::tempdir().unwrap();
    assert!(matches!(
        sync(repo.path(), &all_runtimes()),
        Err(SyncError::Source(_))
    ));
}

#[test]
fn unparseable_mcp_json_is_never_clobbered() {
    let repo = fixture_repo();
    fs::write(repo.path().join(".mcp.json"), "{ not json").unwrap();
    assert!(matches!(
        sync(repo.path(), &all_runtimes()),
        Err(SyncError::Conflict(_))
    ));
    assert_eq!(
        fs::read_to_string(repo.path().join(".mcp.json")).unwrap(),
        "{ not json"
    );
}

#[test]
fn the_repository_skills_load_and_render() {
    let agents = manifest().join("../agents");
    let source = Source::load(&agents).expect("agents/ at the repo root");
    let names: Vec<&str> = source.skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "guard-coder-rules",
            "guard-e2e-authoring",
            "guard-qa-red-first"
        ]
    );
    for s in &source.skills {
        assert!(s.description.len() <= 1024, "{}", s.name);
        assert!(s.description.starts_with("Use when"), "{}", s.name);
    }
    // Renders into a scratch copy without touching the repository.
    let scratch = tempfile::tempdir().unwrap();
    copy_tree(&agents, &scratch.path().join("agents"));
    let report = sync(scratch.path(), &all_runtimes()).unwrap();
    assert!(
        report
            .written
            .contains(&".claude/skills/guard-qa-red-first/SKILL.md".to_string())
    );
}

#[test]
fn sync_mcp_writes_only_the_registrations_and_needs_no_agents_source() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    fs::write(
        root.join(".mcp.json"),
        r#"{"mcpServers": {"other": {"command": "other-mcp"}}}"#,
    )
    .unwrap();
    let runtimes: BTreeSet<RuntimeKind> = [
        RuntimeKind::ClaudeCode,
        RuntimeKind::Codex,
        RuntimeKind::CopilotCli,
    ]
    .into();

    let planned = sync_mcp(root, &runtimes, true).unwrap();
    assert_eq!(
        planned.written,
        vec![
            ".guard/agents/mcp/codex.toml",
            ".guard/agents/mcp/copilot-cli.json",
            ".mcp.json"
        ]
    );
    assert!(!root.join(".guard").exists(), "check writes nothing");

    let report = sync_mcp(root, &runtimes, false).unwrap();
    assert_eq!(report.written, planned.written);
    let written = tree(root);
    assert_eq!(
        written.keys().collect::<Vec<_>>(),
        vec![
            ".guard/agents/mcp/codex.toml",
            ".guard/agents/mcp/copilot-cli.json",
            ".mcp.json"
        ]
    );
    let mcp: serde_json::Value = serde_json::from_str(&written[".mcp.json"]).unwrap();
    assert_eq!(mcp["mcpServers"]["other"]["command"], "other-mcp");
    assert_eq!(mcp["mcpServers"]["guard"]["command"], "guard");

    // Same bytes as the full sync renders for these files.
    let full = fixture_repo();
    sync(full.path(), &all_runtimes()).unwrap();
    let full_tree = tree(full.path());
    for path in [
        ".guard/agents/mcp/codex.toml",
        ".guard/agents/mcp/copilot-cli.json",
    ] {
        assert_eq!(written[path], full_tree[path], "{path}");
    }

    let again = sync_mcp(root, &runtimes, false).unwrap();
    assert!(again.written.is_empty(), "{again:?}");
    assert_eq!(again.unchanged.len(), 3);
}
