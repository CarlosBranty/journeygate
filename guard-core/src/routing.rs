//! Tag selection: which Playwright tags a run passes in `E2E_TAGS`.
//!
//! ```text
//! affected = { @smoke }
//!          ∪ { @ticket:<KEY> }                       if the branch has a ticket
//!          ∪ { @journey:<id> : j active, a changed file matches a glob in j.paths }
//! ```

use crate::catalog::{Catalog, Journey, journey_tag};
use crate::globs;
use crate::ticket::ticket_tag;
use serde::Serialize;
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

pub const SMOKE_TAG: &str = "@smoke";

/// Why a journey runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    /// A changed file matched one of the journey's globs.
    Path { file: String, glob: String },
    /// The journey is a smoke journey and runs through `@smoke`.
    Smoke,
    /// Selected by `--all`.
    All,
    /// Selected by `--journey <id>`.
    Requested,
    /// The impact engine answered `probability > impact_threshold` (Phase 2).
    Impact { probability: f64 },
    /// The impact engine failed: every active journey runs (fail closed).
    EngineUnavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SelectedJourney {
    pub id: String,
    pub tag: String,
    pub reasons: Vec<Reason>,
}

/// The tags to run, plus the journeys they cover and why (sorted by id).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Selection {
    pub tags: BTreeSet<String>,
    pub journeys: Vec<SelectedJourney>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoutingError {
    #[error("unknown journey id: {0}")]
    UnknownJourney(String),
    #[error("invalid glob in catalog: {0}")]
    InvalidGlob(String),
}

impl Selection {
    /// The `E2E_TAGS` value: tags sorted lexicographically, comma-separated.
    pub fn e2e_tags(&self) -> String {
        self.tags.iter().cloned().collect::<Vec<_>>().join(",")
    }

    fn finish(mut self) -> Self {
        self.journeys.sort_by(|a, b| a.id.cmp(&b.id));
        self
    }
}

fn selected(journey: &Journey, reasons: Vec<Reason>) -> SelectedJourney {
    SelectedJourney {
        id: journey.id.clone(),
        tag: journey.tag(),
        reasons,
    }
}

/// The `--affected` selection. `changed_files` keeps its order; each matching
/// file yields one reason with the first glob that matches it.
pub fn select_affected(
    catalog: &Catalog,
    changed_files: &[String],
    ticket: Option<&str>,
) -> Result<Selection, RoutingError> {
    let mut selection = Selection::default();
    selection.tags.insert(SMOKE_TAG.to_string());
    if let Some(key) = ticket {
        selection.tags.insert(ticket_tag(key));
    }
    for journey in catalog.active() {
        let matchers = journey
            .paths
            .iter()
            .map(|g| {
                globs::compile(g)
                    .map(|m| (g, m))
                    .map_err(|_| RoutingError::InvalidGlob(g.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut reasons: Vec<Reason> = changed_files
            .iter()
            .filter_map(|file| {
                matchers
                    .iter()
                    .find(|(glob, m)| globs::matches_entry(glob, m, file))
                    .map(|(glob, _)| Reason::Path {
                        file: file.clone(),
                        glob: (*glob).clone(),
                    })
            })
            .collect();
        if !reasons.is_empty() {
            selection.tags.insert(journey.tag());
        }
        if journey.smoke {
            reasons.push(Reason::Smoke);
        }
        if !reasons.is_empty() {
            selection.journeys.push(selected(journey, reasons));
        }
    }
    Ok(selection.finish())
}

/// The `--affected` selection with the Phase 2 impact rule (ports.md):
/// `impact = Some(map)` adds every active journey whose probability is
/// strictly above `threshold` (a NaN never selects); `impact = None` means the
/// engine failed, so every active journey runs with `EngineUnavailable`.
/// Impact reasons follow the path reasons and precede `Smoke`.
pub fn select_affected_with_impact(
    catalog: &Catalog,
    changed_files: &[String],
    impact: Option<&std::collections::BTreeMap<String, f64>>,
    threshold: f64,
    ticket: Option<&str>,
) -> Result<Selection, RoutingError> {
    let mut selection = select_affected(catalog, changed_files, ticket)?;
    for journey in catalog.active() {
        let reason = match impact {
            None => Reason::EngineUnavailable,
            Some(map) => match map.get(&journey.id) {
                Some(&p) if p > threshold => Reason::Impact { probability: p },
                _ => continue,
            },
        };
        selection.tags.insert(journey.tag());
        match selection.journeys.iter_mut().find(|j| j.id == journey.id) {
            Some(existing) => {
                let at = existing
                    .reasons
                    .iter()
                    .position(|r| *r == Reason::Smoke)
                    .unwrap_or(existing.reasons.len());
                existing.reasons.insert(at, reason);
            }
            None => selection.journeys.push(selected(journey, vec![reason])),
        }
    }
    Ok(selection.finish())
}

/// The `--all` selection: `@smoke` plus every active journey.
pub fn select_all(catalog: &Catalog) -> Selection {
    let mut selection = Selection::default();
    selection.tags.insert(SMOKE_TAG.to_string());
    for journey in catalog.active() {
        selection.tags.insert(journey.tag());
        selection
            .journeys
            .push(selected(journey, vec![Reason::All]));
    }
    selection.finish()
}

/// The `--journey <id>...` selection: exactly those journeys, quarantined allowed.
pub fn select_journeys(catalog: &Catalog, ids: &[String]) -> Result<Selection, RoutingError> {
    let mut selection = Selection::default();
    for id in ids {
        let journey = catalog
            .find(id)
            .ok_or_else(|| RoutingError::UnknownJourney(id.clone()))?;
        if selection.tags.insert(journey_tag(id)) {
            selection
                .journeys
                .push(selected(journey, vec![Reason::Requested]));
        }
    }
    Ok(selection.finish())
}
