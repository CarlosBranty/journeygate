//! The journey list, read from `<repo>/e2e-playwright/catalog/journeys.json`.
//!
//! Parsing and validation are guard's own (`guard_core::catalog`), so the GUI
//! accepts exactly what `guard` accepts (journeys-catalog.md).

use crate::safety::Safety;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// The Playwright project dir under the repo (cli.md `--e2e-dir` default).
pub const E2E_DIR: &str = "e2e-playwright";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JourneyRow {
    pub id: String,
    pub area: String,
    pub title: String,
    pub paths: Vec<String>,
    pub smoke: bool,
    pub quarantined: bool,
    pub safety: Safety,
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogLoadError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid journeys.json: {0}")]
    Invalid(#[from] guard_core::catalog::CatalogError),
}

pub fn e2e_dir(repo: &Path) -> PathBuf {
    repo.join(E2E_DIR)
}

pub fn catalog_path(repo: &Path) -> PathBuf {
    e2e_dir(repo).join("catalog").join("journeys.json")
}

/// Reads and validates the catalog of `repo`.
pub fn load(repo: &Path) -> Result<Vec<JourneyRow>, CatalogLoadError> {
    let path = catalog_path(repo);
    let text =
        std::fs::read_to_string(&path).map_err(|source| CatalogLoadError::Read { path, source })?;
    parse_rows(&text)
}

pub fn parse_rows(text: &str) -> Result<Vec<JourneyRow>, CatalogLoadError> {
    let catalog = guard_core::catalog::parse_catalog(text)?;
    Ok(catalog
        .journeys
        .into_iter()
        .map(|j| JourneyRow {
            id: j.id,
            area: j.area,
            title: j.title,
            paths: j.paths,
            smoke: j.smoke,
            quarantined: j.quarantined,
            safety: Safety::Unclassified,
        })
        .collect())
}

/// Distinct areas, sorted.
pub fn areas(rows: &[JourneyRow]) -> Vec<String> {
    rows.iter()
        .map(|r| r.area.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl JourneyRow {
    /// `tests/journeys/<area>/<id-without-area-prefix>.spec.ts`, the layout
    /// convention of journeys-catalog.md.
    pub fn conventional_spec_file(&self) -> String {
        let rest = self
            .id
            .strip_prefix(&format!("{}.", self.area))
            .or_else(|| self.id.split_once('.').map(|(_, rest)| rest))
            .unwrap_or(&self.id);
        format!("tests/journeys/{}/{rest}.spec.ts", self.area)
    }

    /// Case-insensitive search over id, title and area, plus an exact area filter.
    pub fn matches(&self, search: &str, area: Option<&str>) -> bool {
        if area.is_some_and(|a| a != self.area) {
            return false;
        }
        let needle = search.trim().to_lowercase();
        needle.is_empty()
            || [&self.id, &self.title, &self.area]
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
    }
}

/// One row of the Tests tree: an area header or a journey (an index into the
/// rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeItem {
    /// `count` is the number of journeys of the area that match the search.
    Area {
        name: String,
        count: usize,
    },
    Journey(usize),
}

/// The visible rows of the Tests tree: areas sorted by name, each followed by
/// its matching journeys (catalog order) unless it is collapsed. A non-empty
/// search hides areas without matches and opens the ones with matches.
pub fn tree_items(
    rows: &[JourneyRow],
    search: &str,
    collapsed: &BTreeSet<String>,
) -> Vec<TreeItem> {
    let mut groups: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
    for (index, row) in rows.iter().enumerate() {
        if row.matches(search, None) {
            groups.entry(row.area.as_str()).or_default().push(index);
        }
    }
    let searching = !search.trim().is_empty();
    let mut items = Vec::new();
    for (area, indices) in groups {
        items.push(TreeItem::Area {
            name: area.to_string(),
            count: indices.len(),
        });
        if searching || !collapsed.contains(area) {
            items.extend(indices.into_iter().map(TreeItem::Journey));
        }
    }
    items
}
