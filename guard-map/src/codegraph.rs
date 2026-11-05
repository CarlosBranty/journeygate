//! [`GraphSource`] over a pre-built, read-only CodeGraph index, through the
//! `codegraph` CLI (never its SQLite file, never a write command).
//!
//! Commands used, as verified with `codegraph <cmd> --help` on CodeGraph
//! 1.6.0 (the version the benchmark ran):
//!
//! | need                | command                                            | output |
//! |---------------------|----------------------------------------------------|--------|
//! | freshness           | `status --json <path>`                             | JSON   |
//! | nodes               | `files --json --format flat -p <path>`             | JSON   |
//! | symbols, dependents | `node -f <file> --symbols-only -p <path>`          | text   |
//! | affected tests      | `affected --json --stdin -d <n> -p <path>`         | JSON   |
//!
//! CodeGraph 1.6.0 has no JSON per-file dependency export in its CLI (the
//! SDK's `getFileDependents` is not exposed), so file edges come from the
//! Markdown header of `node --symbols-only`: `used by N files: a, b, ..., +K
//! more`. That list is truncated to 8 names; the hidden remainder is recorded
//! in [`CodeGraphSource::truncated`] and reported in discrepancy records, so a
//! partial edge set is never silently treated as complete.
//!
//! Index writes are out of scope: `sync`, `index`, `init`, `uninit` and
//! friends are never run. The child process gets `CODEGRAPH_TELEMETRY=0`,
//! `DO_NOT_TRACK=1` and `NO_COLOR=1`.

use crate::{Edge, GraphSource, MapError, Symbol};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Runs one `codegraph` invocation and returns its stdout.
pub trait CodeGraphCli: Send + Sync {
    fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, MapError>;
}

/// The real CLI, as a child process (no shell).
#[derive(Debug, Clone)]
pub struct ProcessCli {
    pub program: PathBuf,
}

impl Default for ProcessCli {
    fn default() -> Self {
        // npm installs a `.cmd` shim on Windows; `Command` does not resolve it.
        let program = if cfg!(windows) {
            "codegraph.cmd"
        } else {
            "codegraph"
        };
        Self {
            program: program.into(),
        }
    }
}

impl CodeGraphCli for ProcessCli {
    fn run(&self, args: &[String], stdin: Option<&str>) -> Result<String, MapError> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args)
            .env("CODEGRAPH_TELEMETRY", "0")
            .env("DO_NOT_TRACK", "1")
            .env("NO_COLOR", "1")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            MapError::Unavailable(format!("cannot run {}: {e}", self.program.display()))
        })?;
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(text.as_bytes())
                .map_err(|e| MapError::Unavailable(format!("codegraph stdin: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| MapError::Unavailable(format!("codegraph: {e}")))?;
        if !out.status.success() {
            return Err(MapError::Unavailable(format!(
                "codegraph {} exited with {}: {}",
                args.first().map(String::as_str).unwrap_or(""),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        String::from_utf8(out.stdout)
            .map_err(|_| MapError::Unavailable("codegraph printed non-UTF-8 output".into()))
    }
}

/// One file of `files --json --format flat`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedFile {
    pub path: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub node_count: u64,
}

/// `affected --json` output.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedOutput {
    pub changed_files: Vec<String>,
    pub affected_tests: Vec<String>,
    #[serde(default)]
    pub total_dependents_traversed: u64,
}

/// Parsed `node -f <file> --symbols-only` output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileNode {
    pub symbols: Vec<Symbol>,
    /// Files depending on this one, as listed (at most 8 in CodeGraph 1.6.0).
    pub dependents: Vec<String>,
    /// Dependents CodeGraph counted but did not list (`+K more`).
    pub hidden_dependents: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    #[serde(default)]
    initialized: bool,
    #[serde(default)]
    pending_changes: Option<Pending>,
    #[serde(default)]
    worktree_mismatch: Option<serde_json::Value>,
    #[serde(default)]
    index: Option<IndexState>,
}

#[derive(Debug, Deserialize)]
struct Pending {
    #[serde(default)]
    added: u64,
    #[serde(default)]
    modified: u64,
    #[serde(default)]
    removed: u64,
}

#[derive(Debug, Deserialize)]
struct IndexState {
    #[serde(default)]
    state: Option<String>,
}

#[derive(Debug, Clone)]
struct Loaded {
    files: Vec<String>,
    nodes: BTreeMap<String, FileNode>,
}

pub struct CodeGraphSource {
    cli: Box<dyn CodeGraphCli>,
    project: String,
    commit: String,
    scope: Option<String>,
    concurrency: usize,
    loaded: OnceLock<Result<Loaded, MapError>>,
}

impl std::fmt::Debug for CodeGraphSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGraphSource")
            .field("project", &self.project)
            .field("commit", &self.commit)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl CodeGraphSource {
    /// `project` is the checkout holding the pre-built `.codegraph/` index;
    /// `commit` is the commit that index was built at (the caller knows it:
    /// CodeGraph's status does not report one).
    pub fn new(cli: Box<dyn CodeGraphCli>, project: &str, commit: &str) -> Self {
        Self {
            cli,
            project: project.to_string(),
            commit: commit.to_string(),
            scope: None,
            concurrency: 8,
            loaded: OnceLock::new(),
        }
    }

    /// Only files under `prefix` (repo-relative, `/`-terminated or not) become
    /// nodes. Each node costs one CLI call, so this bounds the cost.
    pub fn with_scope(mut self, prefix: &str) -> Self {
        self.scope = Some(prefix.trim_start_matches("./").to_string());
        self
    }

    /// Parallel `node` calls when loading the graph (at least 1).
    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    fn arg(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Fails when the index is missing, incomplete or stale against the worktree.
    pub fn check_fresh(&self) -> Result<(), MapError> {
        let out = self
            .cli
            .run(&Self::arg(&["status", "--json", &self.project]), None)?;
        let status: Status = serde_json::from_str(out.trim())
            .map_err(|e| MapError::Unavailable(format!("codegraph status: {e}")))?;
        if !status.initialized {
            return Err(MapError::Unavailable("codegraph index missing".into()));
        }
        if let Some(state) = status.index.and_then(|i| i.state)
            && state != "complete"
        {
            return Err(MapError::Unavailable(format!("codegraph index is {state}")));
        }
        if let Some(p) = status.pending_changes
            && p.added + p.modified + p.removed > 0
        {
            return Err(MapError::Unavailable(format!(
                "codegraph index is stale ({} added, {} modified, {} removed)",
                p.added, p.modified, p.removed
            )));
        }
        if status.worktree_mismatch.is_some_and(|v| !v.is_null()) {
            return Err(MapError::Unavailable(
                "codegraph index belongs to another worktree".into(),
            ));
        }
        Ok(())
    }

    /// Indexed files with at least one symbol node, inside the scope, sorted.
    pub fn indexed_files(&self) -> Result<Vec<IndexedFile>, MapError> {
        let mut args = Self::arg(&["files", "--json", "--format", "flat", "-p", &self.project]);
        if let Some(scope) = &self.scope {
            args.extend(["--filter".to_string(), scope.clone()]);
        }
        let out = self.cli.run(&args, None)?;
        let mut files: Vec<IndexedFile> = serde_json::from_str(out.trim())
            .map_err(|e| MapError::Unavailable(format!("codegraph files: {e}")))?;
        files.retain(|f| {
            f.node_count > 0 && self.scope.as_deref().is_none_or(|s| f.path.starts_with(s))
        });
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Ok(files)
    }

    /// Symbols and (truncated) dependents of one file.
    pub fn node(&self, path: &str) -> Result<FileNode, MapError> {
        let out = self.cli.run(
            &Self::arg(&["node", "-f", path, "--symbols-only", "-p", &self.project]),
            None,
        )?;
        Ok(parse_node(&out))
    }

    /// `affected --json --stdin -d <depth>`: test files CodeGraph selects.
    pub fn affected(&self, changed: &[String], depth: u32) -> Result<AffectedOutput, MapError> {
        let depth = depth.to_string();
        let out = self.cli.run(
            &Self::arg(&[
                "affected",
                "--json",
                "--stdin",
                "-d",
                &depth,
                "-p",
                &self.project,
            ]),
            Some(&(changed.join("\n") + "\n")),
        )?;
        serde_json::from_str(out.trim())
            .map_err(|e| MapError::Unavailable(format!("codegraph affected: {e}")))
    }

    fn load(&self) -> Result<&Loaded, MapError> {
        self.loaded
            .get_or_init(|| {
                self.check_fresh()?;
                let files: Vec<String> =
                    self.indexed_files()?.into_iter().map(|f| f.path).collect();
                let chunk = files.len().div_ceil(self.concurrency).max(1);
                let parts: Vec<Result<Vec<(String, FileNode)>, MapError>> =
                    std::thread::scope(|scope| {
                        let handles: Vec<_> = files
                            .chunks(chunk)
                            .map(|part| {
                                scope.spawn(move || {
                                    part.iter()
                                        .map(|f| Ok((f.clone(), self.node(f)?)))
                                        .collect()
                                })
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|h| {
                                h.join().unwrap_or_else(|_| {
                                    Err(MapError::Unavailable("codegraph worker panicked".into()))
                                })
                            })
                            .collect()
                    });
                let mut nodes = BTreeMap::new();
                for part in parts {
                    nodes.extend(part?);
                }
                Ok(Loaded { files, nodes })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Files whose dependents list was truncated, with the hidden count.
    pub fn truncated(&self) -> Result<BTreeMap<String, u32>, MapError> {
        Ok(self
            .load()?
            .nodes
            .iter()
            .filter(|(_, n)| n.hidden_dependents > 0)
            .map(|(p, n)| (p.clone(), n.hidden_dependents))
            .collect())
    }
}

impl GraphSource for CodeGraphSource {
    fn backend(&self) -> &'static str {
        crate::backend::CODEGRAPH
    }

    fn commit(&self) -> &str {
        &self.commit
    }

    fn files(&self) -> Result<Vec<String>, MapError> {
        Ok(self.load()?.files.clone())
    }

    fn edges(&self) -> Result<Vec<Edge>, MapError> {
        let loaded = self.load()?;
        let known: BTreeSet<&str> = loaded.files.iter().map(String::as_str).collect();
        let mut edges = Vec::new();
        for (to, node) in &loaded.nodes {
            for from in &node.dependents {
                if known.contains(from.as_str()) && from != to {
                    edges.push(Edge {
                        from: from.clone(),
                        to: to.clone(),
                        weight: 1.0,
                    });
                }
            }
        }
        Ok(edges)
    }

    fn symbols(&self, path: &str) -> Result<Vec<Symbol>, MapError> {
        Ok(self
            .load()?
            .nodes
            .get(path)
            .map(|n| n.symbols.clone())
            .unwrap_or_default())
    }
}

/// Parses `node -f <file> --symbols-only` (CodeGraph 1.6.0 Markdown):
///
/// ```text
/// **src/router.ts** — 10 symbols, used by 25 files: a.ts, b.ts, +17 more
///
/// **Symbols**
/// - `Router` (interface) — :29
/// - `match` (method) (path: string): Result — :41
/// ```
///
/// `no other indexed file depends on it` means no dependents; a "No indexed
/// file matches" answer is an empty node.
pub fn parse_node(text: &str) -> FileNode {
    let mut node = FileNode::default();
    let mut lines = text.lines();
    if let Some(header) = lines.next()
        && let Some((_, list)) = header
            .split_once(" files: ")
            .or_else(|| header.split_once(" file: "))
    {
        for item in list.split(", ") {
            let item = item.trim();
            if let Some(more) = item.strip_prefix('+').and_then(|r| r.strip_suffix(" more")) {
                node.hidden_dependents = more.trim().parse().unwrap_or(0);
            } else if !item.is_empty() {
                node.dependents.push(item.to_string());
            }
        }
    }
    for line in lines {
        let Some(rest) = line.strip_prefix("- `") else {
            continue;
        };
        let Some((name, after)) = rest.split_once('`') else {
            continue;
        };
        let kind = after
            .trim_start()
            .strip_prefix('(')
            .and_then(|k| k.split_once(')'))
            .map(|(k, _)| k.to_string())
            .unwrap_or_default();
        let Some(line_no) = after
            .rsplit_once(" — :")
            .and_then(|(_, n)| n.trim().parse::<u32>().ok())
        else {
            continue;
        };
        node.symbols.push(Symbol {
            name: name.to_string(),
            kind,
            line: line_no,
        });
    }
    node.symbols.sort_by_key(|s| s.line);
    node
}

#[cfg(test)]
mod tests;
