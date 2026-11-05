//! Command-line surface (clap). Usage errors exit with code 2.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "guard",
    version,
    about = "Runs the E2E journey suite locally and in CI."
)]
pub struct Cli {
    /// Path to target.toml (default: <repo root>/target.toml).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Playwright project dir (default: <repo root>/e2e-playwright).
    #[arg(long = "e2e-dir", global = true, value_name = "PATH")]
    pub e2e_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Detect how the app runs, then write target.toml and the Playwright scaffold.
    Init(InitArgs),
    /// Validate configuration and environment.
    Doctor,
    /// Start the app with [app].start and wait until it is ready.
    Up {
        /// Seconds to wait for readiness.
        #[arg(long, default_value_t = 120, value_name = "SECONDS")]
        timeout: u64,
    },
    /// Stop the app started by guard up.
    Down,
    /// Show [app] and every [[services]] entry: PID, liveness, readiness, log.
    Status {
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// Report every integration of the flow instead (Jira, guard serve,
        /// triage, agent queue, model gateway, code host, CI): read-only,
        /// each probe at most 2 s, all in parallel.
        #[arg(long)]
        integrations: bool,
    },
    /// Connect guard to Jira, GitHub or GitLab (credentials stored per user).
    Connect(ConnectArgs),
    /// Create something new (a journey).
    New {
        #[command(subcommand)]
        what: NewCommand,
    },
    /// Run Playwright tests.
    Test(TestArgs),
    /// Print the --affected selection without running anything.
    Affected {
        /// Base ref for the diff [default: detected: GUARD_BASE, CI, the
        /// remote default branch, then main/master/trunk].
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// thresholds.toml (default: GUARD_THRESHOLDS, else <repo root>/thresholds.toml).
        #[arg(long, value_name = "PATH")]
        thresholds: Option<PathBuf>,
    },
    /// CI gate: plan, shard, finalize.
    Gate {
        #[command(subcommand)]
        step: GateCommand,
    },
    /// Open the HTML report of the last run.
    Report,
    /// Explain the last run's failing and flaky journeys with Claude.
    Explain(ExplainArgs),
    /// List the commits most likely related to each failing or flaky journey.
    Suspects(SuspectsArgs),
    /// Service mode: webhooks, triage, impact API, gate results, dashboard.
    Serve(ServeArgs),
    /// Model Context Protocol server over stdio for coding assistants.
    Mcp,
    /// Reproduce a remote guard failure locally.
    Repro(ReproArgs),
    /// The agent lane: agents.toml, skills sync, ticket runs and their status.
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
    /// Orchestrator memory (read-only views and exports).
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ConnectArgs {
    /// Print every known credential, where it comes from, secrets masked.
    #[arg(long)]
    pub list: bool,
    #[command(subcommand)]
    pub service: Option<ConnectService>,
}

#[derive(Debug, Subcommand)]
pub enum ConnectService {
    /// Jira Cloud: base URL, email and API token; writes [tracker].
    Jira {
        /// Jira site, e.g. https://acme.atlassian.net (default: JIRA_BASE_URL).
        #[arg(long = "base-url", value_name = "URL")]
        base_url: Option<String>,
        /// Atlassian account email (default: JIRA_EMAIL).
        #[arg(long, value_name = "EMAIL")]
        email: Option<String>,
        /// [tracker].project key, e.g. SHOP.
        #[arg(long, value_name = "KEY")]
        project: Option<String>,
        /// Read the API token from the first line of stdin.
        #[arg(long = "token-stdin")]
        token_stdin: bool,
    },
    /// GitHub token; writes [code_host] kind = "github".
    Github(ConnectHostArgs),
    /// GitLab token; writes [code_host] kind = "gitlab".
    Gitlab(ConnectHostArgs),
}

#[derive(Debug, Args)]
pub struct ConnectHostArgs {
    /// API base URL (default: GITHUB_API_URL / GITLAB_API_URL, else the public host).
    #[arg(long = "api-url", value_name = "URL")]
    pub api_url: Option<String>,
    /// Read the token from the first line of stdin.
    #[arg(long = "token-stdin")]
    pub token_stdin: bool,
}

#[derive(Debug, Subcommand)]
pub enum NewCommand {
    /// Add a journey to the catalog and create its spec skeleton.
    Journey(NewJourneyArgs),
}

#[derive(Debug, Args)]
pub struct NewJourneyArgs {
    /// Journey id, e.g. checkout.customer-card.
    #[arg(value_name = "ID")]
    pub id: String,
    /// `[areas]` key (default: the first segment of the id).
    #[arg(long, value_name = "KEY")]
    pub area: Option<String>,
    /// Human-readable title (default: derived from the id).
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,
    /// Repo-relative glob that selects the journey (repeatable).
    #[arg(long = "path", value_name = "GLOB")]
    pub paths: Vec<String>,
    /// Mark it as a smoke journey (@smoke).
    #[arg(long)]
    pub smoke: bool,
    /// Then run `npx playwright codegen <base_url>` in the Playwright dir.
    #[arg(long)]
    pub record: bool,
}

#[derive(Debug, Subcommand)]
pub enum AgentsCommand {
    /// Validate agents.toml (exit 2 on error).
    Check,
    /// Render agents/skills and AGENTS.md per runtime, plus guard mcp registrations.
    Sync {
        /// Write nothing; exit 1 when the rendered files are out of date.
        #[arg(long)]
        check: bool,
        /// Runtimes to render (repeatable; default: the runtimes agents.toml uses).
        #[arg(long = "runtime", value_name = "NAME")]
        runtimes: Vec<String>,
    },
    /// Enqueue (ticket, qa) and run the ticket through QA and Coder to its PR branch.
    Run(AgentsRunArgs),
    /// The 4-arm token A/B report of an experiment (agent_runs + spend_records).
    AbReport {
        /// Experiment id (the first segment of the trials' key aliases).
        #[arg(long, value_name = "ID")]
        experiment: String,
        /// Also write the Markdown report here.
        #[arg(long, value_name = "FILE")]
        out: Option<PathBuf>,
    },
    /// Queue tasks and agent runs of this repo.
    Status {
        /// Only this ticket.
        #[arg(long, value_name = "KEY")]
        ticket: Option<String>,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
pub struct AgentsRunArgs {
    /// Ticket key, e.g. SHOP-42.
    #[arg(value_name = "TICKET")]
    pub ticket: String,
    /// Run every agent on this runtime (claude-code, codex, copilot-cli,
    /// mini-swe-agent, openhands, fake).
    #[arg(long, value_name = "NAME")]
    pub runtime: Option<String>,
    /// Scripted fake runtime (plans from GUARD_AGENT_FAKE_PLANS); never calls a model.
    #[arg(long)]
    pub fake: bool,
    /// The ticket as JSON (key, summary, description, labels, ...) instead of the tracker.
    #[arg(long, value_name = "PATH")]
    pub ticket_file: Option<PathBuf>,
    /// The `[areas]` key the Coder may write (default: the ticket's `area:<key>` labels).
    #[arg(long, value_name = "KEY")]
    pub area: Vec<String>,
    /// Only enqueue; `guard serve` workers run it.
    #[arg(long)]
    pub enqueue_only: bool,
    /// Give up after this many seconds.
    #[arg(long, default_value_t = 3600, value_name = "SECONDS")]
    pub timeout: u64,
    /// Print the final report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Export the repo's memory facts.
    Export {
        /// Engram import file (docs/contracts/memory.md, "Engram export").
        #[arg(long, value_name = "FILE")]
        engram: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Bind address (default: GUARD_LISTEN, else 0.0.0.0:8080).
    #[arg(long, value_name = "ADDR")]
    pub listen: Option<String>,
    /// Checkout of the app repo (default: the current directory).
    #[arg(long, value_name = "PATH")]
    pub repo: Option<PathBuf>,
    /// thresholds.toml (default: GUARD_THRESHOLDS, else <repo>/thresholds.toml).
    #[arg(long, value_name = "PATH")]
    pub thresholds: Option<PathBuf>,
    /// Fake mode: GUARD_JEV_FAKE=1, GUARD_JIRA_FAKE=1 and DATABASE_URL=memory://
    /// unless they are set. Never use in production.
    #[arg(long)]
    pub fake: bool,
}

#[derive(Debug, Args)]
pub struct ReproArgs {
    /// `<project>::<file>::<title>` of the remote failure.
    #[arg(value_name = "TEST_ID")]
    pub test_id: String,
    /// PR number / MR IID whose latest gate result holds the failure.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub pr: u64,
    /// Writes the result as JSON (used by guard mcp).
    #[arg(long, value_name = "PATH", hide = true)]
    pub result_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SuspectsArgs {
    /// Only this journey (default: every failing or flaky journey).
    #[arg(long, value_name = "ID")]
    pub journey: Option<String>,
    /// Playwright JSON report to read (default: <e2e dir>/results/report.json).
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
    /// Commits listed per journey.
    #[arg(long, value_name = "N", default_value_t = 5)]
    pub limit: usize,
    /// Print JSON: {base, journeys, warnings}.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(after_help = "Environment:\n  \
    GUARD_EXPLAIN_API_KEY   API key (falls back to ANTHROPIC_API_KEY)\n  \
    GUARD_EXPLAIN_BASE_URL  Anthropic-compatible API base (default: https://api.anthropic.com)\n  \
    GUARD_EXPLAIN_MODEL     model id (default: claude-haiku-5-5)\n  \
    GUARD_EXPLAIN_LANG      summary language code (default: es)\n\n\
    --list-models picks the endpoint from the base URL's host: GET /v1/models\n\
    (Anthropic), GET /models (DeepSeek, OpenAI format) or GET /api/tags (Ollama),\n\
    trying the others on the same host when the first is missing.")]
pub struct ExplainArgs {
    /// Only this journey (default: every failing or flaky journey).
    #[arg(long, value_name = "ID")]
    pub journey: Option<String>,
    /// Playwright JSON report to read (default: <e2e dir>/results/report.json).
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
    /// Model id (default: GUARD_EXPLAIN_MODEL, else claude-haiku-5-5).
    #[arg(long, value_name = "ID")]
    pub model: Option<String>,
    /// Language of the summary: es (Spanish), en (English) or any other code
    /// (default: GUARD_EXPLAIN_LANG, else es). Journey ids, paths, identifiers
    /// and error text stay verbatim.
    #[arg(long, value_name = "CODE")]
    pub lang: Option<String>,
    /// List the models the configured provider offers (id, then display
    /// name) instead of explaining; with --json, an array of {id, name?}.
    /// Same base URL and key as explain; needs no report.
    #[arg(long, conflicts_with_all = ["journey", "report", "model", "lang"])]
    pub list_models: bool,
    /// Print JSON: {model, rows, verdict, summary, journeys}. Alias of
    /// --format json.
    #[arg(long, conflicts_with = "format")]
    pub json: bool,
    /// Output format: table (box-drawing table, the default), md (GitHub
    /// Markdown), json, or xlsx (Excel workbook; needs --out).
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: Option<ExplainFormat>,
    /// Write the rendered output to PATH instead of stdout (required for
    /// xlsx); prints "wrote <PATH>" to stderr.
    #[arg(long, value_name = "PATH", conflicts_with = "list_models")]
    pub out: Option<PathBuf>,
}

impl ExplainArgs {
    /// The effective format: --format, else json with --json, else table.
    pub fn effective_format(&self) -> ExplainFormat {
        match (self.format, self.json) {
            (Some(format), _) => format,
            (None, true) => ExplainFormat::Json,
            (None, false) => ExplainFormat::Table,
        }
    }
}

/// `guard explain --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ExplainFormat {
    Table,
    Md,
    Json,
    Xlsx,
}

/// `uses:` of the workflow `guard init --ci github` writes.
pub const DEFAULT_ACTION_REF: &str = "CarlosBranty/journeygate/ci/github-action@main";

#[derive(Debug, Args)]
pub struct InitArgs {
    /// Accept every detected value without asking (implied when stdin is not a TTY).
    #[arg(long)]
    pub yes: bool,
    /// Print the plan and the target.toml that would be written; write nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Overwrite target.toml, the hook and the CI file if they exist.
    #[arg(long)]
    pub force: bool,
    /// Who starts the app locally: self (you; guard only needs the URLs) or guard.
    #[arg(long, value_enum, value_name = "WHO")]
    pub runner: Option<RunnerMode>,
    /// Also register `guard mcp` for these runtimes (claude-code, codex, copilot-cli).
    #[arg(long, value_delimiter = ',', value_name = "RUNTIME,...")]
    pub mcp: Vec<String>,
    /// Write .git/hooks/pre-push running `guard test`.
    #[arg(long)]
    pub hook: bool,
    /// Write the CI pipeline for this code host.
    #[arg(long, value_enum, value_name = "HOST")]
    pub ci: Option<CiHost>,
    /// `uses:` value for --ci github.
    #[arg(long, default_value = DEFAULT_ACTION_REF, value_name = "REF")]
    pub action_ref: String,
    /// Run npm install and npx playwright install chromium in the Playwright dir.
    #[arg(long)]
    pub install: bool,
}

/// Who starts the app locally (cli.md "Runner").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum RunnerMode {
    /// The developer starts it; guard only needs the URLs.
    #[value(name = "self")]
    SelfRun,
    /// guard starts it with `[app].start` and `[[services]]`.
    #[default]
    Guard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CiHost {
    Github,
    Gitlab,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// Smoke + ticket + journeys whose paths match the diff (default).
    #[arg(long, conflicts_with_all = ["all", "journey"])]
    pub affected: bool,
    /// Every active journey.
    #[arg(long, conflicts_with = "journey")]
    pub all: bool,
    /// Exactly these journeys (repeatable); quarantined allowed.
    #[arg(long, value_name = "ID")]
    pub journey: Vec<String>,
    /// Base for --affected [default: detected, see `guard affected --help`].
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,
    /// Show the browser.
    #[arg(long, conflicts_with_all = ["ui", "debug"])]
    pub headed: bool,
    /// Playwright UI mode.
    #[arg(long, conflicts_with = "debug")]
    pub ui: bool,
    /// Playwright Inspector.
    #[arg(long)]
    pub debug: bool,
    /// Run inside the official Playwright image.
    #[arg(long, conflicts_with_all = ["ui", "debug", "headed"])]
    pub docker: bool,
    /// Count-based Playwright shard, e.g. 1/3.
    #[arg(long, value_name = "I/N", value_parser = parse_shard)]
    pub shard: Option<ShardSpec>,
    /// thresholds.toml (default: GUARD_THRESHOLDS, else <repo root>/thresholds.toml).
    #[arg(long, value_name = "PATH")]
    pub thresholds: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShardSpec {
    pub index: u32,
    pub total: u32,
}

pub fn parse_shard(value: &str) -> Result<ShardSpec, String> {
    let (i, n) = value.split_once('/').ok_or("expected <i>/<n>, e.g. 1/3")?;
    let index: u32 = i
        .trim()
        .parse()
        .map_err(|_| format!("invalid shard index {i:?}"))?;
    let total: u32 = n
        .trim()
        .parse()
        .map_err(|_| format!("invalid shard count {n:?}"))?;
    if index < 1 || index > total {
        return Err(format!(
            "shard must satisfy 1 <= i <= n, got {index}/{total}"
        ));
    }
    Ok(ShardSpec { index, total })
}

#[derive(Debug, Subcommand)]
pub enum GateCommand {
    /// Write one duration-balanced plan per shard.
    Plan {
        #[arg(long, value_name = "N")]
        shards: usize,
        #[arg(long, value_name = "DIR")]
        out: PathBuf,
    },
    /// Run one shard with the blob reporter.
    Shard {
        #[arg(long, value_name = "I")]
        index: usize,
        #[arg(long, value_name = "DIR")]
        plan: PathBuf,
    },
    /// Merge blob reports, run the baseline and decide the verdict.
    Finalize(FinalizeArgs),
}

#[derive(Debug, Args)]
pub struct FinalizeArgs {
    #[arg(long, value_name = "DIR")]
    pub blobs: PathBuf,
    /// Base commit for the baseline run (overrides discovery).
    #[arg(long, value_name = "SHA")]
    pub base: Option<String>,
    /// PR number / MR IID (overrides discovery).
    #[arg(long, value_name = "N")]
    pub pr: Option<u64>,
    /// Skip the baseline run: every 3/3 failure blocks.
    #[arg(long)]
    pub no_baseline: bool,
    /// Skip Jira, PR comment and status publishing.
    #[arg(long)]
    pub no_report: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn shard_spec_parsing() {
        assert_eq!(parse_shard("1/3"), Ok(ShardSpec { index: 1, total: 3 }));
        assert_eq!(parse_shard("3/3"), Ok(ShardSpec { index: 3, total: 3 }));
        assert!(parse_shard("0/3").is_err());
        assert!(parse_shard("4/3").is_err());
        assert!(parse_shard("x").is_err());
        assert!(parse_shard("1/x").is_err());
    }

    #[test]
    fn journey_flag_is_repeatable() {
        let cli =
            Cli::try_parse_from(["guard", "test", "--journey", "a.b", "--journey", "c.d"]).unwrap();
        let Command::Test(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.journey, vec!["a.b", "c.d"]);
    }

    #[test]
    fn phase_two_subcommands_parse() {
        let cli = Cli::try_parse_from(["guard", "repro", "chromium::f::t", "--pr", "12"]).unwrap();
        let Command::Repro(args) = cli.command else {
            panic!()
        };
        assert_eq!((args.test_id.as_str(), args.pr), ("chromium::f::t", 12));
        assert!(Cli::try_parse_from(["guard", "repro", "x", "--pr", "0"]).is_err());
        assert!(Cli::try_parse_from(["guard", "repro", "x"]).is_err());
        let cli = Cli::try_parse_from([
            "guard",
            "gate",
            "finalize",
            "--blobs",
            "b",
            "--base",
            "abc",
            "--pr",
            "3",
            "--no-baseline",
            "--no-report",
        ])
        .unwrap();
        let Command::Gate {
            step: GateCommand::Finalize(f),
        } = cli.command
        else {
            panic!()
        };
        assert_eq!(f.base.as_deref(), Some("abc"));
        assert_eq!(f.pr, Some(3));
        assert!(f.no_baseline && f.no_report);
        assert!(Cli::try_parse_from(["guard", "agents", "sync", "--check"]).is_ok());
        assert!(Cli::try_parse_from(["guard", "agents", "sync", "--x"]).is_err());
        let cli = Cli::try_parse_from([
            "guard",
            "agents",
            "run",
            "SHOP-42",
            "--fake",
            "--runtime",
            "fake",
            "--area",
            "catalog",
        ])
        .unwrap();
        let Command::Agents {
            command: AgentsCommand::Run(run),
        } = cli.command
        else {
            panic!()
        };
        assert_eq!(
            (run.ticket.as_str(), run.fake, run.area.len()),
            ("SHOP-42", true, 1)
        );
        assert!(Cli::try_parse_from(["guard", "agents", "status", "--json"]).is_ok());
        assert!(Cli::try_parse_from(["guard", "agents", "check"]).is_ok());
        assert!(
            Cli::try_parse_from(["guard", "agents", "ab-report", "--experiment", "e1"]).is_ok()
        );
        assert!(Cli::try_parse_from(["guard", "memory", "export", "--engram", "f.json"]).is_ok());
        assert!(
            Cli::try_parse_from(["guard", "serve", "--fake", "--listen", "127.0.0.1:0"]).is_ok()
        );
        assert!(Cli::try_parse_from(["guard", "mcp"]).is_ok());
    }

    #[test]
    fn new_journey_and_status_parse() {
        let cli = Cli::try_parse_from([
            "guard", "new", "journey", "a.b", "--path", "x/**", "--path", "y/**", "--smoke",
            "--record",
        ])
        .unwrap();
        let Command::New {
            what: NewCommand::Journey(args),
        } = cli.command
        else {
            panic!()
        };
        assert_eq!(args.paths, vec!["x/**", "y/**"]);
        assert!(args.smoke && args.record && args.area.is_none());
        assert!(matches!(
            Cli::try_parse_from(["guard", "status", "--json"])
                .unwrap()
                .command,
            Command::Status {
                json: true,
                integrations: false
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["guard", "status", "--integrations", "--json"])
                .unwrap()
                .command,
            Command::Status {
                json: true,
                integrations: true
            }
        ));
        assert!(Cli::try_parse_from(["guard", "new", "journey"]).is_err());
    }

    #[test]
    fn conflicting_flags_are_rejected() {
        for args in [
            vec!["guard", "test", "--all", "--affected"],
            vec!["guard", "test", "--headed", "--debug"],
            vec!["guard", "test", "--docker", "--ui"],
        ] {
            assert!(Cli::try_parse_from(&args).is_err(), "{args:?}");
        }
    }
}
