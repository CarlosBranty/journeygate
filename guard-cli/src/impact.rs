//! The Phase 2 impact step of `--affected` (ports.md, "Phase 2 changes" 1).
//!
//! `[server].url` set → `POST /api/impact`; else `TYPESAFE_API_KEY` set →
//! `guard-jev` locally; else not configured (path rules only). A configured
//! engine that fails selects every active journey (fail closed).

use crate::git;
use crate::runtime::block_on;
use crate::server_client::ServerClient;
use guard_core::catalog::Catalog;
use guard_core::config::TargetConfig;
use guard_core::ports::{DecisionEngine, DiffSummary, JourneyQuestion};
use guard_mcp::types::{ImpactInfo, ImpactSource, ImpactStatus};
use std::collections::BTreeMap;
use std::path::Path;

/// What the impact step produced.
#[derive(Debug, Clone, PartialEq)]
pub enum ImpactAnswer {
    /// Probabilities per journey id and the threshold to compare against.
    Answered {
        impact: BTreeMap<String, f64>,
        threshold: f64,
    },
    /// The engine failed: every active journey runs.
    Unavailable { threshold: f64, reason: String },
    /// No server and no API key: path rules only.
    NotConfigured,
}

/// The active journeys and their impact questions, in catalog order.
pub fn questions(catalog: &Catalog) -> Vec<JourneyQuestion> {
    catalog
        .active()
        .map(|j| JourneyQuestion {
            id: j.id.clone(),
            impact_question: j.impact_question.clone(),
        })
        .collect()
}

/// The `impact` object of `guard affected --json`.
pub fn info(answer: &ImpactAnswer, source: ImpactSource) -> ImpactInfo {
    match answer {
        ImpactAnswer::Answered { threshold, .. } => ImpactInfo {
            source,
            status: ImpactStatus::Ok,
            threshold: Some(*threshold),
        },
        ImpactAnswer::Unavailable { threshold, .. } => ImpactInfo {
            source,
            status: ImpactStatus::EngineUnavailable,
            threshold: Some(*threshold),
        },
        ImpactAnswer::NotConfigured => ImpactInfo {
            source: ImpactSource::None,
            status: ImpactStatus::NotConfigured,
            threshold: None,
        },
    }
}

/// Which engine `ask` would use.
pub fn source(config: &TargetConfig, api_key: Option<&str>) -> ImpactSource {
    if config.server.is_some() {
        ImpactSource::Server
    } else if api_key.is_some_and(|k| !k.trim().is_empty()) {
        ImpactSource::Local
    } else {
        ImpactSource::None
    }
}

/// The diff summary the impact questions see: `git diff --no-renames <merge-base>`.
/// A git error is an error, never an empty diff (that would fail open).
pub fn diff_summary(root: &Path, merge_base: &str) -> Result<DiffSummary, String> {
    let text = git::diff_text(root, merge_base).map_err(|e| format!("git diff: {e}"))?;
    Ok(guard_impact::summarize_diff(&text))
}

/// Runs the impact step. `threshold` is `thresholds.impact_threshold`; the
/// server's answer carries its own threshold, which wins.
pub fn ask(
    config: &TargetConfig,
    catalog: &Catalog,
    threshold: f64,
    diff: impl FnOnce() -> Result<DiffSummary, String>,
) -> (ImpactAnswer, ImpactSource) {
    let api_key = std::env::var(guard_jev::API_KEY_ENV).ok();
    let source = source(config, api_key.as_deref());
    let journeys = questions(catalog);
    if source == ImpactSource::None {
        return (ImpactAnswer::NotConfigured, source);
    }
    let diff = match diff() {
        Ok(diff) => diff,
        Err(reason) => return (ImpactAnswer::Unavailable { threshold, reason }, source),
    };
    let answer = match source {
        ImpactSource::None => ImpactAnswer::NotConfigured,
        ImpactSource::Server => {
            let client = ServerClient::from_config(config).expect("[server] is set");
            match block_on(client.impact(&diff, &journeys)) {
                Ok(response) => answered(response.impact, response.threshold, &journeys),
                Err(error) => ImpactAnswer::Unavailable {
                    threshold,
                    reason: error.to_string(),
                },
            }
        }
        ImpactSource::Local => {
            let result = guard_jev::JevEngine::from_env()
                .map(|engine| block_on(async move { engine.impact(&diff, &journeys).await }));
            match result {
                Ok(Ok(impact)) => answered(impact, threshold, &questions(catalog)),
                Ok(Err(error)) | Err(error) => ImpactAnswer::Unavailable {
                    threshold,
                    reason: error.to_string(),
                },
            }
        }
    };
    (answer, source)
}

/// Accepts an answer only when every requested id got a finite probability.
fn answered(
    impact: BTreeMap<String, f64>,
    threshold: f64,
    journeys: &[JourneyQuestion],
) -> ImpactAnswer {
    let complete = journeys
        .iter()
        .all(|j| impact.get(&j.id).is_some_and(|p| p.is_finite()));
    if complete && threshold.is_finite() {
        ImpactAnswer::Answered { impact, threshold }
    } else {
        ImpactAnswer::Unavailable {
            threshold,
            reason: "incomplete impact answer".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::catalog::parse_catalog;
    use guard_core::config::parse_target;

    const TARGET: &str = r#"
version = 1
[app]
name = "x"
base_url = "http://localhost:3000"
[areas.a]
description = "A"
paths = ["src/**"]
"#;

    fn catalog() -> Catalog {
        parse_catalog(
            r#"{"version": 1, "journeys": [
              {"id": "a.b", "area": "a", "title": "t", "impact_question": "qa", "paths": [], "smoke": false, "quarantined": false},
              {"id": "c.d", "area": "a", "title": "t", "impact_question": "qc", "paths": [], "smoke": false, "quarantined": true}
            ]}"#,
        )
        .unwrap()
    }

    #[test]
    fn questions_cover_only_active_journeys() {
        assert_eq!(
            questions(&catalog()),
            vec![JourneyQuestion {
                id: "a.b".into(),
                impact_question: "qa".into()
            }]
        );
    }

    #[test]
    fn source_prefers_the_server_then_a_local_key() {
        let mut config = parse_target(TARGET).unwrap();
        assert_eq!(source(&config, None), ImpactSource::None);
        assert_eq!(source(&config, Some(" ")), ImpactSource::None);
        assert_eq!(source(&config, Some("k")), ImpactSource::Local);
        config.server = Some(guard_core::config::Server {
            url: "http://s".into(),
        });
        assert_eq!(source(&config, Some("k")), ImpactSource::Server);
    }

    #[test]
    fn incomplete_or_non_finite_answers_are_unavailable() {
        let journeys = questions(&catalog());
        let ok = answered(BTreeMap::from([("a.b".into(), 0.3)]), 0.25, &journeys);
        assert!(matches!(ok, ImpactAnswer::Answered { .. }));
        let missing = answered(BTreeMap::new(), 0.25, &journeys);
        assert!(matches!(missing, ImpactAnswer::Unavailable { .. }));
        let nan = answered(BTreeMap::from([("a.b".into(), f64::NAN)]), 0.25, &journeys);
        assert!(matches!(nan, ImpactAnswer::Unavailable { .. }));
    }

    #[test]
    fn info_matches_the_affected_json_contract() {
        let json =
            serde_json::to_value(info(&ImpactAnswer::NotConfigured, ImpactSource::None)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"source": "none", "status": "not_configured"})
        );
        let json = serde_json::to_value(info(
            &ImpactAnswer::Unavailable {
                threshold: 0.25,
                reason: "x".into(),
            },
            ImpactSource::Server,
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({"source": "server", "status": "engine_unavailable", "threshold": 0.25})
        );
    }

    #[test]
    fn a_server_that_is_down_makes_the_engine_unavailable() {
        let mut config = parse_target(TARGET).unwrap();
        config.server = Some(guard_core::config::Server {
            url: "http://127.0.0.1:9".into(),
        });
        let (answer, source) = ask(&config, &catalog(), 0.25, || Ok(DiffSummary::default()));
        assert_eq!(source, ImpactSource::Server);
        assert!(
            matches!(answer, ImpactAnswer::Unavailable { threshold, .. } if threshold == 0.25),
            "{answer:?}"
        );
    }

    #[test]
    fn a_failed_diff_makes_the_engine_unavailable_without_asking() {
        let mut config = parse_target(TARGET).unwrap();
        // Would answer if asked; the diff error must win first.
        config.server = Some(guard_core::config::Server {
            url: "http://127.0.0.1:9".into(),
        });
        let (answer, _) = ask(&config, &catalog(), 0.25, || {
            Err("git diff failed: bad revision".to_string())
        });
        match answer {
            ImpactAnswer::Unavailable { reason, .. } => {
                assert!(reason.contains("bad revision"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn diff_summary_reports_git_errors_and_ignores_user_diff_config() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@t"]);
        git(root, &["config", "user.name", "t"]);
        std::fs::write(root.join("a.ts"), "one\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);
        std::fs::write(root.join("a.ts"), "two\n").unwrap();
        git(root, &["config", "color.ui", "always"]);
        git(root, &["config", "diff.external", "guard-no-such-differ"]);
        let text = git::diff_text(root, "HEAD").unwrap();
        assert!(text.contains("+two"), "{text}");
        assert!(!text.contains('\u{1b}'), "colored diff: {text:?}");
        assert!(diff_summary(root, "no-such-rev").is_err());
    }
}
