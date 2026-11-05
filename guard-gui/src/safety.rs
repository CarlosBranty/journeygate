//! The safety gate in front of every test run started from the GUI.
//!
//! - `safe`: runs without asking.
//! - `contained` and `unclassified`: need an explicit confirmation naming them.
//! - `dangerous`: never runnable from the GUI; one in a run blocks the run.

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Safety {
    Safe,
    Contained,
    Dangerous,
    /// No class known. journeys.json has no safety field (its contract allows
    /// no extra keys), so today every journey is unclassified.
    Unclassified,
}

impl Safety {
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Contained => "contained",
            Self::Dangerous => "dangerous",
            Self::Unclassified => "unclassified",
        }
    }

    /// Whether the GUI may run it at all (with confirmation if needed).
    pub fn runnable(self) -> bool {
        self != Self::Dangerous
    }

    fn needs_confirmation(self) -> bool {
        matches!(self, Self::Contained | Self::Unclassified)
    }
}

/// What the GUI must do before starting a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// Every journey is safe.
    Allowed,
    /// Ask first, naming these journeys.
    Confirm(Vec<String>),
    /// Refuse: these journeys are dangerous.
    Blocked(Vec<String>),
    /// Nothing to run.
    Empty,
}

/// The gate for a run that may touch `journeys` (id, class), in order.
pub fn check(journeys: &[(&str, Safety)]) -> Gate {
    if journeys.is_empty() {
        return Gate::Empty;
    }
    let pick = |f: fn(Safety) -> bool| -> Vec<String> {
        journeys
            .iter()
            .filter(|(_, s)| f(*s))
            .map(|(id, _)| (*id).to_string())
            .collect()
    };
    let dangerous = pick(|s| !s.runnable());
    if !dangerous.is_empty() {
        return Gate::Blocked(dangerous);
    }
    let confirm = pick(Safety::needs_confirmation);
    if confirm.is_empty() {
        Gate::Allowed
    } else {
        Gate::Confirm(confirm)
    }
}

/// One catalog journey as the suite gate sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub id: &'a str,
    pub smoke: bool,
    pub quarantined: bool,
    pub safety: Safety,
}

/// A run whose journeys guard picks by tag (cli.md `guard test`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite<'a> {
    /// `--all`: `@smoke` plus every active journey.
    All,
    /// `--affected`, from a fresh `guard affected --json`; `None` when that
    /// preview failed.
    Affected(Option<AffectedPreview<'a>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffectedPreview<'a> {
    /// `journeys[].id`: active journeys selected by path or smoke.
    pub ids: &'a [String],
    /// `tags`; a `@ticket:<KEY>` there may select any journey.
    pub tags: &'a [String],
}

/// Every journey a suite run may execute, with its class, in catalog order.
///
/// `E2E_TAGS` is a plain OR over whole tags (runner-env.md), so `@smoke` also
/// runs quarantined smoke journeys, and `@ticket:<KEY>` runs whichever spec
/// carries that tag, which the catalog does not record: with a ticket (or no
/// usable preview) every journey is a candidate. Preview ids missing from the
/// catalog count as unclassified.
pub fn suite_candidates<'a>(suite: Suite<'a>, catalog: &[Candidate<'a>]) -> Vec<(&'a str, Safety)> {
    let preview = match suite {
        Suite::All => {
            return catalog
                .iter()
                .filter(|c| c.smoke || !c.quarantined)
                .map(|c| (c.id, c.safety))
                .collect();
        }
        Suite::Affected(preview) => preview,
    };
    let Some(preview) = preview.filter(|p| !p.tags.iter().any(|t| t.starts_with("@ticket:")))
    else {
        return catalog.iter().map(|c| (c.id, c.safety)).collect();
    };
    let mut out: Vec<(&'a str, Safety)> = catalog
        .iter()
        .filter(|c| c.smoke || preview.ids.iter().any(|id| id == c.id))
        .map(|c| (c.id, c.safety))
        .collect();
    for id in preview.ids {
        if !catalog.iter().any(|c| c.id == id) {
            out.push((id.as_str(), Safety::Unclassified));
        }
    }
    out
}

/// The gate for a suite run. Never [`Gate::Allowed`]: guard resolves the final
/// selection itself (branch, working tree, spec tags), so the user always
/// confirms; `Confirm` may then name no journey.
pub fn suite_gate(suite: Suite<'_>, catalog: &[Candidate<'_>]) -> Gate {
    let candidates = suite_candidates(suite, catalog);
    match check(&candidates) {
        Gate::Allowed => Gate::Confirm(Vec::new()),
        Gate::Empty if matches!(suite, Suite::Affected(_)) => Gate::Confirm(Vec::new()),
        gate => gate,
    }
}
