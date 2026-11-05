//! guard-gui: a native desktop window over the `guard` binary.
//!
//! The pure modules (catalog, results, commands, safety, env_status, ...) hold
//! every rule and are unit-tested; [`app`] is a thin egui layer over them. The
//! GUI never re-implements guard: it shells out to the `guard` binary.

pub mod accounts;
pub mod ai_settings;
pub mod app;
pub mod args;
pub mod catalog;
pub mod commands;
pub mod credentials;
pub mod env_status;
pub mod export;
pub mod flow;
pub mod guard_bin;
pub mod integrations;
pub mod log_buffer;
pub mod markdown;
pub mod models;
pub mod open;
pub mod projects;
pub mod results;
pub mod run_progress;
pub mod runner;
pub mod safety;
pub mod status;
pub mod suspects;
pub mod theme;
