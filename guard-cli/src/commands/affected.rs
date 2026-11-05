//! `guard affected` and the selection text shared with `guard test`.

use crate::context::Context;
use crate::error::{CliError, CliResult, EXIT_OK};
use crate::git::base::{self, BaseError, BaseSource, MergeBaseError};
use crate::git::changes::{self, Scope};
use crate::git::{self, GitError};
use crate::impact::{self, ImpactAnswer};
use guard_core::catalog::Catalog;
use guard_core::config::TargetConfig;
use guard_core::routing::{
    Reason, SMOKE_TAG, SelectedJourney, Selection, select_affected, select_affected_with_impact,
};
use guard_core::thresholds::Thresholds;
use guard_core::ticket::extract_ticket;
use guard_mcp::types::ImpactInfo;
use serde::Serialize;

pub struct Affected {
    /// The base candidate that resolved (cli.md "Base resolution").
    pub base: String,
    pub base_source: BaseSource,
    pub merge_base: String,
    pub branch: Option<String>,
    pub ticket: Option<String>,
    pub changed_files: Vec<String>,
    pub selection: Selection,
    /// The Phase 2 impact step (`impact` of the JSON output).
    pub impact: ImpactInfo,
    pub answer: ImpactAnswer,
}

#[derive(Serialize)]
struct AffectedJson<'a> {
    base: &'a str,
    base_source: BaseSource,
    merge_base: &'a str,
    branch: Option<&'a str>,
    ticket: Option<&'a str>,
    changed_files: &'a [String],
    tags: Vec<&'a String>,
    journeys: &'a [SelectedJourney],
    impact: &'a ImpactInfo,
}

fn git_error(error: GitError) -> CliError {
    CliError::env(format!("git failed: {error}"))
}

fn base_error(error: BaseError) -> CliError {
    match error {
        BaseError::Unknown { reference, hint } => {
            let error = CliError::usage(format!("unknown git ref: {reference}"));
            match hint {
                Some(base) => error.with_hint(format!("detected default base: {base}")),
                None => error,
            }
        }
        BaseError::NotFound {
            tried,
            shallow: false,
        } => CliError::usage(format!("no base branch found; tried: {}", tried.join(", ")))
            .with_hint("pass --base <ref> or set GUARD_BASE"),
        BaseError::NotFound {
            tried,
            shallow: true,
        } => CliError::usage(format!(
            "no base branch found in this shallow clone; tried: {}",
            tried.join(", ")
        ))
        .with_hint(
            "fetch full history (fetch-depth: 0 / GIT_DEPTH: 0), or pass --base <ref> or set GUARD_BASE",
        ),
        BaseError::Git(e) => git_error(e),
    }
}

fn merge_base_error(error: MergeBaseError, base: &str) -> CliError {
    match error {
        MergeBaseError::NoCommits => {
            CliError::usage("repository has no commits yet").with_hint("commit once, or use --all")
        }
        MergeBaseError::Shallow => CliError::usage(format!(
            "shallow clone: {base} is not reachable from HEAD"
        ))
        .with_hint("fetch full history (fetch-depth: 0 / GIT_DEPTH: 0) or pass --base <sha>"),
        MergeBaseError::Unrelated => CliError::usage(format!("{base} and HEAD share no history")),
        MergeBaseError::Git(e) => git_error(e),
    }
}

/// The `base_source` word of the text header.
fn source_word(source: BaseSource) -> &'static str {
    match source {
        BaseSource::Flag => "flag",
        BaseSource::Env => "env",
        BaseSource::Ci => "ci",
        BaseSource::RemoteHead => "remote_head",
        BaseSource::RemoteBranch => "remote_branch",
        BaseSource::LocalBranch => "local_branch",
    }
}

pub fn compute(
    ctx: &Context,
    config: &TargetConfig,
    catalog: &Catalog,
    thresholds: &Thresholds,
    base: Option<&str>,
) -> CliResult<Affected> {
    if git::version().is_none() {
        return Err(CliError::env("git is not on PATH"));
    }
    if !ctx.in_git {
        return Err(CliError::env(format!(
            "not a git repository: {}",
            ctx.root.display()
        )));
    }
    let pattern = config
        .ticket_regex()
        .map_err(|e| CliError::usage(e.to_string()))?;
    if !git::has_commits(&ctx.root) {
        return Err(
            CliError::usage("repository has no commits yet").with_hint("commit once, or use --all")
        );
    }
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let resolved = base::resolve_base(&ctx.root, base, &env, &mut |w| crate::error::warning(w))
        .map_err(base_error)?;
    let merge_base = base::merge_base_checked(&ctx.root, &resolved.sha)
        .map_err(|e| merge_base_error(e, &resolved.reference))?;
    // From the merge-base sha, so the file list and the impact diff share one base.
    let changed = changes::changed_paths(&ctx.root, &merge_base, Scope::CommittedAndWorktree)
        .map_err(git_error)?;
    git::print_warnings(&changed);
    let changed_files = changed.paths;
    let branch = git::current_branch(&ctx.root).map_err(git_error)?;
    let ticket = pattern.and_then(|re| extract_ticket(&re, branch.as_deref()));
    let (answer, source) = impact::ask(config, catalog, thresholds.impact_threshold, || {
        impact::diff_summary(&ctx.root, &merge_base)
    });
    let selection = match &answer {
        ImpactAnswer::NotConfigured => select_affected(catalog, &changed_files, ticket.as_deref()),
        ImpactAnswer::Answered { impact, threshold } => select_affected_with_impact(
            catalog,
            &changed_files,
            Some(impact),
            *threshold,
            ticket.as_deref(),
        ),
        ImpactAnswer::Unavailable { threshold, .. } => select_affected_with_impact(
            catalog,
            &changed_files,
            None,
            *threshold,
            ticket.as_deref(),
        ),
    }
    .map_err(|e| CliError::usage(e.to_string()))?;
    Ok(Affected {
        base: resolved.reference,
        base_source: resolved.source,
        merge_base,
        branch,
        ticket,
        changed_files,
        selection,
        impact: impact::info(&answer, source),
        answer,
    })
}

/// Header + tag lines, as printed by `guard affected` and `guard test --affected`.
pub fn text(affected: &Affected) -> String {
    let short: String = affected.merge_base.chars().take(7).collect();
    let mut out = format!(
        "base: {} (merge-base {short}; {})\nbranch: {}\nchanged files: {}\n",
        affected.base,
        source_word(affected.base_source),
        affected.branch.as_deref().unwrap_or("(detached HEAD)"),
        affected.changed_files.len()
    );
    out.push_str(&tag_lines(&affected.selection, affected.branch.as_deref()));
    match &affected.answer {
        ImpactAnswer::NotConfigured => {}
        ImpactAnswer::Answered { threshold, .. } => {
            let source = match affected.impact.source {
                guard_mcp::types::ImpactSource::Server => "server",
                _ => "local",
            };
            out.push_str(&format!(
                "impact: {source}, threshold {threshold}
"
            ));
        }
        ImpactAnswer::Unavailable { .. } => {
            out.push_str(
                "impact: engine unavailable, running every journey
",
            );
        }
    }
    out
}

/// One line per selected tag: `@smoke`, `@ticket:*`, then journey tags.
pub fn tag_lines(selection: &Selection, branch: Option<&str>) -> String {
    let mut rows: Vec<(String, String)> = Vec::new();
    if selection.tags.contains(SMOKE_TAG) {
        rows.push((SMOKE_TAG.to_string(), "smoke".to_string()));
    }
    for tag in selection.tags.iter().filter(|t| t.starts_with("@ticket:")) {
        rows.push((
            tag.clone(),
            format!("ticket from branch {}", branch.unwrap_or("-")),
        ));
    }
    for journey in selection
        .journeys
        .iter()
        .filter(|j| selection.tags.contains(&j.tag))
    {
        let reason = match journey.reasons.first() {
            Some(Reason::Path { file, glob }) => format!("path {file} matches {glob}"),
            Some(Reason::Smoke) => "smoke".to_string(),
            Some(Reason::All) => "all active journeys".to_string(),
            Some(Reason::Impact { probability }) => format!("impact {probability:.2}"),
            Some(Reason::EngineUnavailable) => "engine unavailable".to_string(),
            Some(Reason::Requested) | None => "requested".to_string(),
        };
        rows.push((journey.tag.clone(), reason));
    }
    let width = rows.iter().map(|(t, _)| t.len()).max().unwrap_or(0) + 3;
    rows.iter()
        .map(|(tag, reason)| format!("{tag:<width$}{reason}\n"))
        .collect()
}

pub fn run(
    ctx: &Context,
    base: Option<&str>,
    json: bool,
    thresholds: Option<&std::path::Path>,
) -> CliResult<u8> {
    let config = ctx.load_config()?;
    let catalog = ctx.load_catalog(&config)?;
    let thresholds = crate::settings::load_thresholds(ctx, thresholds)?;
    let affected = compute(ctx, &config, &catalog, &thresholds, base)?;
    if let ImpactAnswer::Unavailable { reason, .. } = &affected.answer {
        crate::error::warning(format!("impact: {reason}"));
    }
    if json {
        let body = AffectedJson {
            base: &affected.base,
            base_source: affected.base_source,
            merge_base: &affected.merge_base,
            branch: affected.branch.as_deref(),
            ticket: affected.ticket.as_deref(),
            changed_files: &affected.changed_files,
            tags: affected.selection.tags.iter().collect(),
            journeys: &affected.selection.journeys,
            impact: &affected.impact,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&body).expect("selection serialises")
        );
    } else {
        print!("{}", text(&affected));
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn no_base_in_a_shallow_clone_hints_at_full_history() {
        let tried = vec!["origin/main".to_string(), "main".to_string()];
        let deep = base_error(BaseError::NotFound {
            tried: tried.clone(),
            shallow: false,
        });
        assert_eq!(deep.hints, ["pass --base <ref> or set GUARD_BASE"]);
        let shallow = base_error(BaseError::NotFound {
            tried,
            shallow: true,
        });
        assert_eq!(
            shallow.messages,
            ["no base branch found in this shallow clone; tried: origin/main, main"]
        );
        assert_eq!(
            shallow.hints,
            [
                "fetch full history (fetch-depth: 0 / GIT_DEPTH: 0), or pass --base <ref> or set GUARD_BASE"
            ]
        );
    }

    #[test]
    fn tag_lines_align_reasons() {
        let selection = Selection {
            tags: BTreeSet::from([
                "@smoke".to_string(),
                "@journey:a.b".to_string(),
                "@ticket:X-1".to_string(),
            ]),
            journeys: vec![SelectedJourney {
                id: "a.b".into(),
                tag: "@journey:a.b".into(),
                reasons: vec![Reason::Path {
                    file: "f".into(),
                    glob: "g".into(),
                }],
            }],
        };
        assert_eq!(
            tag_lines(&selection, Some("X-1-y")),
            "@smoke         smoke\n@ticket:X-1    ticket from branch X-1-y\n@journey:a.b   path f matches g\n"
        );
    }

    #[test]
    fn smoke_only_journeys_have_no_line_of_their_own() {
        let selection = Selection {
            tags: BTreeSet::from(["@smoke".to_string()]),
            journeys: vec![SelectedJourney {
                id: "a.b".into(),
                tag: "@journey:a.b".into(),
                reasons: vec![Reason::Smoke],
            }],
        };
        assert_eq!(tag_lines(&selection, None), "@smoke   smoke\n");
    }
}
