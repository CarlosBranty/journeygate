//! `guard_catalog`: a read-only, filtered view of the journey catalog.

use crate::types::{CatalogEntry, CatalogOutput};
use guard_core::catalog::Catalog;
use guard_core::routing::SMOKE_TAG;

/// `guard_catalog` filters; they combine with AND and none = every journey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogQuery {
    pub area: Option<String>,
    /// A tag of the journey, e.g. `@smoke` or `@journey:checkout.customer-card`.
    pub tag: Option<String>,
    /// Case-insensitive substring of id, title or impact question.
    pub text: Option<String>,
    pub include_quarantined: bool,
}

impl Default for CatalogQuery {
    fn default() -> Self {
        Self {
            area: None,
            tag: None,
            text: None,
            include_quarantined: true,
        }
    }
}

/// Applies `query` to `catalog`; entries are sorted by id.
pub fn search(catalog: &Catalog, query: &CatalogQuery) -> CatalogOutput {
    let text = query.text.as_deref().map(str::to_lowercase);
    let mut journeys: Vec<CatalogEntry> = catalog
        .journeys
        .iter()
        .filter(|j| query.include_quarantined || !j.quarantined)
        .filter(|j| query.area.as_deref().is_none_or(|area| j.area == area))
        .filter(|j| {
            text.as_deref().is_none_or(|needle| {
                [&j.id, &j.title, &j.impact_question]
                    .iter()
                    .any(|field| field.to_lowercase().contains(needle))
            })
        })
        .map(|j| {
            let mut tags = vec![j.tag(), j.area_tag()];
            if j.smoke {
                tags.push(SMOKE_TAG.to_string());
            }
            CatalogEntry {
                id: j.id.clone(),
                area: j.area.clone(),
                title: j.title.clone(),
                tags,
                paths: j.paths.clone(),
                smoke: j.smoke,
                quarantined: j.quarantined,
            }
        })
        .filter(|entry| {
            query
                .tag
                .as_deref()
                .is_none_or(|tag| entry.tags.iter().any(|t| t == tag))
        })
        .collect();
    journeys.sort_by(|a, b| a.id.cmp(&b.id));
    CatalogOutput { journeys }
}

#[cfg(test)]
mod tests;
