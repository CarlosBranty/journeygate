# Contract: agent PR publisher (Phase 4)

How the orchestrator publishes a lane-A ticket's result to the real code host. It pushes branch `agent/<KEY>` and opens, or updates, a draft PR (GitHub) or MR (GitLab). Port: `guard_core::ports::code_host::PullRequestPublisher`. Adapters: `guard_codehost::publish::{github::GithubPublisher, gitlab::GitlabPublisher}`.

This port is separate from `CodeHost` because only the orchestrator holds a token with push rights. The gate, the server and the sandboxes never get one.

## Invariants

- **Branch.** Always `agent/<KEY>`. The publisher never writes any other ref. A ticket key is ASCII letters, digits, `-`, `_` and `.`; it starts with a letter or digit, contains no `..`, does not end with `.`, and is at most 100 characters.
- **Draft by default.** `AgentPullRequest::new` sets `draft: true`. An existing draft is never un-drafted.
- **Never merged.** No adapter calls a merge endpoint, enables auto-merge (GitHub GraphQL `enablePullRequestAutoMerge`, GitLab `merge_when_pipeline_succeeds`) or changes PR state. A human approves and merges.
- **Re-publish holds for review.** Updating an existing open PR / MR follows a force-push of new commits, so before the update the adapter turns auto-merge off (GitHub GraphQL `disablePullRequestAutoMerge` when `auto_merge` is set; GitLab `POST .../cancel_merge_when_pipeline_succeeds`) and, for a draft request, puts a PR / MR a human marked ready back to draft (GitHub GraphQL `convertPullRequestToDraft`; GitLab the `Draft: ` title prefix). A failure there fails the publish before the update. Earlier approvals are not dismissed; the draft state blocks the merge until a human marks it ready again.
- **Idempotent.** If an open PR / MR already has head `agent/<KEY>`, it is updated (title, body, labels added) instead of being duplicated. This is also what happens when a concurrent create loses the race (GitHub 422 "already exists", GitLab 409): the adapter lists the open PRs again and updates the one it finds.
- **Labels.** `agent` always comes first, then the request's `labels`, then `GUARD_PR_LABELS`. They are trimmed and deduplicated, and none may contain a comma. Labels are only ever added; existing ones are never removed.
- **Validation is local.** An invalid request becomes `CodeHostError::Rejected { status: 0 }` before any request or git process starts.

## Port

```rust
pub trait PullRequestPublisher: Send + Sync {
    fn provider(&self) -> CodeHostProvider;
    async fn push_branch(&self, push: &BranchPush) -> Result<PushedBranch, CodeHostError>;
    async fn open_pull_request(&self, pr: &AgentPullRequest) -> Result<OpenedPullRequest, CodeHostError>;
}
```

| Type | Fields |
| --- | --- |
| `BranchPush` | `local_repo` (work tree or bare repo), `rev` (any revision that resolves to a commit, such as the branch `LocalBranchPublisher` builds), `ticket` |
| `PushedBranch` | `branch`, `commit` (full sha) |
| `AgentPullRequest` | `ticket`, `title`, `summary`, `base_branch: Option` (else the configured base, else the repository default branch), `base_commit`, `sealed_spec: SealedSpec`, `attempts`, `cost_usd`, `coder: Option`, `labels`, `draft` |
| `OpenedPullRequest` | `provider`, `number` (PR number / MR IID), `url`, `created` (`false` = updated), `draft`, `labels` (the ones guard applied) |

`guard_codehost::publish::publish_agent_pr(&dyn PullRequestPublisher, &BranchPush, &AgentPullRequest)` runs both steps in order. It refuses a push and a PR that name different tickets.

## Body

Both providers use the same body, `guard_codehost::publish::render_body`:

```markdown
<!-- guard:agent-pr SHOP-1 -->
**Agent PR for `SHOP-1`** (lane A). Human approval required: guard never merges agent PRs.

<summary, at most 50 000 characters>

| Field | Value |
| --- | --- |
| Ticket | `SHOP-1` |
| Branch | `agent/SHOP-1` |
| Base commit | `<sha>` |            (omitted when empty)
| Sealed spec | `tests/features/SHOP-1.spec.ts` |
| Sealed spec sha256 | `<64 hex>` |
| Attempts | 3 |
| Cost | $1.2346 |
| Coder | coder (claude-code, sonnet) |   (omitted when None)
```

Table cells have newlines removed, `|` escaped and backticks replaced.

## Push

`git -C <local_repo> -c credential.helper= push --no-verify --porcelain --force <push_url> <sha>:refs/heads/agent/<KEY>`

- The revision is resolved first with `git rev-parse --verify --end-of-options <rev>^{commit}`.
- The token never appears in argv or in the URL. git gets `http.<push_url>.extraHeader = Authorization: Basic base64(<user>:<token>)` through `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_0` / `GIT_CONFIG_VALUE_0` (git >= 2.31). The header is scoped to that URL and is set only for `https://` and loopback `http://` remotes. The user is `x-access-token` on GitHub and `oauth2` on GitLab.
- Prompts, credential helpers, askpass and the local `pre-push` hook are disabled. The push times out after 300 s.
- Accepted push URLs: `https://`, `ssh://`, `file://`, scp-like (`git@host:path`), local paths, and loopback `http://` (used by tests). Refused: other cleartext `http://` remotes and URLs that embed credentials.
- Errors: git's stderr is redacted (token and header) before it goes into the error. "Authentication failed", HTTP 401 and "terminal prompts disabled" map to `Unauthorized(401)`; HTTP 403 and "Permission to … denied" map to `Unauthorized(403)`; anything else maps to `Transport`.

## Requests

| Step | GitHub (REST `2022-11-28`, `Authorization: Bearer`) | GitLab (v4, `PRIVATE-TOKEN`) |
| --- | --- | --- |
| base branch | `GET /repos/{repo}` -> `default_branch` | `GET /projects/:id` -> `default_branch` |
| find | `GET /repos/{repo}/pulls?state=open&head={owner}:agent/<KEY>` | `GET /projects/:id/merge_requests?state=opened&source_branch=agent/<KEY>` |
| create | `POST /repos/{repo}/pulls` `{title, head, base, body, draft, maintainer_can_modify: false}` | `POST /projects/:id/merge_requests` `{source_branch, target_branch, title: "Draft: …", description, labels: "a,b"}` |
| update | first GraphQL `disablePullRequestAutoMerge` / `convertPullRequestToDraft` (`pullRequestId` = `node_id`) when needed, then `PATCH /repos/{repo}/pulls/{n}` `{title, body}` | first `POST /projects/:id/merge_requests/:iid/cancel_merge_when_pipeline_succeeds` when set, then `PUT /projects/:id/merge_requests/:iid` `{title, description, add_labels}` (keeps the `Draft:` prefix when the MR is a draft, puts it back for a draft request) |
| labels | `POST /repos/{repo}/labels` per label (201, or 422 when it already exists), then `POST /repos/{repo}/issues/{n}/labels` `{labels}` | inline in create / update (GitLab creates unknown labels) |

A base-branch lookup happens only when no base is set on the request or in the configuration. GitLab reads the draft state from `draft`, falling back to the deprecated `work_in_progress`, then to the title prefix.

References: <https://docs.github.com/en/rest/pulls/pulls>, <https://docs.github.com/en/rest/issues/labels>, <https://docs.gitlab.com/api/merge_requests/>, <https://docs.gitlab.com/user/project/merge_requests/drafts/>.

## Configuration

`target.toml` `[code_host].kind` selects the provider. Everything else comes from the environment. Use `guard_codehost::publish::from_target_env(&TargetConfig)`, `from_env(CodeHostKind)` or `from_lookup(kind, lookup)`.

| Variable | GitHub | GitLab |
| --- | --- | --- |
| `GUARD_PR_TOKEN_ENV` | names the token variable (default `GITHUB_TOKEN`) | names the token variable (default `GITLAB_TOKEN`) |
| repository | `GITHUB_REPOSITORY` (`owner/name`) | `GITLAB_PROJECT`, else `CI_PROJECT_PATH`, else `CI_PROJECT_ID` |
| API URL | `GITHUB_API_URL` (default `https://api.github.com`) | `GITLAB_API_URL`, else `CI_API_V4_URL` (default `https://gitlab.com/api/v4`) |
| `GUARD_PR_PUSH_URL` | default `https://github.com/{repo}.git`, or `https://<host>/{repo}.git` for GHES `https://<host>/api/v3` | default `<api without /api/v4>/<path>.git`; required for a numeric project id |
| `GUARD_PR_BASE_BRANCH` | default: repository default branch | same |
| `GUARD_PR_LABELS` | extra comma-separated labels | same |

The token needs push access plus pull-request write access. On GitHub that is `contents: write` and `pull_requests: write`, plus `issues: write` for labels. On GitLab it is the `api` and `write_repository` scopes. `Debug` on the configs redacts the token.

## Fake and wiring

The agent lane (`guard serve` and `guard agents run`) picks its `guard_agents` `PrPublisher` with `guard-cli` `agent_lane::host::pr_publisher`:

| Condition | Publisher | Startup line |
| --- | --- | --- |
| `--fake` | `LocalBranchPublisher` | `agents: PRs: local branch (--fake)` |
| no `[code_host]` in `target.toml` | `LocalBranchPublisher` | `agents: PRs: local branch (no [code_host] in target.toml)` |
| `[code_host]` set, but the settings above do not resolve (no token, no repository, a refused push URL) | `LocalBranchPublisher` | `agents: PRs: local branch (<why>)` |
| `[code_host]` set and the settings resolve | `CodeHostPublisher` | `agents: PRs: github draft PR` / `gitlab draft PR` |

`LocalBranchPublisher` builds `agent/<KEY>` in the orchestrator checkout (the Coder diff on `base_commit`, the sealed spec overlaid), writes `.guard/agents/prs/<KEY>.md` and never pushes. `CodeHostPublisher` first does the same, then calls `publish_agent_pr` with `BranchPush { local_repo: <checkout>, rev: "agent/<KEY>" }` and an `AgentPullRequest` mapped from the pipeline's `PrRequest`. `PrRequest` carries `ticket`, `summary` (the winning Coder's summary, else the ticket summary), `attempts` (every Coder attempt of the ticket), `cost_usd` (their spend) and `coder` (`<agent> (<runtime>, <model>)`). The pipeline's `StageOutcome::Passed.pr` then holds the real PR URL and number. A code-host error fails the Coder stage as a seam error ("code host: ..."); the local branch is kept.

## Tests

- `guard-codehost/tests/publish_github.rs` and `tests/publish_gitlab.rs`: wiremock with recorded response shapes in `tests/fixtures/{github,gitlab}/publish/`. They cover create, update instead of duplicate, the create race, draft handling, labels, the absence of any merge call, error mapping and config.
- `guard-codehost/tests/push.rs`: pushes to local bare repos (force-update, nothing else written), an unknown revision or remote, and the auth header asserted at a wiremock git endpoint.
