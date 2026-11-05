# Contract: agent sandbox

Permissions are imposed by mounts and network, never by prompts (architecture section 4). One fresh container per (ticket, role, attempt), destroyed at the end. Port: `guard_core::ports::SandboxProvider` (`create`, `put_file`, `read_file`, `exec`, `extract_diff`, `destroy`). Production adapter: `guard-sandbox` (`DockerSandbox`). Fake: `FakeSandboxProvider`.

`SandboxSpec::for_role(role, layout, egress)` builds the mounts below; `SandboxSpec::validate()` rejects anything weaker, and **every provider calls it in `create`** (`SandboxError::InvalidSpec`). Both are implemented and tested in `guard-core` (`ports::fakes::phase4_tests`).

## Mounts by role

`<tests>` = `[playwright].tests_dir` of `target.toml` (usually `tests`). Targets are fixed constants: `WORKSPACE_DIR = /workspace`, `TASK_DIR = /task`, `CODEGRAPH_DIR = /codegraph`, `SKILLS_DIR = /agents`.

| Target | QA | Coder |
| --- | --- | --- |
| `/workspace` (worktree, branch `agent/<KEY>`) | read-only | read-write |
| `/workspace/<tests>` | read-only (part of `/workspace`) | **hidden** (read-only bind of an empty host "skeleton" dir holding only the mount points of nested mounts: journeys and smoke are invisible; Docker cannot create the sealed spec's mount point inside a read-only tmpfs) |
| `/workspace/<tests>/features` | **read-write** (the only writable code path) | hidden |
| `/workspace/<tests>/features/<KEY>.spec.ts` | written here | **read-only bind of the sealed copy** (from orchestrator storage, not the QA worktree) |
| `/task` | read-write | read-write |
| `/codegraph` (CodeGraph index of this checkout) | read-only | read-only |
| `/agents` (rendered skills / `AGENTS.md`) | read-only | read-only |

Rules `validate` enforces:

- Coder: `/workspace` read-write; `<tests>` hidden or read-only; nothing writable under `<tests>`; the sealed spec present and read-only; no writable mount other than `/workspace` and `/task`.
- QA: `/workspace` read-only; only `<tests>/features` and `/task` writable; no sealed spec.
- `/codegraph` and `/agents` always read-only; targets absolute, unique, without `..`.
- The image is pinned by digest (`repo@sha256:<64 hex>`).
- `env` carries `CODEGRAPH_TELEMETRY=0` and `DO_NOT_TRACK=1` (`REQUIRED_ENV`); no env name ending in `_KEY`, `_TOKEN`, `_SECRET`, `_PASSWORD`, `_CREDENTIALS` (credentials go in `secrets: BTreeMap<String, Secret>`, injected at process start, never serialized or logged; `Secret` prints `***`).

Read-only does not stop a write to `package.json` in the Coder's worktree (it is inside the writable `/workspace`); that is caught by the diff allowlist (`AllowedPaths::coder` denies `CODER_PROTECTED_FILES`, qa-coder-protocol.md). Mounts are the first line, the diff check the second, the gate on a clean checkout the third.

## Network

Each sandbox sits on an internal Docker network (`docker network create --internal`, no route out). Each `EgressAllowlist` rule gets its own hardened forwarder container (`guard-sandbox/assets/egress-proxy.mjs`) that starts on the egress network (`GUARD_SANDBOX_EGRESS_NETWORK`, default `bridge`), is attached to the internal network, and relays TCP to exactly one `host:port`. In the sandbox the allowed names resolve to their forwarder (`--add-host`) and `--dns 127.0.0.1` makes every other name fail:

| Purpose | QA | Coder | Rule |
| --- | --- | --- | --- |
| `gateway` (LiteLLM, `[gateway].url`) | yes | yes | exactly one per sandbox |
| `preview` (the app under test, for Playwright MCP) | yes | **no** | |
| `package_mirror` (read-only registry mirror) | **no** | yes | |
| `copilot_exception` (Copilot-hosted models) | only with `copilot_exception = true` | same | set from `via_gateway = false` (agents-toml.md) |

Hosts are plain names (no wildcards, no paths). Everything else is dropped, DNS included (only a forwarder resolves its upstream). Verified live (guard-sandbox `docker_live`, guard-cli `agent_lane_e2e`): the gateway answers; `example.com`, `1.1.1.1` and `host.docker.internal` do not. The orchestrator, Postgres, MinIO, Jira and the code host are never reachable from a sandbox; there is no secret in the sandbox other than the virtual key.

## Isolation flags (Docker adapter)

Rootless Docker; `--read-only` root filesystem with a size-capped tmpfs at `/tmp` and `$HOME`; `--cap-drop=ALL`; `--security-opt no-new-privileges`; `--pids-limit`, `--cpus`, `--memory` from `ResourceLimits` (defaults 2 CPUs, 4096 MB, 512 pids, 1800 s); `--runtime=runsc` when gVisor is installed; a non-root user. The container only idles (`sleep <timeout + 300 s>`, so an orphaned sandbox stops by itself) and every agent process starts through `docker exec`. The container and its anonymous volumes are removed by `destroy` (idempotent; a failed removal is reported, and the next `destroy` retries it) and by a reaper (`DockerSandbox::reap_expired`: at lane start and every 5 minutes in `guard serve`) for containers labeled `guard.sandbox` whose `guard.deadline` passed. `guard serve` also runs `guard_agents::reaper::NetworkReaper` every 5 minutes: `guard.sandbox` networks (agent and check sandboxes) with no container and at least 15 minutes old are removed. Every `docker` management call is bounded (`DockerConfig::management_timeout`, default 300 s). A timed-out `exec` kills what it left running inside the container (killing the local `docker exec` client does not). Production target: Firecracker microVMs (Linux + KVM), same contract.

## Host layout (orchestrator)

Per (ticket, role, attempt) the orchestrator prepares, outside the checkout (`GUARD_AGENT_WORK_DIR`, default `<temp>/guard-agents/<repo>`): a **standalone repository** (`.git` a directory, `core.autocrlf=false`, branch `agent/<KEY>` at the base commit; a `git worktree` would point its `.git` file at a host path). The QA's is a full clone. The Coder's holds only the base commit (a shallow root) and the trees and blobs of its snapshot **minus every blob under `<tests>`**, with those paths skip-worktree (sparse checkout): its writable `/workspace/.git` cannot reveal the hidden journeys and smoke specs, at the base or in history (their file names stay visible in the trees), an empty `/task` dir (plus `fake-plan.json` for the fake runtime), the QA's `<tests>/features` dir, the Coder's host copy of the verified sealed spec, and a private copy of `.guard/agents/rendered/<runtime>` for `/agents`. The attempt dir is removed when the attempt ends (`GUARD_AGENT_KEEP_WORK=1` keeps it). Keeping these out of the checkout matters on Docker Desktop: a bind mount whose host ancestors hold a directory junction (pnpm `node_modules`) answered EIO after a few seconds in our tests. Red-first and the gate do not use host worktrees: they run in **check sandboxes** (`guard_sandbox::check`, `DockerSandbox::create_check`, `CheckSandboxProvider`): the same image and hardening as an agent sandbox, an `--internal` network with no egress forwarder (`--dns 127.0.0.1`), no mounts, `/workspace` a 1024 MB tmpfs filled from a tar of the candidate tree built on the host, and results copied out to `<work_root>/check-results/<check>/` (qa-coder-protocol.md step 4). Check results are not pruned yet. Only with `GUARD_UNSAFE_HOST_CHECKS=1` (development) do checks run in host worktrees, which link `node_modules` from the checkout and remove those links before `git worktree remove` (which follows junctions and would delete their targets).

## Provider operations

- `create(&SandboxSpec) -> Sandbox {id, role, run_id, ticket, attempt}`.
- `put_file` / `read_file`: files inside writable mounts (`/task/task.json` in, `/task/result.json` and `/task/events.jsonl` out). Missing file → `NotFound`.
- `exec(&ExecRequest {argv, env, workdir, timeout, stdin})`: argv run directly, never through a shell; non-zero exit is `Ok(ExecOutput { exit_code, .. })`; timeout sets `timed_out`.
- `extract_diff(base_commit) -> ExtractedDiff {base_commit, unified_diff, changed_paths}`: `git add -A` into a temporary index inside the sandbox and `git diff --no-renames --binary <base_commit>`, so untracked files count; `changed_paths` sorted, unique, repo-relative. It first kills every leftover agent process, and git runs with `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, `GIT_NO_REPLACE_OBJECTS=1` and `-c` overrides of `diff.noprefix`, `diff.mnemonicPrefix`, `diff.relative`, `diff.srcPrefix`/`dstPrefix` and `core.fsmonitor`, so the agent's git config and `refs/replace/*` cannot change the base or the header paths. The orchestrator still derives the paths to judge from the patch headers themselves and rejects a diff whose headers name a path `changed_paths` omits (qa-coder-protocol.md step 7).
- `destroy`: stop and remove. Unknown id → `NotFound`.

## CodeGraph in the sandbox

The image ships the CodeGraph CLI; the orchestrator builds the index per checkout (outside the sandbox) and mounts it read-only at `/codegraph`. Telemetry is off (`CODEGRAPH_TELEMETRY=0`, `DO_NOT_TRACK=1`). The harness logs, per run, the files CodeGraph's `affected`/`impact` report next to `guard-impact`'s fan-out for the same diff (agent_runs `result` metadata) so the two can be compared; `guard-impact` stays authoritative for routing. The image also ships `rtk-oss` (tokens.md).

## Testing

Unit and harness tests use `FakeSandboxProvider` (`with_exec(program, output)`, `with_diff`, `exec_log`, `is_destroyed`). Docker tests in `guard-sandbox` are `#[ignore]` and env-gated on `GUARD_DOCKER_TESTS=1`; they assert at least: a write to a read-only mount fails, the hidden tests dir is empty, a request to a non-allowlisted host fails, the env carries the telemetry vars, and `destroy` leaves no container behind.
