use super::*;
use std::sync::Mutex;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/codegraph-1.6.0"
);

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

/// Answers from recorded CodeGraph 1.6.0 output and records every call.
#[derive(Default)]
struct FakeCli {
    answers: BTreeMap<String, String>,
    calls: Mutex<Vec<(Vec<String>, Option<String>)>>,
}

impl FakeCli {
    /// Keyed by `cmd` or `cmd <file>` (for `node -f <file>`).
    fn with(mut self, key: &str, out: String) -> Self {
        self.answers.insert(key.into(), out);
        self
    }
}

impl CodeGraphCli for std::sync::Arc<FakeCli> {
    fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, MapError> {
        self.calls
            .lock()
            .unwrap()
            .push((args.to_vec(), stdin.map(str::to_string)));
        let key = match args[0].as_str() {
            "node" => format!("node {}", args[2]),
            other => other.to_string(),
        };
        self.answers
            .get(&key)
            .cloned()
            .ok_or_else(|| MapError::Unavailable(format!("no answer for {key}")))
    }
}

fn hono_preset() -> std::sync::Arc<FakeCli> {
    let quick_test = "**src/preset/quick.test.ts** — 1 symbols, no other indexed file depends on it\n\n**Symbols**\n- `app` (variable) — :5\n";
    let tiny = "**src/preset/tiny.ts** — 1 symbols, used by 1 file: src/preset/tiny.test.ts\n\n**Symbols**\n- `Hono` (class) — :10\n";
    let tiny_test = "**src/preset/tiny.test.ts** — 0 symbols, no other indexed file depends on it\n\n_No indexed symbols in this file._\n";
    std::sync::Arc::new(
        FakeCli::default()
            .with("status", fixture("status.json"))
            .with("files", fixture("files-preset.json"))
            .with("node src/preset/quick.ts", fixture("node-quick.txt"))
            .with("node src/preset/quick.test.ts", quick_test.into())
            .with("node src/preset/tiny.ts", tiny.into())
            .with("node src/preset/tiny.test.ts", tiny_test.into())
            .with("affected", fixture("affected-quick.json")),
    )
}

#[test]
fn parses_a_truncated_dependents_header_and_symbols() {
    let n = parse_node(&fixture("node-router.txt"));
    assert_eq!(n.dependents.len(), 8);
    assert_eq!(n.dependents[1], "src/hono-base.ts");
    assert_eq!(n.hidden_dependents, 17);
    assert_eq!(n.symbols.len(), 10);
    assert_eq!(
        n.symbols[4],
        Symbol {
            name: "Router".into(),
            kind: "interface".into(),
            line: 29
        }
    );
}

#[test]
fn parses_single_no_dependents_and_missing_files() {
    let q = parse_node(&fixture("node-quick.txt"));
    assert_eq!(q.dependents, ["src/preset/quick.test.ts"]);
    assert_eq!(q.hidden_dependents, 0);
    assert_eq!(
        q.symbols
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["Hono", "constructor"]
    );
    assert_eq!(
        parse_node(&fixture("node-funding.txt")),
        FileNode::default()
    );
    assert_eq!(
        parse_node(&fixture("node-missing.txt")),
        FileNode::default()
    );
}

#[test]
fn graph_source_over_recorded_cli_output() {
    let cli = hono_preset();
    let s = CodeGraphSource::new(Box::new(cli.clone()), "/repo", "c91b60a2")
        .with_scope("src/preset/")
        .with_concurrency(2);
    assert_eq!(s.backend(), "codegraph");
    assert_eq!(s.commit(), "c91b60a2");
    assert_eq!(
        s.files().unwrap(),
        [
            "src/preset/quick.test.ts",
            "src/preset/quick.ts",
            "src/preset/tiny.test.ts",
            "src/preset/tiny.ts"
        ]
    );
    let mut edges: Vec<(String, String)> = s
        .edges()
        .unwrap()
        .into_iter()
        .map(|e| (e.from, e.to))
        .collect();
    edges.sort();
    assert_eq!(
        edges,
        [
            (
                "src/preset/quick.test.ts".to_string(),
                "src/preset/quick.ts".to_string()
            ),
            (
                "src/preset/tiny.test.ts".to_string(),
                "src/preset/tiny.ts".to_string()
            ),
        ]
    );
    assert_eq!(s.symbols("src/preset/tiny.ts").unwrap()[0].name, "Hono");
    assert!(s.symbols("elsewhere.ts").unwrap().is_empty());
    assert!(s.truncated().unwrap().is_empty());

    // Loaded once; only read-only commands; scope forwarded; project passed.
    let calls = cli.calls.lock().unwrap();
    assert_eq!(calls.len(), 2 + 4, "status + files + one node per file");
    let read_only = ["status", "files", "node", "affected"];
    assert!(
        calls
            .iter()
            .all(|(a, _)| read_only.contains(&a[0].as_str()))
    );
    let files_call = calls.iter().find(|(a, _)| a[0] == "files").unwrap();
    assert!(
        files_call
            .0
            .windows(2)
            .any(|w| w == ["--filter", "src/preset/"])
    );
    assert!(
        calls
            .iter()
            .filter(|(a, _)| a[0] != "status")
            .all(|(a, _)| a.windows(2).any(|w| w == ["-p", "/repo"]))
    );
}

#[test]
fn affected_uses_stdin_and_json() {
    let cli = hono_preset();
    let s = CodeGraphSource::new(Box::new(cli.clone()), "/repo", "c");
    let out = s.affected(&["src/preset/quick.ts".into()], 3).unwrap();
    assert_eq!(out.affected_tests, ["src/preset/quick.test.ts"]);
    assert_eq!(out.total_dependents_traversed, 1);
    let calls = cli.calls.lock().unwrap();
    let (args, stdin) = &calls[0];
    assert!(args.windows(2).any(|w| w == ["-d", "3"]));
    assert!(args.contains(&"--json".to_string()) && args.contains(&"--stdin".to_string()));
    assert_eq!(stdin.as_deref(), Some("src/preset/quick.ts\n"));
}

#[test]
fn stale_or_missing_index_is_unavailable() {
    let stale = fixture("status.json").replace(
        "\"pendingChanges\":{\"added\":0",
        "\"pendingChanges\":{\"added\":2",
    );
    for status in [
        stale,
        "{\"initialized\":false}".to_string(),
        "{\"initialized\":true,\"index\":{\"state\":\"indexing\"}}".to_string(),
        "{\"initialized\":true,\"worktreeMismatch\":{\"expected\":\"x\"}}".to_string(),
        "not json".to_string(),
    ] {
        let cli = std::sync::Arc::new(FakeCli::default().with("status", status.clone()));
        let s = CodeGraphSource::new(Box::new(cli), "/repo", "c");
        assert!(
            matches!(s.files(), Err(MapError::Unavailable(_))),
            "{status}"
        );
        // The failure is cached: a second call does not re-run the CLI.
        assert!(s.edges().is_err());
    }
}

#[test]
fn truncation_is_reported() {
    let cli = std::sync::Arc::new(
        FakeCli::default()
            .with("status", fixture("status.json"))
            .with(
                "files",
                "[{\"path\":\"src/router.ts\",\"language\":\"typescript\",\"nodeCount\":10}]"
                    .into(),
            )
            .with("node src/router.ts", fixture("node-router.txt")),
    );
    let s = CodeGraphSource::new(Box::new(cli), "/repo", "c");
    assert_eq!(
        s.truncated().unwrap(),
        BTreeMap::from([("src/router.ts".to_string(), 17)])
    );
    // Dependents outside the node set never become dangling edges.
    assert!(s.edges().unwrap().is_empty());
}

#[test]
fn process_cli_reports_a_missing_binary_as_unavailable() {
    let cli = ProcessCli {
        program: "definitely-not-a-codegraph-binary".into(),
    };
    assert!(matches!(
        cli.run(&["status".into()], None),
        Err(MapError::Unavailable(_))
    ));
}

/// Live: the real CLI against a pre-built index. Set
/// `GUARD_CODEGRAPH_TEST_REPO` to a checkout with `.codegraph/` (the
/// benchmark's hono clone works) and run with `--include-ignored`.
#[test]
#[ignore = "needs the codegraph CLI and GUARD_CODEGRAPH_TEST_REPO"]
fn live_codegraph_cli() {
    let Ok(repo) = std::env::var("GUARD_CODEGRAPH_TEST_REPO") else {
        eprintln!("GUARD_CODEGRAPH_TEST_REPO unset; skipping");
        return;
    };
    let s = CodeGraphSource::new(Box::new(ProcessCli::default()), &repo, "live")
        .with_scope("src/preset/");
    let files = s.files().unwrap();
    assert!(!files.is_empty(), "no files under src/preset/");
    assert!(!s.edges().unwrap().is_empty());
    let first = &files[0];
    assert!(s.node(first).is_ok());
    let out = s.affected(&[files[0].clone()], 2).unwrap();
    assert_eq!(out.changed_files, [files[0].clone()]);
}

#[test]
fn affected_discrepancies_against_guard_impact() {
    use crate::discrepancy::{DiscrepancyKind, compare_affected};
    use crate::impact_source::GuardImpactSource;
    use guard_core::config::parse_target;

    let config = parse_target(
        "version = 1\n[app]\nname = \"t\"\nlanguages = []\nbase_url = \"http://localhost\"\n\
         [areas.web]\ndescription = \"Web\"\npaths = [\"src/**\"]\n",
    )
    .unwrap();
    // guard-impact sees quick.test.ts and a second test importing quick.ts.
    let tree: BTreeMap<String, String> = [
        ("src/preset/quick.ts", "export class Hono {}\n"),
        (
            "src/preset/quick.test.ts",
            "import { Hono } from './quick'\n",
        ),
        (
            "src/preset/extra.test.ts",
            "import { Hono } from './quick'\n",
        ),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    let index = guard_impact::ImpactIndex::from_sources(tree.clone(), &config).unwrap();
    let gi = GuardImpactSource::with_reader(index, "c", Box::new(move |p| tree.get(p).cloned()));
    let cli = hono_preset();
    let cg = CodeGraphSource::new(Box::new(cli), "/repo", "c");

    let recs = compare_affected(&gi, &cg, &["src/preset/quick.ts".into()], 3).unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].kind, DiscrepancyKind::Affected);
    assert_eq!(recs[0].left, "codegraph");
    assert_eq!(recs[0].right, "guard-impact");
    assert!(recs[0].only_left.is_empty());
    assert_eq!(recs[0].only_right, ["src/preset/extra.test.ts"]);
    assert_eq!(recs[0].common, 1);
}
