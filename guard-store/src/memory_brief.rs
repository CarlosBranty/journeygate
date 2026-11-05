//! The facts brief injected into `task.json` `facts_brief` at sandbox start
//! (docs/contracts/memory.md, "Facts brief").
//!
//! Collects the `active` and `needs_review` facts of the ticket's repo, its
//! areas and its affected journeys, then renders them with
//! [`FactsBrief::render`] at [`FACTS_BRIEF_MAX_BYTES`]: ordered by (scope,
//! scope_key, topic_key, id), so the brief is byte-identical for the same
//! memory whatever order the areas and journeys are given in (a stable cached
//! prefix, tokens.md), and never longer than 4096 bytes.
//!
//! Built from a [`MemoryFactsReader`] only: in production the read-only
//! `PgMemoryFactsReader`.

use guard_core::ports::memory_facts::{FACTS_BRIEF_MAX_BYTES, MAX_FACTS_PAGE};
use guard_core::ports::{
    FactScope, FactsBrief, FactsQuery, MemoryFact, MemoryFactsReader, StoreError,
};
use std::collections::{BTreeMap, BTreeSet};

/// What a ticket's brief covers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BriefScope {
    pub repo: String,
    /// `[areas]` keys of the ticket (triage area plus areas of touched paths).
    pub areas: Vec<String>,
    /// Affected journey ids.
    pub journeys: Vec<String>,
}

/// Every fact matching `query`, all pages (its `limit` and `cursor` are
/// replaced).
pub async fn all_facts(
    reader: &dyn MemoryFactsReader,
    mut query: FactsQuery,
) -> Result<Vec<MemoryFact>, StoreError> {
    query.limit = MAX_FACTS_PAGE;
    query.cursor = None;
    let mut out = vec![];
    loop {
        let page = reader.list_facts(&query).await?;
        out.extend(page.facts);
        match page.next_cursor {
            Some(c) => query.cursor = Some(c),
            None => return Ok(out),
        }
    }
}

/// Builds the brief of `scope` (see the module docs).
pub async fn build_facts_brief(
    reader: &dyn MemoryFactsReader,
    scope: &BriefScope,
) -> Result<FactsBrief, StoreError> {
    let mut targets: BTreeSet<(FactScope, Option<String>)> = BTreeSet::new();
    targets.insert((FactScope::Repo, None));
    for a in scope.areas.iter().filter(|a| !a.trim().is_empty()) {
        targets.insert((FactScope::Area, Some(a.clone())));
    }
    for j in scope.journeys.iter().filter(|j| !j.trim().is_empty()) {
        targets.insert((FactScope::Journey, Some(j.clone())));
    }
    let mut facts: BTreeMap<i64, MemoryFact> = BTreeMap::new();
    for (s, key) in targets {
        let query = FactsQuery {
            scope: Some(s),
            scope_key: key.clone(),
            ..FactsQuery::for_repo(&scope.repo)
        };
        for f in all_facts(reader, query).await? {
            // `scope_key: None` matches any key; keep only exact matches.
            if f.scope == s && f.scope_key == key {
                facts.insert(f.id.0, f);
            }
        }
    }
    let facts: Vec<MemoryFact> = facts.into_values().collect();
    Ok(FactsBrief::render(&facts, FACTS_BRIEF_MAX_BYTES))
}
