//! Pure domain logic of `guard`.
//!
//! Every module here is free of process, network and filesystem I/O: callers
//! read files and run git, then hand plain values to these functions. That keeps
//! the rules (routing, shard planning, verdicts) testable with unit tests only.
//!
//! [`ports`] declares the async traits that the adapter crates implement
//! (`guard-jev`, `guard-tracker`, `guard-codehost`, `guard-store`,
//! `guard-impact`) and the domain types they exchange.

pub mod baseline;
pub mod catalog;
pub mod config;
pub mod config_edit;
pub mod credentials;
pub mod detect;
pub mod durations;
pub mod fingerprint;
pub mod gatekeeper;
mod globs;
pub use globs::PathMatcher;
pub mod journey_changes;
pub mod ports;
pub mod routing;
pub mod runner_env;
pub mod self_mode;
pub mod shard_plan;
pub mod thresholds;
pub mod ticket;
pub mod timefmt;
pub mod triage;
pub mod verdict;
