//! Builds the [`MapResponse`]: seeds, ranking and the hard token budget.

use crate::rank::personalized_pagerank;
use crate::{GraphSource, MapEntry, MapError, MapRequest, MapResponse, Symbol, estimate_tokens};
use std::collections::{BTreeMap, BTreeSet};

/// Seed weight of a `focus_files` entry and of a file defining a `mentioned` identifier.
pub const FOCUS_WEIGHT: f64 = 1.0;
pub const MENTION_WEIGHT: f64 = 0.5;
/// Definitions shown per file.
pub const MAX_SYMBOLS_PER_FILE: usize = 8;
/// Ranks are rounded to this many decimals (saves tokens, stays ordered).
const RANK_DECIMALS: f64 = 1e6;

/// Runs `guard_map` against `source`.
pub fn build_map(source: &dyn GraphSource, request: &MapRequest) -> Result<MapResponse, MapError> {
    request.validate()?;
    let files = source.files()?;
    let known: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let edges = source.edges()?;

    let mut seeds: BTreeMap<String, f64> = BTreeMap::new();
    for f in &request.focus_files {
        let f = normalize(f);
        if known.contains(f.as_str()) {
            *seeds.entry(f).or_default() += FOCUS_WEIGHT;
        }
    }
    if !request.mentioned.is_empty() {
        let wanted: BTreeSet<&str> = request.mentioned.iter().map(String::as_str).collect();
        for f in &files {
            if source
                .symbols(f)?
                .iter()
                .any(|s| wanted.contains(s.name.as_str()))
            {
                *seeds.entry(f.clone()).or_default() += MENTION_WEIGHT;
            }
        }
    }

    let ranked = personalized_pagerank(&files, &edges, &seeds);
    fit_budget(source, request.token_budget, &ranked)
}

/// Greedy emission under the budget (guard-map.md step 4).
fn fit_budget(
    source: &dyn GraphSource,
    budget: u32,
    ranked: &[(String, f64)],
) -> Result<MapResponse, MapError> {
    let total = u32::try_from(ranked.len()).unwrap_or(u32::MAX);
    // Upper bounds for the two self-referential numbers: the final values are
    // never larger, so they never serialize longer than these placeholders.
    let mut response = MapResponse {
        backend: source.backend().to_string(),
        commit: source.commit().to_string(),
        token_budget: budget,
        estimated_tokens: budget,
        entries: Vec::new(),
        omitted: total,
    };
    if measure(&response) > budget {
        return Err(MapError::InvalidRequest(format!(
            "token_budget {budget} cannot hold the response envelope"
        )));
    }

    for (path, rank) in ranked {
        let symbols: Vec<String> = source
            .symbols(path)?
            .iter()
            .take(MAX_SYMBOLS_PER_FILE)
            .map(render_symbol)
            .collect();
        response.entries.push(MapEntry {
            path: path.clone(),
            rank: (rank * RANK_DECIMALS).round() / RANK_DECIMALS,
            symbols,
        });
        let mut fits = measure(&response) <= budget;
        while !fits {
            let entry = response.entries.last_mut().expect("just pushed");
            if entry.symbols.pop().is_none() {
                break;
            }
            fits = measure(&response) <= budget;
        }
        if !fits {
            response.entries.pop();
            break;
        }
    }

    response.omitted = total - u32::try_from(response.entries.len()).unwrap_or(total);
    // Converge `estimated_tokens` on the exact measure of the final response;
    // each step can only shrink the number, so this terminates quickly.
    for _ in 0..4 {
        let m = measure(&response);
        if m == response.estimated_tokens {
            break;
        }
        response.estimated_tokens = m;
    }
    response.estimated_tokens = response.estimated_tokens.max(measure(&response));
    debug_assert!(measure(&response) <= budget);
    Ok(response)
}

/// [`estimate_tokens`] of the serialized response (the wire form).
pub fn measure(response: &MapResponse) -> u32 {
    estimate_tokens(&serde_json::to_string(response).expect("MapResponse serializes"))
}

fn render_symbol(s: &Symbol) -> String {
    format!("{}:{}", s.name, s.line)
}

fn normalize(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.strip_prefix("./").unwrap_or(&p).to_string()
}

/// Repository files named in free ticket text (paths or `path:line`
/// references), restricted to `files`. Order of first mention, deduplicated.
pub fn mentioned_paths(ticket_text: &str, files: &[String]) -> Vec<String> {
    let known: BTreeSet<&str> = files.iter().map(String::as_str).collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in ticket_text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '<' | '>' | ','
            )
    }) {
        let token = raw.trim_end_matches(['.', ';', ':', '!', '?']);
        let token = token.split_once(':').map_or(token, |(p, _)| p);
        let token = normalize(token);
        if known.contains(token.as_str()) && seen.insert(token.clone()) {
            out.push(token);
        }
    }
    out
}

/// The `focus_files` seed: changed files first, then ticket-mentioned paths.
pub fn focus_files(changed: &[String], ticket_text: &str, files: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in changed
        .iter()
        .map(|f| normalize(f))
        .chain(mentioned_paths(ticket_text, files))
    {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

#[cfg(test)]
mod tests;
