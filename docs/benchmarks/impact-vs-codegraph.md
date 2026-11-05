# Benchmark: guard-impact vs CodeGraph for `guard_map`

**Decision: `guard_map_uses_guard_impact`.** guard-impact scored higher on both pre-registered criteria. Its macro F1 for import edges is 0.981 against CodeGraph's 0.520, and its macro recall for impact selection is 0.955 against 0.866. The result holds when the Go repository, whose ground truth is hardest on CodeGraph, is removed: F1 0.974 vs 0.631 and recall 0.940 vs 0.905.

Run on 2026-11-01. Windows 11 with Git Bash, Rust 1.88, Node 24.14, Python 3.11.5, Go 1.27.0. CodeGraph CLI/SDK v1.6.0. v1.6.2 exists but was not installed, because the benchmark rules forbid `upgrade`.

## Pre-registered decision rule

This rule was fixed before any data was produced:

1. Compute the macro-average over the 4 repos of the metric-1 F1.
2. Compute the macro-average over the 4 repos of the metric-2 recall.
3. If guard-impact is lower than CodeGraph on either average, the decision is `guard_map_uses_codegraph`, and guard-impact stays only for impact selection with glob fallback. Otherwise the decision is `guard_map_uses_guard_impact`.

The primary affected set was also fixed before data was produced. For both tools it is the depth-3 reverse dependents of the changed files, each tool using its own file-level dependents API. Recall per repo is the mean of per-commit recall. Micro-averages and other variants are reported only as context.

## Interpretation: where guard-impact wins, and where it does not

### Why guard-impact scores higher

The two tools answer different questions. guard-impact answers *which file imports which module file*, and that is exactly the question impact selection asks. CodeGraph answers *which symbol references which symbol*, then projects those references onto files. The projection is where it loses the file-level question:

- **guard-impact resolves each import with the language's own rules.** It uses TypeScript module resolution with `tsconfig` `paths`/`baseUrl`/`extends`, the Python search path as `importlib` sees it, Cargo targets plus `mod`/`#[path]` module trees, and Go package membership. When an import names a file, guard-impact gets that file.
- **CodeGraph over-approximates through name references.** A reference that matches by name creates a file edge, even when nothing imports that file. On hono this gave 962 false edges against 872 true ones (precision 0.475).
- **CodeGraph under-approximates when no symbol edge is resolved.** An import whose symbols are never resolved to a reference produces no edge: 164 missed edges on hono and 77 on ripgrep.
- **Go package semantics.** In Go, importing package P makes a file depend on every file of P. CodeGraph only links the files whose symbols are actually referenced, so its import recall on caddy is 0.123 (6,594 missed edges). guard-impact models the package and scores 1.000.

### Tasks where guard-impact did better, with numbers

| Task | guard-impact | CodeGraph | Why it matters for `guard` |
|---|---|---|---|
| Import-edge accuracy (macro F1) | 0.981, then 1.000 after fixes | 0.520 | False edges in `guard_map` burn the agent's token budget on irrelevant files. Missing edges hide the file the agent actually needs. |
| Not missing affected files (macro recall, depth 3) | 0.955, then 1.000 | 0.866 | Recall is safety. A missed dependent means a missed test, which means a regression that escapes the probe. |
| Not over-selecting (macro precision, depth 3) | 1.000 | 0.414 | Precision is cost. CodeGraph would flag about 2.4x more files, so the probe runs more journeys and shards cost more minutes. |
| Go repo (caddy) | F1 1.000, recall 1.000 | F1 0.189, recall 0.749 | Package-level semantics; see above. |
| Never returning an empty prediction | 0 of 200 commits empty | 8 of 50 ripgrep commits empty | An empty prediction on a real change is the worst case: nothing is selected. |
| Test-file selection (context only) | 0.999–1.000 recall on all 4 repos | 0.000 on rich and caddy with the default CLI filter | Test selection is the core job of `guard affected`. |
| Index build | 0.7–2.3 s | 4.1–11.8 s (3.4–8.5x slower) | Rebuilt per checkout, per PR, in CI. |
| Depth-3 query | 20–315 µs | 12–31 ms through the SDK, about 0.6 s through the CLI | About 1,000x faster in-process. Cheap enough for every MCP call, every scheduler conflict check and every staleness pass. |

### Where CodeGraph is better (and why we keep it)

The benchmark scores only file-level import semantics. CodeGraph does work that guard-impact does not do at all:

- **Symbol-level graph.** Functions, methods, types, callers and callees, and call paths between symbols. guard-impact has no symbols, only files.
- **Code exploration.** Questions like "how does X work", "who calls Y" or "what breaks if I change this function's signature" need the symbol graph and the returned source. That is CodeGraph's job, and the reason it stays mounted read-only in the sandbox (doc section 19).
- **Same-package and re-export references.** These are real dependencies that are not imports. Some of CodeGraph's "false positives" are exactly those.
- **Language coverage.** CodeGraph covers 30+ languages. guard-impact covers TypeScript, JavaScript, Python, Go and Rust, and falls back to `target.toml` globs for anything else.
- **Full-text search** over the indexed code.

So the split is: **guard-impact for "what does this change affect"** (test selection, `guard_map`, blast radius, conflicts, staleness), and **CodeGraph for "how does this code work"** (agent exploration).

### What neither tool sees

Static import graphs miss dependencies that only exist at run time: computed `import()`/`require` paths, dependency-injection containers, reflection, plugin registries, config-driven routes, templates that name components by string, and cross-service calls over HTTP or queues. That is why the guard still runs the full suite on every PR, and impact only orders the probe and filters local runs.

### Other places guard-impact can be used

It is a plain synchronous library (`ImpactIndex::build`, `imports_of`, `importers_of`, `transitive_importers`, `fan_out`), so it fits anywhere a "who depends on this file" answer is needed:

- **Already used in `guard`:**
  - Impact selection (`guard affected`, `guard test --affected`).
  - The Gatekeeper's fan-out hard rule.
  - The `guard_map` repo map.
  - Scheduler conflict avoidance between parallel agent tasks.
  - Memory-fact staleness (a commit touching a fact's files marks it `needs_review`).
- **Unit and integration test selection in any CI**, not only Playwright: run the test files in the reverse closure of the diff.
- **Monorepo affected builds:** build or deploy only the packages reached by a change, in the spirit of `nx affected` or `turbo --filter`, without adopting those tools.
- **Reviewer routing:** map the blast radius to `target.toml` areas or CODEOWNERS, so the right people review a wide change.
- **Architecture rules:** fail CI when an import crosses a forbidden layer. One example is a domain module importing an adapter in a hexagonal layout.
- **Dead-file detection:** files that nothing imports and that are not entry points.
- **Risk signals for triage:** a high fan-out on a ticket's files is an objective "this is risky" input, independent of the LLM.
- **Local pre-push:** run only the affected tests before pushing.

### How far to trust these numbers

- **In-sample after fixes.** The defects were found and fixed on these same four repos.
- **Shared rules with the ground truth.** The Python and Rust ground truths follow the same rules guard-impact now implements, and the Rust one was written by the same author.
- **Before fixes, the result was already decisive.** guard-impact led by 0.46 F1 and 0.09 recall, under a rule fixed before any data existed.
- **Not yet tested elsewhere.** It should still be re-run on repos that use tsconfig `paths`, `src/` Python layouts, `#[path]` and Go multi-module workspaces. See [Threats to validity](#threats-to-validity).

## Method

### Repositories

Each repo is a shallow clone (`--depth 400`) under `~/work/e2e-agentdev-bench/`.

| Repo | Language | Pinned commit (HEAD) | Commit date | Tracked files |
|---|---|---|---|---|
| honojs/hono | TypeScript | `c91b60a251186fb024557c6d55a11d8e388f179c` | 2026-11-01 | 584 |
| Textualize/rich | Python | `9d8f9a372cc5916fd4781fec207ced7ddac2f08f` | 2026-06-23 | 553 |
| caddyserver/caddy | Go | `6bfeb4f8252f7054fcf1fbc11ec9b3846defc749` | 2026-11-01 | 746 |
| BurntSushi/ripgrep | Rust | `91f5d22ef48bde459d90a8c7dbeab416e97d0f2b` | 2026-10-27 | 226 |

### Ground truth

Ground truth is a set of file→file import edges inside each repo. The **universe** is the set of files the ground-truth tool knows about. Both tools are scored only on edges whose two endpoints are in the universe. Self-edges are dropped.

| Language | Toolchain resolver | Universe | GT edges |
|---|---|---|---|
| TypeScript | TypeScript 5.9.3 compiler API, script `gt_typescript.cjs`. `ts.preProcessFile` extracts specifiers: import/export-from, `import()`, `require`, triple-slash refs, and `declare module` augmentations. `ts.resolveModuleName` resolves them under the options of the tsconfig that owns each file. Shared `*base*` configs are used last. | 459 git-tracked TS/JS files | 1,036 |
| Python | CPython 3.11.5, script `gt_python.py`. `ast` extracts every `Import`/`ImportFrom` node, function bodies included. `importlib.machinery.PathFinder` resolves each one package by package, and `importlib.util.resolve_name` handles relative imports. Nothing is executed. The search path is the file's non-package basedir (pytest rootdir-prepend rule), then the repo root, then `src/`. | 213 `.py` files | 750 |
| Go | `go list -e -json ./...` with GOOS=linux GOARCH=amd64 gives packages and file lists. `go/parser` (ImportsOnly) gives each file's imports (`gogt`). A file importing package P depends on every non-test file of P. Cross-check: the per-file union matches go list's `Imports+TestImports+XTestImports` for all 50 packages, with 0 mismatches. | 379 files listed by go list | 7,521 |
| Rust | rust-analyzer is unavailable (the rustup proxy has no component). The resolver `gt_rust.py` follows the Rust Reference. `cargo metadata --no-deps` gives crates and targets (including `[[bin]] path`), renames and path dependencies. The module tree is built from `mod` declarations (inline modules included), and `use` trees resolve through `crate`/`self`/`super`/`::`/uniform paths/extern crates to the deepest existing module file. `mod x;` counts as an edge. | 101 files in the module trees | 252 |

Edge semantics are the same in every language: an import points at the **module file** the import names. A Python `from a.b import x` also adds `a.b.x` when `x` is a submodule. Implicit execution of a parent `__init__.py` is not counted. Go same-package references need no import, so they are not import edges.

### What each tool contributes

- **guard-impact** is driven through its public API by the bench binary `bench/impact-vs-codegraph/src/main.rs`. That binary is a standalone cargo project with a path dependency on `guard-impact`, outside the workspace.
  - `ImpactIndex::build` builds the graph, with a minimal `target.toml` that enables one language.
  - `imports_of` supplies edges.
  - `bounded_dependents` supplies the depth-3 affected set: a BFS over `importers_of`, unit-tested in `tests/bounded.rs`.
  - The unbounded `transitive_importers` set (the input to production `fan_out`) and `fan_out` latency are recorded alongside.
- **CodeGraph** is used through its SDK in read-only mode (`CodeGraph.open(repo, {readOnly: true})`).
  - `getFileDependencies` supplies edges. CodeGraph documents this as the file-level projection of its symbol graph and as the basis of `affected`.
  - A depth-3 BFS over `getFileDependents` supplies the affected set.
  - The `codegraph affected --depth 3 --json` CLI was also run per commit, for its own test selection and its latency.
  - Each clone was indexed with `codegraph init -y`, and timing runs used `codegraph index -q`. No other lifecycle command was run, and nothing ran in the guard repo.

### Metric 2 sampling

The sample is built by `sample_commits.py`:

- It walks `git log --no-merges` back from HEAD and skips the shallow boundary commit.
- For each commit, the changed files are intersected with the universe at HEAD.
- It keeps the 50 most recent commits where that intersection is non-empty and the ground-truth depth-3 dependent set is non-empty.
- Every commit is evaluated against the HEAD graph of all three sides.

| Repo | Commits considered | Commits kept | Mean changed files | Mean GT dependents |
|---|---|---|---|---|
| hono | 101 | 50 | 5.90 | 58.0 |
| rich | 96 | 50 | 1.64 | 147.2 |
| caddy | 54 | 50 | 2.82 | 81.1 |
| ripgrep | 117 | 50 | 1.78 | 13.7 |

### Timing

Timing is the median of 3 warm runs on one machine (`timing.sh`).

- guard-impact index time is measured two ways: the bench process wall time, and the in-process `ImpactIndex::build` time.
- guard-impact query time is the in-process depth-3 BFS.
- CodeGraph index time is the wall time of `codegraph index -q`.
- CodeGraph query time is measured two ways: the in-process SDK depth-3 BFS, and the wall time of the `codegraph affected` CLI process, which includes Node startup.

## Results

### Metric 1: import-edge precision / recall / F1

| Repo | Tool | TP | FP | FN | Precision | Recall | F1 |
|---|---|---|---|---|---|---|---|
| hono (TS) | guard-impact | 1031 | 0 | 5 | 1.000 | 0.995 | **0.998** |
| hono (TS) | CodeGraph | 872 | 962 | 164 | 0.475 | 0.842 | 0.608 |
| rich (Py) | guard-impact | 750 | 7 | 0 | 0.991 | 1.000 | **0.995** |
| rich (Py) | CodeGraph | 714 | 107 | 36 | 0.870 | 0.952 | 0.909 |
| caddy (Go) | guard-impact | 7521 | 0 | 0 | 1.000 | 1.000 | **1.000** |
| caddy (Go) | CodeGraph | 927 | 1373 | 6594 | 0.403 | 0.123 | 0.189 |
| ripgrep (Rust) | guard-impact | 219 | 0 | 33 | 1.000 | 0.869 | **0.930** |
| ripgrep (Rust) | CodeGraph | 175 | 504 | 77 | 0.258 | 0.694 | 0.376 |

### Metric 2: impact selection (depth-3 dependents, 50 commits per repo)

Precision and recall are means over commits. Micro values are pooled over all 50 commits.

| Repo | Tool | Recall | Precision | Micro recall | Micro precision | Commits with empty prediction |
|---|---|---|---|---|---|---|
| hono | guard-impact | **0.999** | 1.000 | 0.997 | 1.000 | 0 |
| hono | CodeGraph | 0.959 | 0.395 | 0.987 | 0.322 | 1 |
| rich | guard-impact | **1.000** | 1.000 | 1.000 | 1.000 | 0 |
| rich | CodeGraph | 0.985 | 0.864 | 0.984 | 0.877 | 0 |
| caddy | guard-impact | **1.000** | 1.000 | 1.000 | 1.000 | 0 |
| caddy | CodeGraph | 0.749 | 0.241 | 0.875 | 0.253 | 0 |
| ripgrep | guard-impact | **0.820** | 1.000 | 0.819 | 1.000 | 0 |
| ripgrep | CodeGraph | 0.772 | 0.154 | 0.778 | 0.151 | 8 |

Context only, not part of the decision: guard-impact's unbounded `transitive_importers` set, which is what production `fan_out` counts.

| Repo | Recall | Precision |
|---|---|---|
| hono | 1.000 | 0.924 |
| rich | 1.000 | 0.938 |
| caddy | 1.000 | 0.607 |
| ripgrep | 0.820 | 0.991 |

Context only, not part of the decision: test-file selection. The truth set is the ground-truth dependents that look like tests: `tests/`, `_test.go`, `.test.ts`, `test_*.py` and similar.

| Repo | guard-impact recall / precision | `codegraph affected` CLI recall / precision |
|---|---|---|
| hono (46 commits) | 0.999 / 1.000 | 0.952 / 0.473 |
| rich (50) | 1.000 / 1.000 | 0.000 / n/a (the CLI returned no tests) |
| caddy (50) | 1.000 / 1.000 | 0.000 / n/a (the CLI returned no tests) |
| ripgrep (38) | 1.000 / 1.000 | 0.719 / 0.484 |

On rich and caddy, the CLI's default test heuristic returned an empty list for every commit. Passing an explicit `--filter "tests/*.py"` does return tests, so this is a default-filter behavior on this setup, not a missing graph.

### Metric 3: wall time

| Repo | guard-impact index, process (in-process) | CodeGraph index | guard-impact query | CodeGraph SDK query | `codegraph affected` CLI |
|---|---|---|---|---|---|
| hono | 2,303 ms (2,160) | 8,291 ms | 20.5 µs | 12.6 ms | 579 ms |
| rich | 979 ms (865) | 4,372 ms | 315.5 µs | 22.6 ms | 613 ms |
| caddy | 1,252 ms (1,146) | 9,825 ms | 42.5 µs | 31.1 ms | 525 ms |
| ripgrep | 741 ms (659) | 6,322 ms | 20.0 µs | 21.9 ms | 614 ms |

First `codegraph init` runs, including setup: ripgrep 14.1 s, hono 13.4 s, rich 15.3 s, caddy 28.6 s. CodeGraph indexes far more than imports (symbols, calls, references, FTS), so its index time buys features that this benchmark does not score.

### Macro averages and decision

| Macro average (4 repos) | guard-impact | CodeGraph |
|---|---|---|
| Metric 1 F1 | **0.981** | 0.520 |
| Metric 2 recall (decision input) | **0.955** | 0.866 |
| Metric 2 precision | 1.000 | 0.414 |

guard-impact is lower on F1: **no**. guard-impact is lower on recall: **no**. Decision: **`guard_map_uses_guard_impact`**.

## Defects found in guard-impact

These are reported to the owner of `guard-impact`. None was fixed here. They were fixed afterwards; see [After fixes (2026-11-01)](#after-fixes-2026-11-01).

1. **Python stdlib shadowing (7 false-positive edges on rich).** `resolve_python` tries the importing file's ancestor directories as roots for *absolute* imports. As a result, `from abc import ABC` in `rich/console.py` resolves to `rich/abc.py`, and `import json` resolves to `rich/json.py`. That is Python 2 implicit-relative semantics. Absolute imports should search only the real roots: the basedir, the repo root and `src/`.
2. **Rust targets outside `src/` (33 missed edges; ripgrep impact recall 0.82).** The crate model assumes `src/lib.rs` or `src/main.rs`. ripgrep's binary is `[[bin]] path = "crates/core/main.rs"`, so every file under `crates/core/` is resolved as a standalone root, and the `crate::flags::…` uses are lost. The fix is to read `[lib]`/`[[bin]]`/`[[test]]` `path` from `Cargo.toml`.
3. **TypeScript module augmentation (5 missed edges on hono).** `declare module '../..' { … }` is not captured as a dependency on the augmented module.

## After fixes (2026-11-01)

The sections above are the original, pre-registered run and stay unchanged. This section re-runs guard-impact after the three defects were fixed. The decision rule, ground truth, clones (same pinned commits), change sets and CodeGraph outputs are the same files as before. Only guard-impact's outputs (`gi_*.json`) and the timings were regenerated.

### What changed in guard-impact

1. **Python search path.** Absolute imports resolve the way `importlib` sees them: the file's basedir first (the first ancestor directory without `__init__.py`), then the repo root, then `src/`, then the roots configured in `pyproject.toml` files. Configured roots come from pytest `pythonpath`, setuptools `package-dir` (`""`) and `packages.find.where`, poetry `packages[].from`, and the project directory of a nested `pyproject.toml`. The package directories around the importer are no longer roots, so `from abc import ABC` in `rich/console.py` stays the stdlib.
2. **Cargo targets and module trees.** Manifests are parsed as TOML.
   - Every target root is read from `Cargo.toml`: `[lib] path` and `name`, `[[bin]]`, `[[test]]`, `[[example]]` and `[[bench]]` `path`. Cargo's `src/` and `tests/`/`examples/`/`benches/` conventions apply, `auto*` switches are honoured, and `build.rs` (or `package.build`) is a root too.
   - Path dependencies are read, including renames and `workspace = true` entries that point to `[workspace.dependencies]`.
   - Each root builds its module tree from `mod` declarations. `#[path = "..."]` is honoured with the Rust Reference rules: relative to the file's directory, or to the inline module's directory inside `mod x { .. }`. A `#[path]` file owns its own directory.
   - `use` paths resolve through that tree. Files that no tree reaches fall back to their location, as before.
3. **TypeScript.**
   - `declare module '<relative>'` augmentations are captured as imports. Bare names are skipped, because a bare name may be an ambient declaration.
   - Non-relative specifiers resolve through the nearest `tsconfig.json` (else `jsconfig.json`). `compilerOptions.paths` is tried first: an exact pattern, else the longest wildcard prefix. `baseUrl` is tried second. `extends` chains are followed inside the repo (a string or an array). Configs are read as JSONC.
   - `paths` targets are relative to `baseUrl` when one is set, else to the config that declares `paths`.

One more gap surfaced during the re-run: `extern crate x as y;` was not captured. It is now an edge to the crate root of `x`, and later `y::...` paths resolve into `x`. That accounted for 6 of ripgrep's 33 missed edges (`crates/grep/src/lib.rs` re-exports six crates this way). The fix is covered by its own red-first test.

Each fix has a red-first test: `guard-impact/tests/layouts.rs` and the unit tests in `src/{extract,cargo,tsconfig,resolve}.rs`.

### Metric 1 after fixes

| Repo | TP | FP | FN | Precision | Recall | F1 (before → after) |
|---|---|---|---|---|---|---|
| hono (TS) | 1036 | 0 | 0 | 1.000 | 1.000 | 0.998 → **1.000** |
| rich (Py) | 750 | 0 | 0 | 1.000 | 1.000 | 0.995 → **1.000** |
| caddy (Go) | 7521 | 0 | 0 | 1.000 | 1.000 | 1.000 → **1.000** |
| ripgrep (Rust) | 252 | 0 | 0 | 1.000 | 1.000 | 0.930 → **1.000** |

CodeGraph's metric-1 numbers are unchanged (same `cg_edges_*.json`).

### Metric 2 after fixes (depth-3 dependents, same 50 commits per repo)

| Repo | Recall (before → after) | Precision | Micro recall / precision | Empty predictions |
|---|---|---|---|---|
| hono | 0.999 → **1.000** | 1.000 | 1.000 / 1.000 | 0 |
| rich | 1.000 → **1.000** | 1.000 | 1.000 / 1.000 | 0 |
| caddy | 1.000 → **1.000** | 1.000 | 1.000 / 1.000 | 0 |
| ripgrep | 0.820 → **1.000** | 1.000 | 1.000 / 1.000 | 0 |

Context only: the unbounded `transitive_importers` set now has recall / precision of 1.000 / 0.923 on hono, 1.000 / 0.938 on rich, 1.000 / 0.607 on caddy and 1.000 / 0.935 on ripgrep. Test-file selection is 1.000 / 1.000 on all four repos (46, 50, 50 and 38 commits).

### Macro averages and decision after fixes

| Macro average (4 repos) | guard-impact before | guard-impact after | CodeGraph |
|---|---|---|---|
| Metric 1 F1 | 0.981 | **1.000** | 0.520 |
| Metric 2 recall (decision input) | 0.955 | **1.000** | 0.866 |
| Metric 2 precision | 1.000 | 1.000 | 0.414 |

Without Go: F1 1.000 vs 0.631, recall 1.000 vs 0.905. Applying the same pre-registered rule, the decision is unchanged: **`guard_map_uses_guard_impact`**.

### Wall time after fixes

Medians of 3 warm runs with `timing.sh`, same machine. The script re-times both tools, so the CodeGraph index column was measured again in this run, on the same index and version. The CodeGraph query columns come from the unchanged `cg_*.json`.

| Repo | guard-impact index, process (in-process) | CodeGraph index | guard-impact query | CodeGraph SDK query | `codegraph affected` CLI |
|---|---|---|---|---|---|
| hono | 1,824 ms (1,714) | 8,654 ms | 32.5 µs | 12.6 ms | 579 ms |
| rich | 850 ms (709) | 4,077 ms | 298.0 µs | 22.6 ms | 613 ms |
| caddy | 1,796 ms (1,634) | 11,828 ms | 63.5 µs | 31.1 ms | 525 ms |
| ripgrep | 771 ms (679) | 7,205 ms | 21.5 µs | 21.9 ms | 614 ms |

The two-pass build (extract everything, then build Rust module trees, then resolve) did not measurably slow the index. Run-to-run noise on this machine is larger than any difference.

### Threats specific to the re-run

- **Perfect scores are agreement with the ground truth, not proof of correctness.** The Python fix adopts the same search-path rule `gt_python.py` uses (basedir, root, `src/`). The Rust fix builds module trees from Cargo targets, like `gt_rust.py`, which was written by the same author. Shared blind spots remain: macros, `cfg`, and item-level targets. The defects were also found on these same four repos, so the after-fixes numbers are in-sample.
- **Not exercised by any benchmark repo.** Configured Python roots, `#[path]`, `[lib] path`, renamed or workspace path dependencies, and tsconfig `paths`/`baseUrl`/`extends` are covered only by unit and fixture tests. None of the four repos uses them.
- Results files: `results/summary.json` holds the after-fixes run. `results/summary-before-fixes.json` and `results/timing-before-fixes.tsv` keep the original run. These summaries are committed; the root `.gitignore` has an exception for `bench/impact-vs-codegraph/results/`, while the large raw outputs stay ignored by the bench `.gitignore`.

## Threats to validity

- **The ground-truth definition is module-level import resolution, which is guard-impact's design point.** CodeGraph projects a symbol reference graph instead. Its "false positives" include real non-import dependencies, such as Go same-package calls and references reached through re-exports. Its "false negatives" include imports whose symbols are never referenced by a resolved edge. That penalty is largest in Go, where an import of package P makes the file depend on every file of P, which explains CodeGraph's metric-1 recall of 0.123 there. Metric 1 therefore measures agreement with import semantics, not the absolute quality of the graph. The decision survives dropping Go, as shown at the top.
- **Two ground truths are hand-written resolvers.**
  - Rust has no rust-analyzer. `gt_rust.py` was written by the same author, after reading guard-impact's resolver, so the two can share blind spots: no `#[path]` (ripgrep has none, checked), no macros, no `cfg` evaluation, and module-level targets for item paths.
  - Python uses CPython's real finders, but the edge semantics are a choice: leaf module only, plus submodules in `from X import y`.
  - TypeScript (compiler API) and Go (toolchain, cross-checked at 0/50 mismatches) are toolchain-grade.
- **guard-impact's precision of 1.0 on metric 2** follows from its edges being almost a subset of the ground truth: a reverse closure over a subset cannot leave the truth set.
- **One repo per language at mid size.** Path aliases (`tsconfig paths`), `src/` Python layouts, multi-module Go workspaces and `#[path]` are not exercised. guard-impact does not resolve TS `paths` aliases, and a repo that uses them would lower its TS recall.
- **Historical commits are scored on the HEAD graph.** Only changed files that still exist at HEAD count. The sampling requires a non-empty ground-truth dependent set.
- **Depth 3 is a benchmark bound.** Production guard-impact `fan_out` is unbounded; see the context table above.
- **Timing comes from a single Windows machine with a warm cache and 3 runs.** The disk was close to full during the run. CodeGraph's index does strictly more work. The CLI query time includes process startup.
- **Software versions:** CodeGraph v1.6.0, since v1.6.2 was available but not installed. No LLM or paid provider was involved anywhere.

## Reproduction

Everything lives in `bench/impact-vs-codegraph/`:

- `scripts/run_all.sh <clones-dir> <typescript-module-dir> <codegraph-package-dir>` runs the whole pipeline.
- `scripts/score.py` applies the rule and writes `results/summary.json`. The original run's summary and timings are kept as `results/summary-before-fixes.json` and `results/timing-before-fixes.tsv`.
- `results/` also holds `changesets_*.json` and `timing.tsv`. The large raw tool outputs are git-ignored.
- Unit tests: `cargo test` (bench crate) and `python -m unittest test_metrics` (run from `scripts/`).

## C# and PHP (2026-11-01)

This section extends the benchmark to the two newest guard-impact plugins, C# and PHP. Everything above stays as it was. The metrics, the commit sampling, the scoring code and both tool drivers are the same as before. What is new: one repository per language, a ground truth for each, and a decision rule per language.

**Decisions: `guard_impact_better_for_csharp` and `guard_impact_better_for_php`.**

| Language | Repo | Metric 1 F1, guard-impact vs CodeGraph | Metric 2 recall, guard-impact vs CodeGraph |
|---|---|---|---|
| C# | dotnet/eShop | **0.986** vs 0.587 | **0.846** vs 0.667 |
| PHP | symfony/console | **1.000** vs 0.917 | **1.000** vs 1.000 (tie) |

guard-impact scores higher on C#, but the pre-registered run found four real defects in the C# plugin (see [Defects found in the C# and PHP plugins](#defects-found-in-the-c-and-php-plugins)). The top-level `Program` defect (1) costs the most recall, and on the tests that matter most for an ASP.NET Core app: changing `Program.cs` misses its functional tests. The project-boundary defect (3) costs the next most, and defect 2 costs little. All four are fixed now; see [C# and PHP after fixes](#c-and-php-after-fixes). The tables above and below keep the pre-registered numbers. PHP agrees with its ground truth exactly. That agreement is only partly informative, because the ground truth follows the same prescribed semantics and the same autoload mapping. See [Threats to validity for C# and PHP](#threats-to-validity-for-c-and-php).

### Pre-registered rule (per language)

This rule was fixed before any C# or PHP tool output existed. It is implemented in `language_decision` (`bench/impact-vs-codegraph/scripts/metrics.py`) and unit-tested in `test_metrics.py`.

- For each language, compare guard-impact with CodeGraph on two numbers: the metric-1 F1 and the metric-2 recall (the mean per-commit recall of the depth-3 dependents, as in the original rule).
- If guard-impact is strictly lower on **either** number, the decision is `codegraph_better_for_<lang>`, and the guide must say `guard_map` should prefer CodeGraph for that language.
- Otherwise the decision is `guard_impact_better_for_<lang>`. A tie is not "lower".

### Repositories

Both repos are shallow clones (`--depth 400`) under `~/work/e2e-agentdev-bench/`.

| Repo | Language | Pinned commit (HEAD) | Commit date | Tracked files | Source files |
|---|---|---|---|---|---|
| dotnet/eShop | C# | `913acea66708a59595351174cee9b29f37179d29` | 2026-10-15 | 1,145 | 547 `.cs`, 28 `.csproj` |
| symfony/console | PHP | `e8f4b6c86efa6940af1cafe2e43f3dc3a94c340a` | 2026-11-01 | 615 | 390 `.php` |

Why these two:

- **eShop** is Microsoft's ASP.NET Core reference app. It has 28 SDK-style projects with `ProjectReference` chains, `GlobalUsings.cs` files, `<Compile Include>` links to `src/Shared/`, a .NET Aspire AppHost, MSTest and xUnit test projects, and two MAUI apps.
- **symfony/console** is a widely used Composer library. Its root PSR-4 map is `"Symfony\\Component\\Console\\": ""`, so `Tests/` is autoloaded too. It also has 67 global-namespace fixture classes under `Tests/Fixtures/` that no autoloader maps; tests load them with computed `require_once` paths.
- Neither clone has a `vendor/` directory, because nothing was installed.

### Ground truth

The edge semantics match each plugin's definition (guide sections "C#" and "PHP"). As before, both tools are scored only on edges whose endpoints are in the ground-truth universe, and self-edges are dropped.

**C#: Roslyn semantic model.** The tool is `bench/impact-vs-codegraph/gt_csharp/` (Microsoft.CodeAnalysis.CSharp `[5.6.0]`, published 2026-07-02). For every `.csproj` it builds one `CSharpCompilation` the way MSBuild would feed `csc`, without a restore:

- **Sources.** The default `**/*.cs` glob, minus `bin/`, `obj/` and nested project directories, plus unconditioned `<Compile Include>`, minus `<Compile Remove>`.
- **MAUI projects.** Only `Platforms/<target platform>/` is kept. `ClientApp` is compiled for `net10.0` (no platform folder) and `HybridApp` for `net10.0-android`.
- **Global usings.** The SDK's implicit usings when `ImplicitUsings` is enabled: Sdk, Web, Worker, MAUI and Aspire sets, read from the csproj and the `Directory.Build.props` chain. Unconditioned `<Using>` items are added too.
- **Preprocessor symbols.** Those of one target framework (`NET10_0`, `NET10_0_OR_GREATER`, …, plus `DEBUG`/`TRACE`).
- **References.** The `Microsoft.NETCore.App.Ref` and `Microsoft.AspNetCore.App.Ref` 10.0.12 reference assemblies of the installed SDK, plus transitive `ProjectReference`s as compilation references.

Edge rule:

- Every `SimpleNameSyntax` is visited. That covers identifiers, generic names and the parts of qualified names, in any position: types, `new`, attributes, `typeof`, `nameof`, static member access, and `using static`/alias directives.
- `GetSymbolInfo` is asked for the name's symbol, or for every candidate when it does not bind to one.
- A source named type, or the constructor of one (attributes), gives an edge to every file in its `DeclaringSyntaxReferences`.
- An implicit `var`, type parameters, extension-method calls, doc comments and inactive `#if` branches name nothing.
- Generated files (`*.g.cs`, `*.designer.cs`, files with an auto-generated header) are compiled, so names bind, but they are left out of the universe.

Result on eShop:

- 28 compilations, a universe of 518 files and 1,011 edges.
- The compilations report 2,783 errors. Most come from the un-restored NuGet packages: CS0246 ×1,427, CS0103 ×778, CS1061 ×169, CS0234 ×167. These do not touch repository types.
- Completeness check: the tool lists every unbound name that is spelled like a repository type. There are 9. All of them are non-repository types:
  - MAUI's `Location` and `PermissionStatus`. The repo's own types with those names live in namespaces the files do not import.
  - Razor's generated `App`.
  - The proto-generated `BasketItem`.

**PHP: PHP's own lexer.** The tool is `bench/impact-vs-codegraph/scripts/gt_php.php`, run with PHP 8.5.1.

- `PhpToken::tokenize` lexes each file. A structural pass over the significant tokens then implements PHP's compile-time name resolution: unbraced and braced namespaces, `use` imports (plain, aliased, grouped, mixed groups, `use function`, `use const`), `namespace\X`, and fully qualified names.
- Class names are taken from these positions: `new`, `extends`/`implements`, `instanceof`, `insteadof`, `catch`, trait `use`, `X::…`, attributes, and parameter/return/property/typed-constant types. A class `use` import also counts, because the import itself names the class.
- A fully qualified name maps to a file through every non-vendor `composer.json`: the `psr-4`, then `psr-0` maps of `autoload` and `autoload-dev`, longest prefix first. The exact-case file must exist.
- When no map gives a file, the name maps to every file that declares the class (case-insensitive).
- Static `require`/`include` paths (a literal, or `__DIR__`/`dirname(...)` joined with a literal) are edges.
- Result: a universe of 390 files and 1,719 edges.
- **Semantic gap.** Nothing is executed, and no autoloader runs. The include path and cwd, classmap-only autoloading, conditional declarations, container or facade wiring, and class names in strings are not observed. They are not edges for either side.

**Validation.** `scripts/test_gt.py` runs both tools on hand-checked fixtures (`bench/impact-vs-codegraph/fixtures/gt_csharp`, `fixtures/gt_php`) that cover every rule above. The tests were written first and seen failing. (Correction: in the pre-registered run the PHP fixture had no parameter attribute and no `new` in a parameter default, and the ground truth dropped both positions. See [C# and PHP after fixes](#c-and-php-after-fixes).)

**A ground-truth bug was found and fixed during the run.** The first PHP run disagreed with guard-impact on 2 edges (`Tester/ApplicationTester.php`/`CommandTester.php` → `Tester/TesterTrait.php`). The cause was in the ground truth:

- The `fn () =>` helpers captured the brace stack by value, so a trait `use` in a class body was read as a namespace import.
- The same bug counted methods as functions and dropped property types.
- The fixture was extended (red), the capture was fixed, and the whole PHP pipeline was re-run: ground truth, sample, both tools.
- The first run read guard-impact F1 0.999 (0 FN, 2 FP) against CodeGraph 0.917, with recall 1.000 for both. The decision was the same.
- Fixing a ground truth after seeing a disagreement can bias it towards the tool. Here both changed edges are trait uses that PHP itself resolves to that trait.

### Metric 2 sampling

The sampling is the same `sample_commits.py` as before: the 50 most recent non-merge commits whose changed files meet the universe and have a non-empty ground-truth depth-3 dependent set.

| Repo | Commits considered | Commits kept | Mean changed files | Mean GT dependents |
|---|---|---|---|---|
| eShop | 276 | 50 | 7.66 | 7.0 |
| symfony/console | 55 | 50 | 2.56 | 146.5 |

### Metric 1: edge precision / recall / F1

| Repo | Tool | TP | FP | FN | Precision | Recall | F1 |
|---|---|---|---|---|---|---|---|
| eShop (C#) | guard-impact | 998 | 16 | 13 | 0.984 | 0.987 | **0.986** |
| eShop (C#) | CodeGraph | 692 | 653 | 319 | 0.514 | 0.684 | 0.587 |
| symfony/console (PHP) | guard-impact | 1719 | 0 | 0 | 1.000 | 1.000 | **1.000** |
| symfony/console (PHP) | CodeGraph | 1679 | 262 | 40 | 0.865 | 0.977 | 0.917 |

### Metric 2: impact selection (depth-3 dependents, 50 commits per repo)

| Repo | Tool | Recall | Precision | Micro recall | Micro precision | Commits with empty prediction |
|---|---|---|---|---|---|---|
| eShop | guard-impact | **0.846** | 0.903 | 0.932 | 0.934 | 5 |
| eShop | CodeGraph | 0.667 | 0.127 | 0.704 | 0.111 | 4 |
| symfony/console | guard-impact | **1.000** | 1.000 | 1.000 | 1.000 | 0 |
| symfony/console | CodeGraph | 1.000 | 0.812 | 1.000 | 0.800 | 0 |

Context only, not part of the decision:

- **Unbounded `transitive_importers`.** Recall / precision is 0.846 / 0.886 on eShop and 1.000 / 0.661 on symfony/console.
- **Test-file selection.** The truth set is the ground-truth dependents under `tests/` or `Tests/`, or named `*Test(s).cs`/`*Test.php`.
  - guard-impact: 0.829 recall / 0.983 precision on eShop (34 commits), 1.000 / 1.000 on symfony/console (49 commits).
  - `codegraph affected` with its default filter returned no tests on either repo. With an explicit `--filter "Tests/**/*.php"` it does return tests on symfony/console, as with rich and caddy above.

### Metric 3: wall time

Medians of 3 warm runs with `timing.sh` (`results/timing-csharp-php.tsv`). The CodeGraph query columns come from `cg_*.json`.

| Repo | guard-impact index, process (in-process) | CodeGraph index | guard-impact query | CodeGraph SDK query | `codegraph affected` CLI |
|---|---|---|---|---|---|
| eShop | 1,101 ms (1,009) | 5,594 ms | 7.0 µs | 2.5 ms | 487 ms |
| symfony/console | 1,270 ms (1,149) | 6,590 ms | 184.5 µs | 14.8 ms | 497 ms |

The first `codegraph init -y` took 18.3 s on eShop (643 files indexed; 5,248 nodes, 8,456 edges) and 8.3 s on symfony/console (417 files; 6,782 nodes, 22,940 edges).

### Decisions

| Language | guard-impact lower on F1? | guard-impact lower on recall? | Decision |
|---|---|---|---|
| C# | no (0.986 vs 0.587) | no (0.846 vs 0.667) | **`guard_impact_better_for_csharp`** |
| PHP | no (1.000 vs 0.917) | no (1.000 vs 1.000, a tie) | **`guard_impact_better_for_php`** |

`guard_map` keeps guard-impact for both languages. Neither condition of the rule fired, so the guide carries no "prefer CodeGraph" note.

### Why the tools differ here

- **CodeGraph over-links by name across projects.** In eShop, a name shared by several projects links to all of them. `src/WebhookClient/Services/HooksRepository.cs` → `tests/ClientApp.UnitTests/Mocks/MockSettingsService.cs` and `src/HybridApp/MauiProgram.cs` → `tests/eShop.AppHost.UnitTests/AppHostConfigurationTests.cs` are examples. Its impact precision on eShop is 0.127.
- **CodeGraph misses type references that resolve to no call.** Generic arguments in DI registrations are the main case: `AddHttpClient<OrderingService>()` in `src/WebApp/Extensions/Extensions.cs`.
- **CodeGraph misses global-namespace PHP classes.** On symfony/console its 40 missed edges are mostly the global fixture classes (`new \FooCommand()` → `Tests/Fixtures/FooCommand.php`) and classes named in `new` lists (`ArgumentResolver.php` → `ValueResolver/*`).
- **Some of CodeGraph's PHP "false" edges are real non-name dependencies.** For example, `Command/Command.php` → `Formatter/OutputFormatter.php` is a method call on a returned object. The ground truth does not model those.

### Defects found in the C# and PHP plugins

These were reported to the owner of guard-impact and were **not fixed** in the pre-registered stage; they are fixed in [C# and PHP after fixes](#c-and-php-after-fixes). Counts are eShop metric-1 edges, which add up to all 16 FP and 13 FN. The `guard-impact/src/csharp.rs` line numbers below refer to the code before the fixes.

Metric-2 recall lost to each defect, recomputed from the pre-registered outputs (`gt_eshop.json`, `gi_eshop.json`, `changesets_eshop.json`). For each defect, its missed edges were added to guard-impact's edges and the per-commit depth-3 recall was recomputed. The table gives the gain summed over the 50 commits, out of 7.71 commit-units lost in total:

| Defect | Commits affected | Recall restored (commit-units) |
| --- | --- | --- |
| 1. top-level `Program` | 7 | 5.55 |
| 3. project boundaries (the AppHost `Extensions` FN) | 3: `ae71a061` (recall 0, empty prediction), `74f15b15` and `1c90ec9c` (0.5 each) | 2.00 |
| 2. implicit usings | 1: `0ac61017` | 0.17 |
| 4. alias shadowing | 0 (false edges only) | 0 |

1. **Top-level statements do not declare `Program` (7 FN).**
   - eShop's API `Program.cs` files use top-level statements. The compiler synthesizes `Program` in them and merges it with `public partial class Program` in `Program.Testing.cs`.
   - The C# query captures only explicit type declarations (`languages/csharp/imports.scm:21-26`), so `Program.cs` declares nothing.
   - `WebApplicationFactory<Program>` in the functional tests, `typeof(Program)` and `ILogger<Program>` then miss `Program.cs`.
   - This is the main recall loss (5.55 of the 7.71 commit-units). A change to `src/Catalog.API/Program.cs` does not select `tests/Catalog.FunctionalTests/*`. Five of the 50 commits get an empty prediction: four because of this defect, one because of defect 3 (`src/eShop.AppHost/Extensions.cs`).
2. **SDK implicit usings are ignored (5 FN).**
   - It is easy to assume that implicit usings name only BCL namespaces, which hold no repository types. eShop disproves it. (An earlier version of this paragraph attributed that claim to the guide in quotation marks. No version of the guide contains that text. At the time of this run, the guide already listed the ignored implicit usings as a known defect, citing these two eShop types.)
   - It declares `IEventBusBuilder` in `Microsoft.Extensions.DependencyInjection` (`src/EventBus/Abstractions/IEventBusBuilder.cs`) and `IDbSeeder<>` in `Microsoft.AspNetCore.Hosting` (`src/Shared/MigrateDbContextExtensions.cs`). Projects on the Web SDK name them through the implicit usings, without any `using`.
   - The `.csproj` and the SDK name are never read (`guard-impact/src/csharp.rs:304-315`).
3. **The repository-wide symbol table crosses project boundaries (14 FP, 1 FN).**
   - The full-name → files table ignores `ProjectReference` (`guard-impact/src/csharp.rs:260`). That is the whole cause. The lookup order (`:377-391`) searches the global namespace's types before the compilation-unit usings. This is C#'s order, and Roslyn binds the same way. The other projects' global `Program` and `Extensions` types are wrong only because they are not in the referencing compilation. The ground truth shows eShop.AppHost with 0 project references, so its `Extensions` binds to `eShop.AppHost.Extensions`. Searching the usings first would add new wrong edges: a file that declares a global-namespace type and imports a same-named type would bind to the import.
   - The global-namespace `partial class Program` of Catalog.API and of Ordering.API reaches both functional-test projects and other APIs: 9 FP.
   - `Extensions.IsFoundryEnabled` in `src/eShop.AppHost/AppHost.cs` resolves to four global-namespace `Extensions` classes of other projects instead of `eShop.AppHost.Extensions`: 4 FP and 1 FN. That one FN costs 2.00 commit-units of metric-2 recall: it empties the prediction whenever only `src/eShop.AppHost/Extensions.cs` changes.
   - WebApp's `using eShop.Basket.API.Grpc;` names a namespace that WebApp fills with generated gRPC client code. guard-impact links it to the Basket.API server class `BasketService`: 1 FP.
4. **A using alias does not shadow using-namespace imports (2 FP).**
   - `src/Ordering.API/Apis/OrdersApi.cs` has `using Order = eShop.Ordering.API.Application.Queries.Order;`, and its project has `global using` for the domain namespace.
   - C# binds `Order` to the alias. guard-impact unions both at the same scope (`guard-impact/src/csharp.rs:383-387`) and also links the domain's `Order.cs` and `CardType.cs`.

No PHP plugin defect was observed: 0 FP and 0 FN. Two plugin features were barely exercised:

- **Static includes:** one `__DIR__` include.
- **`use function`:** two imports, both of symfony/string, which is not in the repo.

### C# and PHP after fixes

The pre-registered numbers and decisions above are unchanged. This subsection re-runs **only guard-impact's side** after fixing the four C# defects and one more C# defect found in review. The ground truths (`gt_eshop.json`, `gt_symfony-console.json`), the commit samples and every CodeGraph output are the files of the pre-registered run.

**What changed in guard-impact** (C# only; the PHP plugin is unchanged). Each fix started from a failing test in `guard-impact/tests/layouts.rs`, named in parentheses:

1. **Top-level statements declare `Program`.** A new `@program` capture matches `global_statement` (`languages/csharp/imports.scm:31`) (`csharp_top_level_statements_declare_program`).
2. **MSBuild project model.** `.csproj`, `Directory.Build.props` and `Directory.Build.targets` are read with `roxmltree` 0.21.1. They give the SDK, `ImplicitUsings`, `UseMaui`, `<Using>` items, `ProjectReference` and `<Compile Include>` (`csharp_implicit_usings_follow_the_project_sdk`).
3. **Project scoping.** A type is visible only inside its project's `ProjectReference` closure. Files that `<Compile Include>` links into a project are part of it. A name with no visible hit is retried repository-wide (`csharp_project_references_scope_the_symbol_table`). The lookup order was not changed.
4. **Alias shadowing.** At each namespace level, a matching alias binds the name and the using namespaces of that level are skipped (`csharp_a_using_alias_shadows_using_namespace_imports`).
5. **Relative using targets** (found in review, not by the benchmark). `using Orders;` inside `namespace Shop` and `using Domain.Orders;` after `namespace Contoso.Api;` resolve from the enclosing namespace outward (`csharp_usings_inside_a_namespace_resolve_relative_to_it`). eShop has no such directive, so this fix does not move its numbers.

The guide's [C# section](../guard-impact.md#c) describes the new rules with line references.

**Metric 1 after fixes**

| Repo | Tool | TP | FP | FN | Precision | Recall | F1 |
|---|---|---|---|---|---|---|---|
| eShop (C#) | guard-impact | 1011 | 2 | 0 | 0.998 | 1.000 | **0.999** |
| eShop (C#) | CodeGraph (unchanged) | 692 | 653 | 319 | 0.514 | 0.684 | 0.587 |
| symfony/console (PHP) | guard-impact | 1719 | 0 | 0 | 1.000 | 1.000 | **1.000** |
| symfony/console (PHP) | CodeGraph (unchanged) | 1679 | 262 | 40 | 0.865 | 0.977 | 0.917 |

The two remaining false edges are `tests/Ordering.FunctionalTests/OrderingApiFixture.cs` and `OrderingApiTests.cs` → `src/Identity.API/Program.cs`. That test project references both `Ordering.API` and `Identity.API`. Only Ordering.API's `Program` is public (`public partial class Program`); Identity.API's synthesized `Program` is internal. Roslyn binds to the accessible type. guard-impact does not model accessibility, so it keeps both candidates.

**Metric 2 after fixes** (depth-3 dependents, same 50 commits per repo)

| Repo | Tool | Recall | Precision | Micro recall | Micro precision | Commits with empty prediction |
|---|---|---|---|---|---|---|
| eShop | guard-impact | **1.000** | 0.969 | 1.000 | 0.975 | 0 |
| eShop | CodeGraph (unchanged) | 0.667 | 0.127 | 0.704 | 0.111 | 4 |
| symfony/console | guard-impact | **1.000** | 1.000 | 1.000 | 1.000 | 0 |
| symfony/console | CodeGraph (unchanged) | 1.000 | 0.812 | 1.000 | 0.800 | 0 |

Context only: on eShop, the unbounded `transitive_importers` set has recall / precision 1.000 / 0.951. Test-file selection has 1.000 recall and 0.941 precision over 34 commits; the pre-registered run had 0.829 and 0.983. symfony/console is unchanged.

**Decisions after fixes:** still `guard_impact_better_for_csharp` and `guard_impact_better_for_php`. Neither condition of the rule fires.

**Wall time after fixes.** Medians of 3 warm runs of guard-impact only, same machine (`results/timing-csharp-php-after-fixes.tsv`). The CodeGraph rows of that file are copied from the pre-registered `timing-csharp-php.tsv`.

| Repo | guard-impact index, process (in-process) | guard-impact query |
|---|---|---|
| eShop | 889 ms (812) | 9.0 µs |
| symfony/console | 1,112 ms (994) | 133.0 µs |

- References that name no declared type (most BCL and package names) skip the lookup, so the extra C# work does not slow eShop's index.
- A first version without that pre-check took 1.2–1.6 s on eShop.
- Run-to-run noise on this machine is large. symfony/console's code path did not change, and its in-process median still moved from 1,149 to 994 ms.

**PHP ground-truth fix.** `gt_php.php` skipped the attributes and default values inside a function signature:

- A parameter attribute (`#[\App\Attr] $x`), and `new X` or `X::…` in a parameter default, are now class positions too. This matches the script's own header and guard-impact (`languages/php/imports.scm`).
- `fixtures/gt_php/src/Braced.php` gained `function h(#[\Acme\Attr\Marker] $a, $b = new \Acme\Service\Clock(), $c = \Acme\Model\Role::Admin)`, with its three edges in `expected.json`. `test_gt.py` failed on it before the fix and passes after.
- The fixed script was run on symfony/console into a scratch file. It gives the same universe (390 files) and the same 1,719 edges as the committed `gt_symfony-console.json`. Every edge these positions give on symfony/console already comes from another position, such as a `use` import. The committed ground truth was therefore left as it was, and the PHP scores do not move.

**Threats specific to this re-run**

- **In-sample.** All four C# defects were found on eShop, and the fixes were made after seeing its results. A perfect recall here is no evidence for other repositories.
- **Shared project model.** The plugin's implicit-using lists are copied from the C# ground truth's `GlobalUsings` (`gt_csharp/Program.cs`). Its build-file chain is the same rule (nearest `Directory.Build.props`, continued through `GetPathOfFileAbove`). Where the ground truth's MSBuild approximation is wrong, the plugin now shares the error. Name binding is still Roslyn's.
- **Not exercised by eShop:** relative using targets, `<Using Remove>`, `<Sdk Name>` elements, MAUI implicit usings on a repository type, and the repository-wide fallback on a missing `ProjectReference`. These are covered by unit and layout tests only.
- Results files: `results/summary-csharp-php-after-fixes.json` and `results/timing-csharp-php-after-fixes.tsv`. The pre-registered `summary-csharp-php.json` and `timing-csharp-php.tsv` are unchanged. The previous raw guard-impact outputs are kept as `results/gi_eshop-before-fixes.json` and `results/gi_symfony-console-before-fixes.json`, which are git-ignored like every `gi_*.json`.

### Threats to validity for C# and PHP

- **The ground truth shares the plugins' edge definitions by design.** The task prescribed them: "names a type", class `use` imports count as references, and PSR-4/PSR-0 first, then a declared-class table.
  - The PHP ground truth uses a different parser (the PHP lexer instead of tree-sitter). Its name-to-file mapping is the same design as the plugin's, so a perfect PHP score is agreement, not proof.
  - The C# ground truth is toolchain-grade for name binding (Roslyn). The project model is a hand-written approximation of MSBuild.
- **Roslyn without NuGet packages.** Unbound package types cannot create false repository edges. The reverse case can: a name that would bind to a package type might bind to a same-named repository type. The completeness check covers unbound names only.
- **Approximations in the project model.**
  - Conditioned properties and items are ignored.
  - XAML and Razor generated halves are absent.
  - Aspire-SDK test projects treat every `ProjectReference` as an assembly reference, including `Identity.API` without `IsAspireProjectResource="false"`. That reference yields no edge (checked).
  - MAUI apps are compiled for one target.
- **Out of sample for the plugins.** The plugins were built against fixtures. These two repos were never used during their development, and no plugin was changed before the pre-registered results were recorded. The C# fixes came after; their numbers are in-sample ([after fixes](#c-and-php-after-fixes)).
- **One mid-size repo per language.** eShop mixes web, MAUI, Aspire and tests. symfony/console is a single PSR-4 package with no vendor directory. Laravel-style apps (facades, container strings, `config/*.php`) are not covered, and neither tool sees those dependencies.
- **Historical commits are scored on the HEAD graph, depth 3, timing on one Windows machine with 3 warm runs**, as in the original run. CodeGraph v1.6.0 was used. No LLM or paid provider was involved.

### Reproduction (C# and PHP)

- `scripts/run_csharp_php.sh <clones-dir> <codegraph-package-dir>` runs the whole extension. Its steps:
  1. The fixture and metric tests.
  2. Both ground truths.
  3. The commit samples.
  4. The guard-impact bench binary (`config_for("csharp" | "php")`).
  5. The CodeGraph SDK export and query.
  6. Timings (`REPOS=... OUT=timing-csharp-php.tsv scripts/timing.sh`).
  7. Scoring (`scripts/score_csharp_php.py`, which reuses `score.score_repo`).
- After fixes: rebuild the bench binary, run step 4 again, time guard-impact only, then run `python score_csharp_php.py ../results timing-csharp-php-after-fixes.tsv summary-csharp-php-after-fixes.json` from `scripts/`.
- Committed results:
  - `results/summary-csharp-php.json` and `results/timing-csharp-php.tsv` (pre-registered)
  - `results/summary-csharp-php-after-fixes.json` and `results/timing-csharp-php-after-fixes.tsv`
  - `results/changesets_eshop.json`
  - `results/changesets_symfony-console.json`
- The raw `gt_*`, `gi_*` and `cg_*` outputs stay git-ignored.
- The refactor of `score.py` (`load_timing`, `score_repo`, `finish`) reproduces the committed `results/summary.json` exactly.
