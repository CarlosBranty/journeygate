# guard-gui

A native desktop window over the `guard` binary, written in pure Rust (eframe/egui, glow backend). No webview, no Node, no JS.

The GUI never re-implements guard. Every action runs the `guard` binary with the target repo as its working directory. The window only reads guard's files (`journeys.json`, `results/report.json`, `.guard/durations.json`) and guard's `--json` output.

## Build

```sh
cargo build -p guard-gui --release   # target/release/guard-gui(.exe)
cargo build -p guard-cli --release   # target/release/guard(.exe), if you need guard too
```

## Run

```sh
guard-gui --repo <path>     # open this project, e.g. D:\work\shop
guard-gui                   # the most recent project, else the current dir if it is a project
```

A project is a folder with a `target.toml`. When it opens, guard-gui checks that `target.toml` and `e2e-playwright/catalog/journeys.json` exist and shows what is missing in the status bar. Without `--repo`, guard-gui opens the most recent project that still exists, then the current directory, but only when that directory has a `target.toml`. Otherwise it shows a welcome screen, and it reads no files.

Switch projects with the **project selector** in the toolbar:

- It lists up to 10 recent projects, most recent first. Each project appears once, by its canonical path.
- **Open project...** opens the OS folder dialog. A folder without `target.toml` is refused.
- **Remove from recent** forgets a project. Hover a row to see its remove button.
- Picking the project that is already open does nothing.
- Switching drops everything from the old project: a run in progress is cancelled (you confirm first), and pending probe and branch results are dropped. Then guard-gui reloads the catalog and results.

The recent list and the layout persist between sessions through eframe's storage.

guard-gui looks for the guard binary in this order:

1. `GUARD_BIN`, if set. If it does not point at a file, the status bar says so; guard-gui does not fall back to another binary.
2. `PATH`.
3. The directory of `guard-gui` itself, since a release build puts both binaries in `target/release`.

A release build on Windows has no console window, so `--help` prints nothing there. Use a debug build to see it.

## Window

The look follows the JetBrains Rider New UI dark theme, with its islands layout. Test states use the impact-graph palette: green passed, red failed, amber flaky, blue affected, violet quarantined, grey skipped or not run.

**Main toolbar.** Shows the app mark, the project selector, the git branch (read-only, read in the background by `git rev-parse --abbrev-ref HEAD`), the green **Run affected** button, **Run all**, **Stop**, a services status dot (hover for each service, click to open the Run view) and the settings menu. Settings has **Recheck guard and E2E_* variables**, **Reload catalog and results** and the `guard up` timeout.

**Left stripe.** Switches the main view between Tests, Flow and Run. The icons at the bottom toggle the bottom tool window on its Run log or Problems tab.

**Bottom tool window.** **Run** holds the live log, with stdout and stderr merged. It keeps the last 5000 lines, plus **Cancel** and **Open HTML report**. **Problems** lists the failed and flaky journeys of the last report. Click one to show it in Tests.

**Status bar.** Shows the project path and anything missing, the journey count, and whether guard was found (hover to see the path). It also shows how many `E2E_*` variables are set (hover to see the names) and the last run result: **Last run** for a run of this session, else **Last report** with the outcome and age of the `report.json` already on disk. guard-gui never reads, shows, logs or stores a value, and it has no credential fields.

**Tests.**

- A tree of the journeys in `<repo>/e2e-playwright/catalog/journeys.json`, grouped by area. You can collapse each area. Each row shows the state icon, id, title, smoke and quarantined tags, and the last duration. A summary line and bar count every state.
- Search by id, title or area. A search opens every area that has a match.
- Check journeys, then use the green play button in the tree header (`guard test --journey <id>...`). **Run affected** (`guard test --affected`) and **Run all** (`guard test --all`) are in the toolbar.
- The target button runs **Preview affected** (`guard affected --json`, which runs no tests). Affected journeys are shown in blue.
- Click a row to open its details on the right: paths, last error, and attachments (screenshots, traces, videos). Each attachment has an **Open** button that opens the file in the OS default app. PNG screenshots are also shown inline.
- Last results come from `results/report.json`. Tests are mapped to journeys through their `@journey:<id>` tag. A journey with no result shows its historical duration from `.guard/durations.json`, prefixed with `~`.

**Run.**

- Services status from `guard status --json`, loaded when a project opens. Press refresh to load it again.
- **Start services** runs `guard up --timeout <n>` and **Stop services** runs `guard down`.
- The current or last run, with **Cancel** and **Open HTML report**. **Open HTML report** runs `guard report`, which keeps serving the report until you press Cancel.
- **Cancel** kills the whole process tree, including Playwright and its browsers. The run then shows as **Cancelled**, not as the exit code of the kill.

Only one run happens at a time. A probe (`guard status --json` or `guard affected --json`) is killed after 60 s, and the git branch read after 10 s. The timeout is reported.

**Flow.** A native port of `docs/flujo-e2e.html`: the E2E platform flow from Jira to the merge. The whole view is in Spanish, as in the HTML, live mode included; the rest of the app is in English.

- The 18 steps are cards on the HTML's three-lane grid, joined by curved wires with arrowheads. Infrastructure and context (skills, the model gateway, the orchestrator's side channel) are dashed. The legend explains the wire styles.
- The segmented control picks the journey: **Completo**, **Carril agente**, **Carril humano** or **Entrada directa**. **Simular regresión** and **Simular inestable** switch the outcome and the feedback loops.
- **Play** walks the journey step by step (2.3 s a step), with a comet along the wire into each step. **Pause**, **Step** and **Stop** control it, and the camera follows the current step without panning past the diagram's edges. Esc stops it.
- Click a card to open it in the inspector on the right: kicker, title, body, bullets, commands and the document sections it comes from. The rest of the diagram dims to that step's lineage. With nothing selected, the inspector describes the journey.
- Drag to pan and use the wheel to zoom. The fit button shows the whole diagram.
- **Live.** While guard-gui runs guard, the diagram lights the local test path: **guard up**, then the affected preview that selects the journeys, then the Playwright run, then the result (**Verde**, **Regresión**, **Inestable** or **Detenida**, read from the exit code and the report; a report the run did not rewrite counts no failure). The breadcrumb above the canvas shows the stage, and the inspector explains it. Hide it with its close button. A new run shows it again.
- The view repaints on its own only while the walkthrough plays or the camera glides. Idle, and during a live run, it repaints only on input and run output.

## Safety gate

Every test run passes a gate first:

| Class | From the GUI |
| --- | --- |
| `safe` | Runs without asking. |
| `contained` | Needs a confirmation dialog that names the journeys. |
| `unclassified` | Same confirmation as `contained`. |
| `dangerous` | Not runnable. The checkbox is disabled, and any run that would include the journey is refused. |

**Run all** checks every active journey. **Run affected** checks the journeys from the last **Preview affected**. Without a preview, it checks every active journey, because any of them may run.

The catalog contract (`docs/contracts/journeys-catalog.md`, `schemas/journeys.schema.json`) allows no extra keys, so `journeys.json` cannot carry a safety class. guard-gui does not change that format, so **every journey is unclassified today**, and every test run asks for confirmation.

## Layout

| Path | What it holds |
| --- | --- |
| `src/flow.rs` | The Flow diagram's data (ported from `docs/flujo-e2e.html`), grid layout, wire geometry, the walkthrough and the live mapping from a run to the diagram's nodes. |
| `src/catalog.rs` | Journey rows and the Tests tree. Uses `guard_core::catalog` for parsing and validation. |
| `src/projects.rs` | Project validation, the recent list, badge initials and colours, the project to open at launch, git branch parsing. |
| `src/theme.rs` | The Rider dark palette, sizes, system fonts and the test-state mapping. |
| `src/results.rs` | `report.json` mapped to journeys, plus duration history. |
| `src/commands.rs` | The exact guard argv for each action, as defined in `docs/contracts/cli.md`. |
| `src/safety.rs` | The gate. |
| `src/env_status.rs` | `E2E_*` names, never values. |
| `src/runner.rs` | Child process handling: streaming, cancel, `CREATE_NO_WINDOW`. |
| `src/guard_bin.rs`, `src/open.rs`, `src/status.rs`, `src/log_buffer.rs`, `src/args.rs` | Small helpers. |
| `src/app*.rs` | The thin egui layer: shell, views, painter-drawn icons, demo data for screenshots. |

All modules except `app` are unit-tested: `cargo test -p guard-gui`.

## Screenshots for review

A hidden flag renders a few frames, saves a PNG and exits:

```sh
guard-gui --screenshot shot.png [--view tests|run|flow] [--size 1600x1000] [--repo <path>]
```

`--flow` sets the Flow view's starting state, for example `--flow mode=agent,select=seal` or `--flow mode=human,step=6,sim=reg` or `--flow live=red`. Keys: `mode` (`full`, `agent`, `human`, `direct`), `select` (a node id), `step` (a paused walkthrough step), `sim` (`reg`, `flaky`, `both`) and `live` (`up`, `select`, `test`, `green`, `red`, `flaky`, `stopped`). Flow shots leave the bottom tool window closed.

Without `--repo` it shows built-in demo data. With `--repo` it reads that project's files and git branch, but it runs no guard command. It never touches the saved recent list or layout. Fonts come from the system: Segoe UI and JetBrains Mono on Windows when they are installed, otherwise egui's defaults.
