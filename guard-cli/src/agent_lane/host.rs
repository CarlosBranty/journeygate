//! Host-side seams of the agent pipeline: per-attempt sandbox layouts
//! ([`HostLayout`]), the local PR branch publisher ([`LocalBranchPublisher`])
//! and the real code-host publisher on top of it ([`CodeHostPublisher`],
//! chosen by [`pr_publisher`]).
//!
//! Everything here runs as the orchestrator on the host, never in a sandbox.
//! Checkouts are standalone clones (their `.git` is a directory, so the
//! sandbox can run git on them) made with `core.autocrlf=false`, so the files
//! the agent sees and the diff it produces are byte-identical to the commit.

use guard_agents::config::RuntimeKind;
use guard_agents::pipeline::seams::{
    LayoutProvider, LayoutRequest, PrPublisher, PrRequest, PublishedPr, SeamError,
};
use guard_core::config::TargetConfig;
use guard_core::ports::{
    AgentPullRequest, BranchPush, CodeHostError, PullRequestPublisher, Role, SandboxLayout,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

/// `git -c core.autocrlf=false <args>` in `dir`; stdout on success.
pub fn git(dir: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<String, SeamError> {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.safecrlf=false",
        "-c",
        "user.name=guard-orchestrator",
        "-c",
        "user.email=guard-orchestrator@localhost",
    ])
    .args(args)
    .current_dir(dir)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = cmd
        .spawn()
        .map_err(|e| SeamError(format!("cannot run git: {e}")))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(bytes)
            .map_err(|e| SeamError(format!("git stdin: {e}")))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| SeamError(format!("git: {e}")))?;
    if !out.status.success() {
        return Err(SeamError(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn io(context: &str) -> impl FnOnce(std::io::Error) -> SeamError + '_ {
    move |e| SeamError(format!("{context}: {e}"))
}

/// Blocking work off the async runtime.
pub async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SeamError> + Send + 'static,
) -> Result<T, SeamError> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| Err(SeamError(format!("host task failed: {e}"))))
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), SeamError> {
    std::fs::create_dir_all(to).map_err(io("copy skills"))?;
    for entry in std::fs::read_dir(from).map_err(io("read skills"))? {
        let entry = entry.map_err(io("read skills"))?;
        let target = to.join(entry.file_name());
        if entry.file_type().map_err(io("read skills"))?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(io("copy skills"))?;
        }
    }
    Ok(())
}

/// A standalone repository at `dest` holding only `base` (a shallow root)
/// and the trees and blobs of its snapshot, minus every blob under `hidden`
/// (the tests dir). The Coder's `/workspace/.git` therefore cannot reveal
/// the hidden journeys and smoke specs, at `base` or in any older commit.
/// Hidden paths stay in the index as skip-worktree entries (sparse
/// checkout), so `git status` is clean and the sandbox's `read-tree <base>`
/// plus `diff --cached <base>` never need their blobs.
fn export_without(
    repo: &Path,
    base: &str,
    hidden: &str,
    dest: &Path,
    branch: &str,
) -> Result<(), SeamError> {
    let hidden = hidden.trim_matches('/');
    let prefix = format!("{hidden}/");
    let mut objects = vec![
        base.to_string(),
        git(repo, &["rev-parse", &format!("{base}^{{tree}}")], None)?,
    ];
    for line in git(
        repo,
        &["ls-tree", "-r", "-t", "-z", "--full-tree", base],
        None,
    )?
    .split('\0')
    .filter(|l| !l.is_empty())
    {
        let (meta, path) = line
            .split_once('\t')
            .ok_or_else(|| SeamError(format!("ls-tree line {line:?}")))?;
        let mut f = meta.split(' ');
        let (kind, oid) = (f.nth(1).unwrap_or_default(), f.next().unwrap_or_default());
        match kind {
            // Submodule commits are not objects of this repository.
            "commit" => continue,
            "blob" if !hidden.is_empty() && (path == hidden || path.starts_with(&prefix)) => {
                continue;
            }
            _ => objects.push(oid.to_string()),
        }
    }
    let pack = pack_objects(repo, &objects)?;
    std::fs::create_dir_all(dest).map_err(io("create checkout"))?;
    git(dest, &["init", "--quiet"], None)?;
    git(dest, &["config", "core.autocrlf", "false"], None)?;
    git(dest, &["unpack-objects", "-q"], Some(&pack))?;
    std::fs::write(
        dest.join(".git").join("shallow"),
        format!(
            "{base}
"
        ),
    )
    .map_err(io("write shallow"))?;
    let reference = format!("refs/heads/{branch}");
    git(dest, &["update-ref", &reference, base], None)?;
    git(dest, &["symbolic-ref", "HEAD", &reference], None)?;
    let mut sparse = "/*
"
    .to_string();
    if !hidden.is_empty() {
        sparse.push_str(&format!(
            "!/{hidden}/
"
        ));
    }
    std::fs::create_dir_all(dest.join(".git").join("info")).map_err(io("sparse"))?;
    std::fs::write(
        dest.join(".git").join("info").join("sparse-checkout"),
        sparse,
    )
    .map_err(io("sparse"))?;
    git(dest, &["config", "core.sparseCheckout", "true"], None)?;
    git(dest, &["read-tree", "-mu", "HEAD"], None)?;
    Ok(())
}

/// `git pack-objects --stdout` of exactly `objects` (raw bytes).
fn pack_objects(repo: &Path, objects: &[String]) -> Result<Vec<u8>, SeamError> {
    let mut child = Command::new("git")
        .args(["pack-objects", "--stdout", "-q"])
        .current_dir(repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| SeamError(format!("cannot run git: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped");
    let list = objects.join(
        "
",
    ) + "
";
    let writer = std::thread::spawn(move || stdin.write_all(list.as_bytes()));
    let out = child
        .wait_with_output()
        .map_err(|e| SeamError(format!("git pack-objects: {e}")))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(SeamError(format!(
            "git pack-objects: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// The host side of one sandbox (sandbox.md "Mounts").
#[derive(Debug, Clone)]
pub struct HostLayout {
    /// The checkout the agent lane works on (cloned per attempt).
    pub repo_root: PathBuf,
    /// Per-attempt clones, `/task` dirs and skill copies
    /// (`agent_lane::sandbox_work_root`, outside the checkout).
    pub work_root: PathBuf,
    /// `<repo>/.guard/agents/rendered` (`guard agents sync`); the runtime's
    /// subdirectory is mounted read-only at `/agents` when it exists.
    pub rendered_root: PathBuf,
    /// Fake runtime only: `<dir>/<KEY>/<role>-<attempt>.json` (else
    /// `<role>.json`) is copied to `/task/fake-plan.json`.
    pub fake_plans: Option<PathBuf>,
    /// Keep every attempt dir after the attempt (`GUARD_AGENT_KEEP_WORK=1`,
    /// for inspection); by default `release` removes it.
    pub keep_work: bool,
}

impl HostLayout {
    fn attempt_dir(&self, r: &LayoutRequest<'_>) -> PathBuf {
        self.work_root
            .join(&r.input.run_id)
            .join(&r.input.ticket)
            .join(format!("{}-{}-{}", r.role.as_str(), r.attempt, r.agent))
    }

    fn fake_plan(&self, r: &LayoutRequest<'_>) -> Option<PathBuf> {
        let dir = self.fake_plans.as_ref()?.join(&r.input.ticket);
        [
            format!("{}-{}.json", r.role.as_str(), r.attempt),
            format!("{}.json", r.role.as_str()),
        ]
        .into_iter()
        .map(|name| dir.join(name))
        .find(|p| p.is_file())
    }

    fn prepare(&self, r: &LayoutRequest<'_>) -> Result<SandboxLayout, SeamError> {
        let dir = self.attempt_dir(r);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(io("clean attempt dir"))?;
        }
        let task_dir = dir.join("task");
        std::fs::create_dir_all(&task_dir).map_err(io("create task dir"))?;
        let checkout = dir.join("checkout");
        let branch = format!("agent/{}", r.input.ticket);
        if r.role == Role::Coder {
            export_without(
                &self.repo_root,
                &r.input.base_commit,
                &r.input.tests_dir,
                &checkout,
                &branch,
            )?;
        } else {
            git(
                &self.repo_root,
                &[
                    "clone",
                    "--quiet",
                    "--no-hardlinks",
                    "--no-checkout",
                    &slash(&self.repo_root),
                    &slash(&checkout),
                ],
                None,
            )?;
            git(&checkout, &["config", "core.autocrlf", "false"], None)?;
            git(
                &checkout,
                &["checkout", "--quiet", "-B", &branch, &r.input.base_commit],
                None,
            )?;
        }
        if r.role == Role::Qa {
            // The QA's writable mount; created with the clone, well before the
            // sandbox binds it (a bind of a just-created dir has been seen to
            // fail with EIO on Docker Desktop).
            std::fs::create_dir_all(checkout.join(&r.input.tests_dir).join("features"))
                .map_err(io("create features dir"))?;
        }
        if r.runtime == RuntimeKind::Fake {
            if let Some(plan) = self.fake_plan(r) {
                std::fs::copy(&plan, task_dir.join("fake-plan.json"))
                    .map_err(io("copy fake plan"))?;
            }
        }
        let sealed_spec = match (r.role, r.sealed) {
            (Role::Coder, Some((sealed, bytes))) => {
                let host = dir.join("sealed").join(
                    Path::new(&sealed.path)
                        .file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("sealed.spec.ts")),
                );
                std::fs::create_dir_all(host.parent().expect("has a parent"))
                    .map_err(io("create sealed dir"))?;
                std::fs::write(&host, bytes).map_err(io("write sealed copy"))?;
                Some((slash(&host), sealed.path.clone()))
            }
            _ => None,
        };
        // A private copy, so the bind source lives in the work dir and never
        // under the checkout (see `sandbox_work_root`).
        let rendered = self.rendered_root.join(r.runtime.as_str());
        let skills = dir.join("skills");
        if rendered.is_dir() {
            copy_dir(&rendered, &skills)?;
        }
        Ok(SandboxLayout {
            run_id: r.input.run_id.clone(),
            ticket: r.input.ticket.clone(),
            attempt: r.attempt,
            image: r.image.to_string(),
            checkout: slash(&checkout),
            task_dir: slash(&task_dir),
            codegraph_index: None,
            sealed_spec,
            tests_dir: r.input.tests_dir.clone(),
            skills_dir: skills.is_dir().then(|| slash(&skills)),
        })
    }
}

#[async_trait::async_trait]
impl LayoutProvider for HostLayout {
    async fn layout(&self, request: &LayoutRequest<'_>) -> Result<SandboxLayout, SeamError> {
        // Owned copies for the blocking task.
        let this = self.clone();
        let input = request.input.clone();
        let sealed = request.sealed.map(|(s, b)| (s.clone(), b.to_vec()));
        let (role, agent, runtime, attempt, image) = (
            request.role,
            request.agent.to_string(),
            request.runtime,
            request.attempt,
            request.image.to_string(),
        );
        blocking(move || {
            let r = LayoutRequest {
                input: &input,
                role,
                agent: &agent,
                runtime,
                attempt,
                image: &image,
                sealed: sealed.as_ref().map(|(s, b)| (s, b.as_slice())),
            };
            this.prepare(&r)
        })
        .await
    }

    async fn release(&self, layout: &SandboxLayout) -> Result<(), SeamError> {
        if self.keep_work {
            return Ok(());
        }
        let checkout = PathBuf::from(&layout.checkout);
        let dir = checkout
            .parent()
            .filter(|_| checkout.file_name().is_some_and(|n| n == "checkout"))
            .map(Path::to_path_buf)
            .ok_or_else(|| SeamError(format!("{} is not an attempt checkout", layout.checkout)))?;
        let root = std::fs::canonicalize(&self.work_root).map_err(io("work root"))?;
        let real = match std::fs::canonicalize(&dir) {
            Ok(p) => p,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io("attempt dir")(e)),
        };
        if !real.starts_with(&root) || real == root {
            return Err(SeamError(format!(
                "{} is outside the work root {}",
                dir.display(),
                self.work_root.display()
            )));
        }
        blocking(move || std::fs::remove_dir_all(&real).map_err(io("remove attempt dir"))).await
    }
}

/// The fake code host of the agent lane: the PR is branch `agent/<KEY>` in
/// the orchestrator's checkout (diff on `base_commit` + the sealed spec
/// overlaid), and its title and body are written to
/// `.guard/agents/prs/<KEY>.md`. It never merges: every agent PR requires a
/// human approval. [`CodeHostPublisher`] pushes this branch to the real host.
#[derive(Debug, Clone)]
pub struct LocalBranchPublisher {
    pub repo_root: PathBuf,
    pub work_root: PathBuf,
}

impl LocalBranchPublisher {
    fn publish_blocking(&self, r: &PrRequest) -> Result<PublishedPr, SeamError> {
        let dir = self
            .work_root
            .join("publish")
            .join(r.branch.replace('/', "-"));
        if dir.exists() {
            let _ = git(
                &self.repo_root,
                &["worktree", "remove", "--force", &slash(&dir)],
                None,
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
        git(&self.repo_root, &["worktree", "prune"], None)?;
        git(
            &self.repo_root,
            &[
                "worktree",
                "add",
                "--detach",
                "--quiet",
                &slash(&dir),
                &r.base_commit,
            ],
            None,
        )?;
        let result = (|| {
            if !r.diff.trim().is_empty() {
                git(
                    &dir,
                    &["apply", "--index", "--whitespace=nowarn", "-"],
                    Some(r.diff.as_bytes()),
                )?;
            }
            let spec = dir.join(&r.sealed_spec.path);
            std::fs::create_dir_all(spec.parent().expect("has a parent"))
                .map_err(io("create spec dir"))?;
            std::fs::write(&spec, &r.sealed_bytes).map_err(io("overlay sealed spec"))?;
            git(&dir, &["add", "--", &r.sealed_spec.path], None)?;
            let message = format!("{}\n\n{}", r.title, r.body);
            git(
                &dir,
                &["commit", "--quiet", "--no-verify", "-F", "-"],
                Some(message.as_bytes()),
            )?;
            let sha = git(&dir, &["rev-parse", "HEAD"], None)?;
            git(
                &self.repo_root,
                &["branch", "--force", &r.branch, &sha],
                None,
            )?;
            Ok(sha)
        })();
        let _ = git(
            &self.repo_root,
            &["worktree", "remove", "--force", &slash(&dir)],
            None,
        );
        let sha = result?;
        let prs = self.repo_root.join(".guard").join("agents").join("prs");
        std::fs::create_dir_all(&prs).map_err(io("create prs dir"))?;
        let ticket = r.branch.trim_start_matches("agent/");
        std::fs::write(
            prs.join(format!("{ticket}.md")),
            format!(
                "# {}\n\nbranch: {}\ncommit: {sha}\nstatus: open (human approval required; never auto-merged)\n\n{}",
                r.title, r.branch, r.body
            ),
        )
        .map_err(io("write PR record"))?;
        Ok(PublishedPr {
            url: format!("local:{}@{}", r.branch, &sha[..12.min(sha.len())]),
            number: None,
        })
    }
}

#[async_trait::async_trait]
impl PrPublisher for LocalBranchPublisher {
    async fn publish(&self, request: &PrRequest) -> Result<PublishedPr, SeamError> {
        let this = self.clone();
        let request = request.clone();
        blocking(move || this.publish_blocking(&request)).await
    }
}

/// The real agent PR: [`LocalBranchPublisher`] builds `agent/<KEY>` in the
/// orchestrator's checkout, then the code host publisher
/// (`guard_codehost::publish`, docs/contracts/pr-publisher.md) force-pushes
/// that branch and opens or updates its draft PR / MR. Never merges.
pub struct CodeHostPublisher {
    pub local: LocalBranchPublisher,
    pub remote: Arc<dyn PullRequestPublisher>,
}

impl CodeHostPublisher {
    fn agent_pull_request(r: &PrRequest) -> AgentPullRequest {
        let mut pr = AgentPullRequest::new(&r.ticket, &r.title, r.sealed_spec.clone());
        pr.summary = r.summary.clone();
        pr.base_commit = r.base_commit.clone();
        pr.attempts = r.attempts;
        pr.cost_usd = r.cost_usd;
        pr.coder = r.coder.clone();
        pr
    }
}

#[async_trait::async_trait]
impl PrPublisher for CodeHostPublisher {
    async fn publish(&self, request: &PrRequest) -> Result<PublishedPr, SeamError> {
        self.local.publish(request).await?;
        let push = BranchPush {
            local_repo: self.local.repo_root.clone(),
            rev: request.branch.clone(),
            ticket: request.ticket.clone(),
        };
        let (_, opened) = guard_codehost::publish::publish_agent_pr(
            self.remote.as_ref(),
            &push,
            &Self::agent_pull_request(request),
        )
        .await
        .map_err(|e: CodeHostError| SeamError(format!("code host: {e}")))?;
        Ok(PublishedPr {
            url: opened.url,
            number: Some(opened.number),
        })
    }
}

/// Which publisher the lane uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherKind {
    /// `[code_host].kind`: pushes and opens a draft PR / MR.
    CodeHost(&'static str),
    /// The local branch only, and why.
    Local(String),
}

impl PublisherKind {
    pub fn describe(&self) -> String {
        match self {
            Self::CodeHost(kind) => format!("{kind} draft PR"),
            Self::Local(why) => format!("local branch ({why})"),
        }
    }
}

/// The lane's PR publisher: the code host of `target.toml` `[code_host]`
/// when its publisher settings resolve from `lookup` (token, repository),
/// else the local branch. `fake` (`--fake`) always stays local.
pub fn pr_publisher(
    fake: bool,
    target: &TargetConfig,
    local: LocalBranchPublisher,
    lookup: impl Fn(&str) -> Option<String>,
) -> (Arc<dyn PrPublisher>, PublisherKind) {
    if fake {
        return (Arc::new(local), PublisherKind::Local("--fake".into()));
    }
    let Some(code_host) = &target.code_host else {
        return (
            Arc::new(local),
            PublisherKind::Local("no [code_host] in target.toml".into()),
        );
    };
    match guard_codehost::publish::from_lookup(code_host.kind, lookup) {
        Ok(remote) => (
            Arc::new(CodeHostPublisher { local, remote }),
            PublisherKind::CodeHost(code_host.kind.as_str()),
        ),
        Err(e) => (Arc::new(local), PublisherKind::Local(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_agents::pipeline::seams::TicketInput;
    use guard_core::ports::SealedSpec;
    use guard_core::ports::{CodeHostProvider, OpenedPullRequest, PushedBranch};

    fn repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/app.mjs"), "export const n = 1;\n").unwrap();
        git(p, &["init", "--quiet", "-b", "main"], None).unwrap();
        git(p, &["add", "-A"], None).unwrap();
        git(p, &["commit", "--quiet", "-m", "base"], None).unwrap();
        let base = git(p, &["rev-parse", "HEAD"], None).unwrap();
        (dir, base)
    }

    fn input(base: &str) -> TicketInput {
        TicketInput {
            repo: "acme/shop".into(),
            ticket: "SHOP-1".into(),
            run_id: "run-1".into(),
            base_commit: base.into(),
            summary: "s".into(),
            acceptance_criteria: vec![],
            app_paths: vec!["src/**".into()],
            tests_dir: "tests".into(),
            labels: vec![],
            predicted_files: vec![],
            preview: None,
            package_mirror: None,
            experiment: None,
            areas: vec![],
            journeys: vec![],
            claim: 0,
        }
    }

    #[tokio::test]
    async fn layouts_are_standalone_clones_with_task_dir_plan_and_sealed_copy() {
        let (repo, base) = repo();
        let plans = repo.path().join("plans/SHOP-1");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::write(plans.join("coder-2.json"), "{\"steps\": []}").unwrap();
        let layout = HostLayout {
            repo_root: repo.path().to_path_buf(),
            work_root: repo.path().join(".guard/agents/work"),
            rendered_root: repo.path().join(".guard/agents/rendered"),
            fake_plans: Some(repo.path().join("plans")),
            keep_work: false,
        };
        let sealed = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"sealed");
        let input = input(&base);
        let l = layout
            .layout(&LayoutRequest {
                input: &input,
                role: Role::Coder,
                agent: "coder",
                runtime: RuntimeKind::Fake,
                attempt: 2,
                image: "img@sha256:00",
                sealed: Some((&sealed, b"sealed")),
            })
            .await
            .unwrap();
        let checkout = PathBuf::from(&l.checkout);
        assert!(checkout.join(".git").is_dir(), "standalone clone");
        assert_eq!(
            std::fs::read(checkout.join("src/app.mjs")).unwrap(),
            b"export const n = 1;\n"
        );
        assert_eq!(git(&checkout, &["rev-parse", "HEAD"], None).unwrap(), base);
        assert_eq!(
            git(&checkout, &["branch", "--show-current"], None).unwrap(),
            "agent/SHOP-1"
        );
        assert!(PathBuf::from(&l.task_dir).join("fake-plan.json").is_file());
        let (host, target) = l.sealed_spec.unwrap();
        assert_eq!(target, "tests/features/SHOP-1.spec.ts");
        assert_eq!(std::fs::read(host).unwrap(), b"sealed");
        assert_eq!(l.skills_dir, None);
    }

    /// Regression: the Coder's `/workspace/.git` held every test blob, so
    /// `git show HEAD:tests/smoke/...` printed the hidden suites.
    #[tokio::test]
    async fn the_coder_checkout_holds_no_test_blobs_in_its_history() {
        let (repo, _) = repo();
        let p = repo.path();
        std::fs::create_dir_all(p.join("tests/smoke")).unwrap();
        std::fs::write(
            p.join("tests/smoke/old.spec.ts"),
            "old-hidden
",
        )
        .unwrap();
        git(p, &["add", "-A"], None).unwrap();
        git(p, &["commit", "--quiet", "-m", "old"], None).unwrap();
        std::fs::write(
            p.join("tests/smoke/old.spec.ts"),
            "hidden-assertion
",
        )
        .unwrap();
        git(p, &["add", "-A"], None).unwrap();
        git(p, &["commit", "--quiet", "-m", "base"], None).unwrap();
        let base = git(p, &["rev-parse", "HEAD"], None).unwrap();
        let work = tempfile::tempdir().unwrap();
        let layout = HostLayout {
            repo_root: p.to_path_buf(),
            work_root: work.path().to_path_buf(),
            rendered_root: p.join(".guard/agents/rendered"),
            fake_plans: None,
            keep_work: false,
        };
        let input = input(&base);
        let sealed = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"sealed");
        let request = |role, sealed| LayoutRequest {
            input: &input,
            role,
            agent: "a",
            runtime: RuntimeKind::Fake,
            attempt: 1,
            image: "img@sha256:00",
            sealed,
        };
        let l = layout
            .layout(&request(Role::Coder, Some((&sealed, b"sealed".as_slice()))))
            .await
            .unwrap();
        let co = PathBuf::from(&l.checkout);
        assert_eq!(git(&co, &["rev-parse", "HEAD"], None).unwrap(), base);
        assert_eq!(
            std::fs::read(co.join("src/app.mjs")).unwrap(),
            b"export const n = 1;
"
        );
        assert!(!co.join("tests/smoke/old.spec.ts").exists());
        assert!(git(&co, &["show", "HEAD:tests/smoke/old.spec.ts"], None).is_err());
        assert!(
            git(&co, &["log", "--oneline"], None)
                .unwrap()
                .lines()
                .count()
                == 1
        );
        for blob in [
            git(p, &["rev-parse", "HEAD:tests/smoke/old.spec.ts"], None).unwrap(),
            git(p, &["rev-parse", "HEAD~1:tests/smoke/old.spec.ts"], None).unwrap(),
        ] {
            assert!(
                git(&co, &["cat-file", "-e", &blob], None).is_err(),
                "blob {blob} is readable"
            );
        }
        assert_eq!(git(&co, &["status", "--porcelain"], None).unwrap(), "");
        // The QA still sees the existing tests.
        let q = layout.layout(&request(Role::Qa, None)).await.unwrap();
        assert_eq!(
            std::fs::read(PathBuf::from(&q.checkout).join("tests/smoke/old.spec.ts")).unwrap(),
            b"hidden-assertion
"
        );
    }

    #[tokio::test]
    async fn released_layouts_leave_nothing_behind() {
        let (repo, base) = repo();
        let work = tempfile::tempdir().unwrap();
        let layout = HostLayout {
            repo_root: repo.path().to_path_buf(),
            work_root: work.path().to_path_buf(),
            rendered_root: repo.path().join(".guard/agents/rendered"),
            fake_plans: None,
            keep_work: false,
        };
        let input = input(&base);
        let l = layout
            .layout(&LayoutRequest {
                input: &input,
                role: Role::Qa,
                agent: "qa",
                runtime: RuntimeKind::Fake,
                attempt: 1,
                image: "img@sha256:00",
                sealed: None,
            })
            .await
            .unwrap();
        let attempt_dir = PathBuf::from(&l.checkout).parent().unwrap().to_path_buf();
        assert!(attempt_dir.is_dir());
        // Kept for inspection when asked (GUARD_AGENT_KEEP_WORK=1).
        let keep = HostLayout {
            keep_work: true,
            ..layout.clone()
        };
        keep.release(&l).await.unwrap();
        assert!(attempt_dir.is_dir(), "kept on request");
        layout.release(&l).await.unwrap();
        assert!(!attempt_dir.exists(), "attempt clone removed");
        // A layout outside the work root is never deleted.
        let mut outside = l.clone();
        outside.checkout = slash(&repo.path().join("checkout"));
        assert!(layout.release(&outside).await.is_err());
        assert!(repo.path().join("src/app.mjs").is_file());
    }

    #[tokio::test]
    async fn the_publisher_creates_the_agent_branch_with_the_sealed_spec() {
        let (repo, base) = repo();
        let publisher = LocalBranchPublisher {
            repo_root: repo.path().to_path_buf(),
            work_root: repo.path().join(".guard/agents/work"),
        };
        let diff = "diff --git a/src/app.mjs b/src/app.mjs\n--- a/src/app.mjs\n+++ b/src/app.mjs\n@@ -1 +1 @@\n-export const n = 1;\n+export const n = 2;\n";
        let sealed = SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"spec\n");
        let pr = publisher
            .publish(&PrRequest {
                diff: diff.into(),
                sealed_spec: sealed,
                ..pr_request(&base)
            })
            .await
            .unwrap();
        assert!(pr.url.starts_with("local:agent/SHOP-1@"), "{}", pr.url);
        let p = repo.path();
        assert_eq!(
            git(p, &["show", "agent/SHOP-1:src/app.mjs"], None).unwrap(),
            "export const n = 2;"
        );
        assert_eq!(
            git(
                p,
                &["show", "agent/SHOP-1:tests/features/SHOP-1.spec.ts"],
                None
            )
            .unwrap(),
            "spec"
        );
        assert_eq!(git(p, &["rev-parse", "agent/SHOP-1^"], None).unwrap(), base);
        assert_eq!(
            git(p, &["rev-parse", "HEAD"], None).unwrap(),
            base,
            "main untouched"
        );
        let record = std::fs::read_to_string(p.join(".guard/agents/prs/SHOP-1.md")).unwrap();
        assert!(record.contains("human approval required"));
    }

    fn pr_request(base: &str) -> PrRequest {
        let diff = "diff --git a/src/app.mjs b/src/app.mjs\n--- a/src/app.mjs\n+++ b/src/app.mjs\n@@ -1 +1 @@\n-export const n = 1;\n+export const n = 2;\n";
        PrRequest {
            repo: "acme/shop".into(),
            ticket: "SHOP-1".into(),
            branch: "agent/SHOP-1".into(),
            base_commit: base.into(),
            diff: diff.into(),
            sealed_spec: SealedSpec::seal("tests/features/SHOP-1.spec.ts", b"spec\n"),
            sealed_bytes: b"spec\n".to_vec(),
            title: "SHOP-1: s".into(),
            body: "Human approval required before merge.".into(),
            summary: "Shows the stock badge.".into(),
            attempts: 2,
            cost_usd: 0.25,
            coder: Some("coder (claude-code, sonnet)".into()),
        }
    }

    #[derive(Default)]
    struct RecordingHost {
        pushes: std::sync::Mutex<Vec<(BranchPush, String)>>,
        prs: std::sync::Mutex<Vec<AgentPullRequest>>,
        fail_open: bool,
    }

    #[async_trait::async_trait]
    impl PullRequestPublisher for RecordingHost {
        fn provider(&self) -> CodeHostProvider {
            CodeHostProvider::Github
        }
        async fn push_branch(&self, push: &BranchPush) -> Result<PushedBranch, CodeHostError> {
            // The branch must already exist locally when the push runs.
            let commit = git(&push.local_repo, &["rev-parse", &push.rev], None)
                .map_err(|e| CodeHostError::Unconfigured(e.0))?;
            self.pushes
                .lock()
                .unwrap()
                .push((push.clone(), commit.clone()));
            Ok(PushedBranch {
                branch: push.branch(),
                commit,
            })
        }
        async fn open_pull_request(
            &self,
            pr: &AgentPullRequest,
        ) -> Result<OpenedPullRequest, CodeHostError> {
            if self.fail_open {
                return Err(CodeHostError::Rejected {
                    status: 422,
                    body: "nope".into(),
                });
            }
            self.prs.lock().unwrap().push(pr.clone());
            Ok(OpenedPullRequest {
                provider: CodeHostProvider::Github,
                number: 41,
                url: "https://github.com/acme/shop/pull/41".into(),
                created: true,
                draft: true,
                labels: vec!["agent".into()],
            })
        }
    }

    #[tokio::test]
    async fn the_code_host_publisher_builds_the_branch_then_pushes_and_opens_a_draft_pr() {
        let (repo, base) = repo();
        let host = Arc::new(RecordingHost::default());
        let publisher = CodeHostPublisher {
            local: LocalBranchPublisher {
                repo_root: repo.path().to_path_buf(),
                work_root: repo.path().join(".guard/agents/work"),
            },
            remote: host.clone(),
        };
        let published = publisher.publish(&pr_request(&base)).await.unwrap();
        assert_eq!(published.url, "https://github.com/acme/shop/pull/41");
        assert_eq!(published.number, Some(41));
        let pushes = host.pushes.lock().unwrap().clone();
        assert_eq!(pushes.len(), 1);
        assert_eq!(pushes[0].0.local_repo, repo.path());
        assert_eq!(pushes[0].0.rev, "agent/SHOP-1");
        assert_eq!(pushes[0].0.ticket, "SHOP-1");
        assert_eq!(
            git(repo.path(), &["rev-parse", "agent/SHOP-1^"], None).unwrap(),
            base
        );
        let prs = host.prs.lock().unwrap().clone();
        assert_eq!(prs.len(), 1);
        let pr = &prs[0];
        assert_eq!(pr.ticket, "SHOP-1");
        assert_eq!(pr.title, "SHOP-1: s");
        assert_eq!(pr.summary, "Shows the stock badge.");
        assert_eq!(pr.base_commit, base);
        assert_eq!(pr.sealed_spec.path, "tests/features/SHOP-1.spec.ts");
        assert_eq!(pr.attempts, 2);
        assert!((pr.cost_usd - 0.25).abs() < 1e-9);
        assert_eq!(pr.coder.as_deref(), Some("coder (claude-code, sonnet)"));
        assert!(pr.draft, "agent PRs are drafts");
    }

    #[tokio::test]
    async fn a_code_host_failure_is_a_seam_error() {
        let (repo, base) = repo();
        let publisher = CodeHostPublisher {
            local: LocalBranchPublisher {
                repo_root: repo.path().to_path_buf(),
                work_root: repo.path().join(".guard/agents/work"),
            },
            remote: Arc::new(RecordingHost {
                fail_open: true,
                ..RecordingHost::default()
            }),
        };
        let err = publisher.publish(&pr_request(&base)).await.unwrap_err();
        assert!(err.0.contains("code host"), "{}", err.0);
    }

    fn target(code_host: bool) -> guard_core::config::TargetConfig {
        let mut toml = String::from(
            "version = 1\n[app]\nname = \"shop\"\nlanguages = [\"javascript\"]\nbase_url = \"http://localhost:3000\"\n[areas.a]\ndescription = \"a\"\npaths = [\"src/**\"]\n",
        );
        if code_host {
            toml.push_str("[code_host]\nkind = \"github\"\n");
        }
        guard_core::config::parse_target(&toml).unwrap()
    }

    fn local(dir: &Path) -> LocalBranchPublisher {
        LocalBranchPublisher {
            repo_root: dir.to_path_buf(),
            work_root: dir.join("w"),
        }
    }

    #[test]
    fn the_publisher_is_the_code_host_only_with_a_token_and_never_in_fake_mode() {
        let dir = tempfile::tempdir().unwrap();
        let env = |k: &str| match k {
            "GITHUB_TOKEN" => Some("t".to_string()),
            "GITHUB_REPOSITORY" => Some("acme/shop".to_string()),
            _ => None,
        };
        let no_env = |_: &str| None;
        let (_, kind) = pr_publisher(false, &target(true), local(dir.path()), env);
        assert_eq!(kind, PublisherKind::CodeHost("github"));
        let (_, kind) = pr_publisher(true, &target(true), local(dir.path()), env);
        assert_eq!(kind, PublisherKind::Local("--fake".into()));
        let (_, kind) = pr_publisher(false, &target(false), local(dir.path()), env);
        assert!(
            matches!(&kind, PublisherKind::Local(why) if why.contains("[code_host]")),
            "{kind:?}"
        );
        let (_, kind) = pr_publisher(false, &target(true), local(dir.path()), no_env);
        assert!(
            matches!(&kind, PublisherKind::Local(why) if why.contains("GITHUB")),
            "{kind:?}"
        );
        assert!(kind.describe().starts_with("local branch"));
        assert_eq!(
            PublisherKind::CodeHost("gitlab").describe(),
            "gitlab draft PR"
        );
    }
}
