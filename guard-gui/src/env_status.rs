//! Which `E2E_*` environment variables are set, by name only.
//!
//! Values are never kept: [`from_process`] drops each value as soon as the
//! environment iterator yields it, and nothing here can return one.

use std::collections::BTreeSet;
use std::ffi::OsString;

#[cfg(test)]
mod tests;

pub const PREFIX: &str = "E2E_";

/// Names in `keys` that start with `E2E_` (and have more after it), sorted and unique.
pub fn e2e_names(keys: impl IntoIterator<Item = OsString>) -> Vec<String> {
    keys.into_iter()
        .filter_map(|k| k.into_string().ok())
        .filter(|k| k.len() > PREFIX.len() && k.starts_with(PREFIX))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The `E2E_*` names of this process's environment.
pub fn from_process() -> Vec<String> {
    e2e_names(std::env::vars_os().map(|(name, _value)| name))
}
