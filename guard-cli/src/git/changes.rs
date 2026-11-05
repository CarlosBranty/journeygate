//! The one path-collection walker: changed, tracked and listed paths of the
//! superproject and, recursively, of every repository nested in it
//! (submodules and untracked nested repositories). cli.md "Changed files".
//!
//! Paths are relative to the superproject root, use `/`, are deduplicated and
//! keep the order in which they were collected. A submodule whose inner change
//! cannot be read is reported as one subtree entry `<path>/`, plus a warning.

use super::{GitError, git_in, nul_list};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// The all-zero object id git prints for a missing side.
pub(crate) const ZERO: &str = "0000000000000000000000000000000000000000";
const GITLINK: &str = "160000";
const NO_MODE: &str = "000000";
/// How many submodule levels the walker goes into.
pub const MAX_DEPTH: usize = 8;

/// Paths plus the warnings collected while walking (each printed once).
#[derive(Debug, Default, Clone)]
pub struct PathSet {
    pub paths: Vec<String>,
    pub warnings: Vec<String>,
    seen: HashSet<String>,
}

impl PathSet {
    fn push(&mut self, path: String) {
        if self.seen.insert(path.clone()) {
            self.paths.push(path);
        }
    }

    fn warn(&mut self, warning: String) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    /// Records `<prefix><path>/` as standing for everything under it.
    fn subtree(&mut self, prefix: &str, path: &str, why: String) {
        self.push(format!("{prefix}{path}/"));
        self.warn(why);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Commits between `from` and HEAD only.
    Committed,
    /// Plus staged, unstaged and untracked files.
    CommittedAndWorktree,
}

/// One `git diff --raw -z --no-renames` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawEntry {
    pub old_mode: String,
    pub new_mode: String,
    pub old_sha: String,
    pub new_sha: String,
    pub path: String,
}

impl RawEntry {
    fn touches_gitlink(&self) -> bool {
        self.old_mode == GITLINK || self.new_mode == GITLINK
    }

    /// The non-submodule side of a type change (or a plain file) exists.
    fn has_file_side(&self) -> bool {
        let file = |mode: &str| mode != GITLINK && mode != NO_MODE;
        file(&self.old_mode) || file(&self.new_mode)
    }
}

/// Parses `:<om> <nm> <os> <ns> <st>\0<path>\0...`.
pub(crate) fn parse_raw_z(bytes: &[u8]) -> Vec<RawEntry> {
    let fields = nul_list(bytes);
    let mut entries = Vec::new();
    let mut it = fields.into_iter();
    while let Some(header) = it.next() {
        let Some(header) = header.strip_prefix(':') else {
            continue;
        };
        let Some(path) = it.next() else { break };
        let parts: Vec<&str> = header.split(' ').collect();
        if parts.len() < 5 {
            continue;
        }
        entries.push(RawEntry {
            old_mode: parts[0].to_string(),
            new_mode: parts[1].to_string(),
            old_sha: parts[2].to_string(),
            new_sha: parts[3].to_string(),
            path,
        });
    }
    entries
}

/// A repository level: a working tree, or the object store of a submodule
/// whose working tree is gone (`--git-dir`).
#[derive(Debug, Clone)]
struct Repo {
    dir: PathBuf,
    git_dir: Option<PathBuf>,
}

impl Repo {
    fn worktree(dir: &Path) -> Self {
        Repo {
            dir: dir.to_path_buf(),
            git_dir: None,
        }
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        match &self.git_dir {
            None => git_in(&self.dir, args),
            Some(git_dir) => {
                // The store's `core.worktree` names a directory that is gone:
                // an explicit work tree keeps git from changing into it.
                let flag = format!("--git-dir={}", git_dir.display());
                let tree = format!("--work-tree={}", self.dir.display());
                let mut all = vec![flag.as_str(), tree.as_str()];
                all.extend_from_slice(args);
                git_in(&self.dir, &all)
            }
        }
    }

    fn has_commit(&self, sha: &str) -> bool {
        self.run(&["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .is_ok()
    }

    fn common_dir(&self) -> Option<PathBuf> {
        let out = self.run(&["rev-parse", "--git-common-dir"]).ok()?;
        let path = PathBuf::from(String::from_utf8_lossy(&out).trim());
        Some(if path.is_absolute() {
            path
        } else {
            self.dir.join(path)
        })
    }

    /// The submodule name `.gitmodules` at `rev` gives `path`.
    fn submodule_name_at(&self, rev: &str, path: &str) -> Option<String> {
        let blob = format!("{rev}:.gitmodules");
        let out = self
            .run(&[
                "config",
                "-z",
                "--blob",
                &blob,
                "--get-regexp",
                r"^submodule\..*\.path$",
            ])
            .ok()?;
        nul_list(&out).into_iter().find_map(|entry| {
            let (key, value) = entry.split_once('\n')?;
            (value == path).then(|| {
                key.trim_start_matches("submodule.")
                    .trim_end_matches(".path")
                    .to_string()
            })
        })
    }

    /// The repository mounted at `path`: its working tree when this level
    /// is a working tree and `<path>/.git` exists; inside an object store,
    /// the nested store `modules/<name>` (name from `.gitmodules` at `rev`).
    fn nested(&self, path: &str, rev: &str) -> Option<Repo> {
        match self.git_dir {
            None => {
                let dir = self.dir.join(path);
                dir.join(".git").exists().then(|| Repo::worktree(&dir))
            }
            Some(_) => self.store(path, rev),
        }
    }

    /// `$(git rev-parse --git-common-dir)/modules/<name>`.
    fn store(&self, path: &str, rev: &str) -> Option<Repo> {
        let name = self
            .submodule_name_at(rev, path)
            .unwrap_or_else(|| path.to_string());
        let store = self.common_dir()?.join("modules").join(name);
        store.is_dir().then(|| Repo {
            dir: self.dir.clone(),
            git_dir: Some(store),
        })
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn not_initialized(prefix: &str, path: &str) -> String {
    format!("submodule {prefix}{path} is not initialized; treating it as entirely changed")
}

fn commit_missing(prefix: &str, path: &str, sha: &str) -> String {
    format!(
        "submodule {prefix}{path}: commit {} not present; treating it as entirely changed",
        short(sha)
    )
}

fn store_gone(prefix: &str, path: &str) -> String {
    format!(
        "submodule {prefix}{path}: removed and its object store is gone; treating it as entirely changed"
    )
}

fn too_deep(prefix: &str, path: &str, max: usize) -> String {
    format!(
        "submodule {prefix}{path}: nested deeper than {max} levels; treating it as entirely changed"
    )
}

const RAW: [&str; 6] = [
    "diff",
    "--raw",
    "-z",
    "--no-renames",
    "--no-abbrev",
    "--ignore-submodules=none",
];

pub(crate) struct Walk {
    pub max_depth: usize,
}

impl Default for Walk {
    fn default() -> Self {
        Walk {
            max_depth: MAX_DEPTH,
        }
    }
}

impl Walk {
    /// One level compared from `from` to HEAD, plus its working tree.
    fn level(
        &self,
        repo: &Repo,
        prefix: &str,
        from: &str,
        scope: Scope,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        self.committed(repo, prefix, from, "HEAD", depth, out)?;
        if scope == Scope::CommittedAndWorktree {
            self.worktree(repo, prefix, depth, out)?;
        }
        Ok(())
    }

    /// `git diff --raw <from> <to>`, gitlinks expanded.
    fn committed(
        &self,
        repo: &Repo,
        prefix: &str,
        from: &str,
        to: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        let mut args = RAW.to_vec();
        args.extend([from, to]);
        for entry in parse_raw_z(&repo.run(&args)?) {
            if entry.touches_gitlink() {
                self.committed_gitlink(repo, prefix, &entry, from, depth, out)?;
            } else {
                out.push(format!("{prefix}{}", entry.path));
            }
        }
        Ok(())
    }

    /// A pointer change between two commits (contract table). `names_rev`
    /// is where `.gitmodules` names a deleted submodule.
    fn committed_gitlink(
        &self,
        repo: &Repo,
        prefix: &str,
        e: &RawEntry,
        names_rev: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        if e.has_file_side() {
            out.push(format!("{prefix}{}", e.path));
        }
        match (e.old_mode == GITLINK, e.new_mode == GITLINK) {
            (true, true) => self.modified(repo, prefix, e, depth, out),
            (false, true) => self.added(repo, prefix, &e.path, &e.new_sha, names_rev, depth, out),
            (true, false) => self.deleted(repo, prefix, &e.path, &e.old_sha, names_rev, depth, out),
            (false, false) => Ok(()),
        }
    }

    fn modified(
        &self,
        repo: &Repo,
        prefix: &str,
        e: &RawEntry,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        let path = &e.path;
        if depth >= self.max_depth {
            out.subtree(prefix, path, too_deep(prefix, path, self.max_depth));
            return Ok(());
        }
        let Some(sub) = repo.nested(path, "HEAD") else {
            out.subtree(prefix, path, not_initialized(prefix, path));
            return Ok(());
        };
        for sha in [&e.old_sha, &e.new_sha] {
            if !sub.has_commit(sha) {
                out.subtree(prefix, path, commit_missing(prefix, path, sha));
                return Ok(());
            }
        }
        let inner = format!("{prefix}{path}/");
        self.committed(&sub, &inner, &e.old_sha, &e.new_sha, depth + 1, out)
    }

    #[allow(clippy::too_many_arguments)]
    fn added(
        &self,
        repo: &Repo,
        prefix: &str,
        path: &str,
        sha: &str,
        names_rev: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        if depth >= self.max_depth {
            out.subtree(prefix, path, too_deep(prefix, path, self.max_depth));
            return Ok(());
        }
        let Some(sub) = repo.nested(path, names_rev) else {
            out.subtree(prefix, path, not_initialized(prefix, path));
            return Ok(());
        };
        self.whole_tree(&sub, prefix, path, sha, depth, out)
    }

    #[allow(clippy::too_many_arguments)]
    fn deleted(
        &self,
        repo: &Repo,
        prefix: &str,
        path: &str,
        sha: &str,
        names_rev: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        if depth >= self.max_depth {
            out.subtree(prefix, path, too_deep(prefix, path, self.max_depth));
            return Ok(());
        }
        let Some(store) = repo.store(path, names_rev) else {
            out.subtree(prefix, path, store_gone(prefix, path));
            return Ok(());
        };
        self.whole_tree(&store, prefix, path, sha, depth, out)
    }

    /// Every path of `sha` in `sub`, mounted at `<prefix><path>/`.
    fn whole_tree(
        &self,
        sub: &Repo,
        prefix: &str,
        path: &str,
        sha: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        if !sub.has_commit(sha) {
            out.subtree(prefix, path, commit_missing(prefix, path, sha));
            return Ok(());
        }
        let inner = format!("{prefix}{path}/");
        for entry in nul_list(&sub.run(&["ls-tree", "-r", "-z", sha])?) {
            let Some((meta, file)) = entry.split_once('\t') else {
                continue;
            };
            let mut meta = meta.split(' ');
            let (mode, child) = (meta.next().unwrap_or(""), meta.nth(1).unwrap_or(""));
            if mode == GITLINK {
                self.added(sub, &inner, file, child, sha, depth + 1, out)?;
            } else {
                out.push(format!("{inner}{file}"));
            }
        }
        Ok(())
    }

    /// Staged + unstaged changes and untracked files of a working tree.
    fn worktree(
        &self,
        repo: &Repo,
        prefix: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        let mut args = RAW.to_vec();
        args.push("HEAD");
        for entry in parse_raw_z(&repo.run(&args)?) {
            if entry.touches_gitlink() {
                self.worktree_gitlink(&repo.dir, prefix, &entry, depth + 1, out)?;
            } else {
                out.push(format!("{prefix}{}", entry.path));
            }
        }
        self.untracked(repo, prefix, depth, out)
    }

    /// A submodule that differs from the pointer recorded in HEAD: its own
    /// level, compared from that pointer (`depth` is the submodule's level).
    pub(crate) fn worktree_gitlink(
        &self,
        dir: &Path,
        prefix: &str,
        e: &RawEntry,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        let repo = Repo::worktree(dir);
        let path = &e.path;
        if e.has_file_side() {
            out.push(format!("{prefix}{path}"));
        }
        if e.new_mode != GITLINK {
            // Gone from the index (or now a file): HEAD's tree from its store.
            return self.deleted(&repo, prefix, path, &e.old_sha, "HEAD", depth - 1, out);
        }
        if depth > self.max_depth {
            out.subtree(prefix, path, too_deep(prefix, path, self.max_depth));
            return Ok(());
        }
        let Some(sub) = repo.nested(path, "HEAD") else {
            out.subtree(prefix, path, not_initialized(prefix, path));
            return Ok(());
        };
        let inner = format!("{prefix}{path}/");
        if e.old_mode != GITLINK {
            // Staged as new: no pointer in HEAD; its nested gitlinks expanded.
            return self.files(&sub, &inner, true, depth, out);
        }
        if !sub.has_commit(&e.old_sha) {
            out.subtree(prefix, path, commit_missing(prefix, path, &e.old_sha));
            return Ok(());
        }
        self.level(
            &sub,
            &inner,
            &e.old_sha,
            Scope::CommittedAndWorktree,
            depth,
            out,
        )
    }

    /// `ls-files --others`; an untracked nested repository (`<dir>/`) is
    /// expanded into its files, its own gitlinks included.
    fn untracked(
        &self,
        repo: &Repo,
        prefix: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        let others = repo.run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
        for file in nul_list(&others) {
            match file.strip_suffix('/') {
                Some(dir) => self.nested_repo(repo, prefix, dir, depth + 1, out)?,
                None => out.push(format!("{prefix}{file}")),
            }
        }
        Ok(())
    }

    /// An untracked nested repository at `dir` (`depth` is its level).
    fn nested_repo(
        &self,
        repo: &Repo,
        prefix: &str,
        dir: &str,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        let nested = repo.dir.join(dir);
        if !nested.join(".git").exists() {
            out.push(format!("{prefix}{dir}/"));
            return Ok(());
        }
        if depth > self.max_depth {
            out.subtree(prefix, dir, too_deep(prefix, dir, self.max_depth));
            return Ok(());
        }
        let inner = format!("{prefix}{dir}/");
        self.files(&Repo::worktree(&nested), &inner, true, depth, out)
    }

    /// `ls-files -s` of one level, initialized submodules expanded (an
    /// uninitialized or too deep one is a subtree entry, with a warning);
    /// with `others`, untracked files (and nested repositories) follow.
    fn files(
        &self,
        repo: &Repo,
        prefix: &str,
        others: bool,
        depth: usize,
        out: &mut PathSet,
    ) -> Result<(), GitError> {
        for entry in nul_list(&repo.run(&["ls-files", "-s", "-z"])?) {
            let Some((meta, path)) = entry.split_once('\t') else {
                continue;
            };
            if !meta.starts_with(GITLINK) {
                out.push(format!("{prefix}{path}"));
                continue;
            }
            if depth >= self.max_depth {
                out.subtree(prefix, path, too_deep(prefix, path, self.max_depth));
                continue;
            }
            match repo.nested(path, "HEAD") {
                Some(sub) => {
                    self.files(&sub, &format!("{prefix}{path}/"), others, depth + 1, out)?;
                }
                None => out.subtree(prefix, path, not_initialized(prefix, path)),
            }
        }
        if others {
            self.untracked(repo, prefix, depth, out)?;
        }
        Ok(())
    }
}

/// Changed paths from `from_sha` (cli.md "Changed files").
pub fn changed_paths(root: &Path, from_sha: &str, scope: Scope) -> Result<PathSet, GitError> {
    let mut out = PathSet::default();
    Walk::default().level(&Repo::worktree(root), "", from_sha, scope, 0, &mut out)?;
    Ok(out)
}

/// Tracked paths, initialized submodules expanded; an uninitialized one is
/// the subtree entry `<path>/`.
pub fn tracked_paths(root: &Path) -> Result<PathSet, GitError> {
    let mut out = PathSet::default();
    Walk::default().files(&Repo::worktree(root), "", false, 0, &mut out)?;
    Ok(out)
}

/// Tracked plus untracked, not ignored, paths at every level.
pub fn listed_paths(root: &Path) -> Result<PathSet, GitError> {
    let mut out = PathSet::default();
    Walk::default().files(&Repo::worktree(root), "", true, 0, &mut out)?;
    Ok(out)
}

/// `(mode, sha)` of `path` in `rev`'s tree.
fn tree_entry(root: &Path, rev: &str, path: &str) -> Option<(String, String)> {
    let out = git_in(root, &["ls-tree", "-z", rev, "--", path]).ok()?;
    let entry = nul_list(&out).into_iter().next()?;
    let (meta, _) = entry.split_once('\t')?;
    let mut meta = meta.split(' ');
    let mode = meta.next()?.to_string();
    let sha = meta.nth(1)?.to_string();
    Some((mode, sha))
}

/// A changed-path list from elsewhere (a PR API): each entry that is a
/// gitlink at HEAD or at `from_sha` is replaced by its local expansion, or by
/// the subtree entry `<path>/` without a base. Other entries are kept.
pub fn expand_gitlinks(
    root: &Path,
    from_sha: Option<&str>,
    entries: &[String],
) -> Result<PathSet, GitError> {
    let walk = Walk::default();
    let repo = Repo::worktree(root);
    let mut out = PathSet::default();
    for path in entries {
        let head = tree_entry(root, "HEAD", path);
        let base = from_sha.and_then(|b| tree_entry(root, b, path));
        let is_link = |e: &Option<(String, String)>| e.as_ref().is_some_and(|(m, _)| m == GITLINK);
        if !is_link(&head) && !is_link(&base) {
            out.push(path.clone());
            continue;
        }
        let Some(from) = from_sha else {
            out.push(format!("{path}/"));
            continue;
        };
        let side = |e: Option<(String, String)>| e.unwrap_or_else(|| (NO_MODE.into(), ZERO.into()));
        let (old_mode, old_sha) = side(base);
        let (new_mode, new_sha) = side(head);
        let entry = RawEntry {
            old_mode,
            new_mode,
            old_sha,
            new_sha,
            path: path.clone(),
        };
        walk.committed_gitlink(&repo, "", &entry, from, 0, &mut out)?;
    }
    Ok(out)
}
