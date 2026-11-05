//! `catalog/journeys.json`: parsing, validation and tags.
//!
//! Contract: `docs/contracts/journeys-catalog.md`. [`parse_catalog`] enforces the
//! JSON schema; [`check_against_config`] enforces the cross-file rules that
//! `guard doctor` reports as errors (unique ids, areas that exist).

use crate::config::{
    ConfigWarning, TargetConfig, ValidationError, ValidationErrorKind, Validator, join_errors,
};
use crate::globs;
use serde::Deserialize;
use std::collections::BTreeSet;

#[cfg(test)]
mod tests;

pub mod new_journey;

const JOURNEY_ID: &str = r"^[a-z0-9]+(\.[a-z0-9-]+)+$";
const AREA_KEY: &str = r"^[a-z][a-z0-9_]*$";

/// The parsed journey catalog.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub version: i64,
    pub journeys: Vec<Journey>,
}

/// One business journey; `quarantined == false` means active.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journey {
    pub id: String,
    pub area: String,
    pub title: String,
    pub impact_question: String,
    pub paths: Vec<String>,
    pub smoke: bool,
    pub quarantined: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("{0}")]
    Parse(String),
    #[error("{}", join_errors(.0))]
    Invalid(Vec<ValidationError>),
}

/// Cross-file problems between the catalog and `target.toml`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogIssue {
    #[error("duplicate journey id {0}")]
    DuplicateId(String),
    #[error("journey {journey}: area {area} is not a key of [areas] in target.toml")]
    UnknownArea { journey: String, area: String },
}

impl Catalog {
    /// Journeys with `quarantined == false`, in file order.
    pub fn active(&self) -> impl Iterator<Item = &Journey> {
        self.journeys.iter().filter(|j| !j.quarantined)
    }

    pub fn find(&self, id: &str) -> Option<&Journey> {
        self.journeys.iter().find(|j| j.id == id)
    }

    pub fn quarantined_count(&self) -> usize {
        self.journeys.iter().filter(|j| j.quarantined).count()
    }
}

impl Journey {
    /// `@journey:<id>`.
    pub fn tag(&self) -> String {
        journey_tag(&self.id)
    }

    /// `@area:<area>`.
    pub fn area_tag(&self) -> String {
        format!("@area:{}", self.area)
    }
}

pub fn journey_tag(id: &str) -> String {
    format!("@journey:{id}")
}

/// Parses and schema-validates `journeys.json`.
pub fn parse_catalog(text: &str) -> Result<Catalog, CatalogError> {
    let catalog: Catalog =
        serde_json::from_str(text).map_err(|e| CatalogError::Parse(e.to_string()))?;
    let mut v = Validator::default();
    if catalog.version != 1 {
        v.push(
            "version",
            ValidationErrorKind::UnsupportedVersion(catalog.version),
        );
    }
    let id_re = regex::Regex::new(JOURNEY_ID).expect("built-in pattern compiles");
    for (index, journey) in catalog.journeys.iter().enumerate() {
        // Locate errors by id once the id itself is valid, by index otherwise.
        let label = if id_re.is_match(&journey.id) {
            format!("journeys[{}]", journey.id)
        } else {
            v.pattern(&format!("journeys[{index}].id"), &journey.id, JOURNEY_ID);
            format!("journeys[{index}]")
        };
        v.pattern(&format!("{label}.area"), &journey.area, AREA_KEY);
        if journey.title.is_empty() {
            v.push(&format!("{label}.title"), ValidationErrorKind::Empty);
        }
        if journey.impact_question.is_empty() {
            v.push(
                &format!("{label}.impact_question"),
                ValidationErrorKind::Empty,
            );
        }
        v.globs(&format!("{label}.paths"), &journey.paths);
    }
    if v.errors.is_empty() {
        Ok(catalog)
    } else {
        Err(CatalogError::Invalid(v.errors))
    }
}

/// Unknown areas first (in file order), then duplicate ids.
pub fn check_against_config(catalog: &Catalog, config: &TargetConfig) -> Vec<CatalogIssue> {
    let mut issues: Vec<CatalogIssue> = catalog
        .journeys
        .iter()
        .filter(|j| !config.areas.contains_key(&j.area))
        .map(|j| CatalogIssue::UnknownArea {
            journey: j.id.clone(),
            area: j.area.clone(),
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut reported = BTreeSet::new();
    for journey in &catalog.journeys {
        if !seen.insert(journey.id.as_str()) && reported.insert(journey.id.as_str()) {
            issues.push(CatalogIssue::DuplicateId(journey.id.clone()));
        }
    }
    issues
}

/// Dead globs in journey `paths`.
pub fn lint(catalog: &Catalog, tracked: &[String]) -> Vec<ConfigWarning> {
    catalog
        .journeys
        .iter()
        .flat_map(|j| {
            j.paths
                .iter()
                .filter(|g| !globs::matches_any(g, tracked))
                .map(move |g| ConfigWarning::DeadGlob {
                    location: format!("journeys[{}].paths", j.id),
                    glob: g.clone(),
                })
        })
        .collect()
}
