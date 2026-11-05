//! `DecisionEngine` adapter for TypeSafe Jev.
//!
//! Implements `guard_core::ports::DecisionEngine` against
//! `POST https://api.typesafe.ai/v1/systemone` with the pinned model
//! `jev-1.13.0`. Wire types, the seven triage questions, the per-journey impact
//! Noul questions and Choice permutation follow `docs/contracts/jev-questions.md`.
//!
//! ```no_run
//! # async fn demo(request: guard_core::ports::TriageRequest) -> Result<(), guard_core::ports::EngineError> {
//! use guard_core::ports::DecisionEngine;
//! let engine = guard_jev::JevEngine::from_env()?; // TYPESAFE_API_KEY, optional JEV_URL
//! let answers = engine.triage(&request).await?;
//! # let _ = answers; Ok(()) }
//! ```

pub mod client;
pub mod convert;
pub mod error_detail;
pub mod permutation;
pub mod questions;
pub mod wire;

pub use client::{API_KEY_ENV, JevConfig, JevEngine, URL_ENV};
pub use convert::ParsedResponse;

/// The pinned model. Never an alias such as `jev-latest`.
pub const MODEL: &str = "jev-1.13.0";

/// Production endpoint (overridable with `JEV_URL`).
pub const DEFAULT_URL: &str = "https://api.typesafe.ai/v1/systemone";
