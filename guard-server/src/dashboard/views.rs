//! View models of the dashboard: pure functions from catalog, config and
//! stored results to the rows the templates print.

use guard_core::catalog::Catalog;
use guard_core::config::TargetConfig;
use guard_core::ports::{
    BaselineOutcome, CalibrationRow, GateVerdict, Lane, LaneKind, StoredGateResult, TestStatus,
};
use std::collections::BTreeMap;

pub fn verdict(v: GateVerdict) -> &'static str {
    match v {
        GateVerdict::Green => "green",
        GateVerdict::Red => "red",
    }
}

fn status_name(s: TestStatus) -> &'static str {
    match s {
        TestStatus::Passed => "passed",
        TestStatus::Flaky => "flaky",
        TestStatus::Failed => "failed",
        TestStatus::Skipped => "skipped",
    }
}

fn percent(num: usize, den: usize) -> String {
    if den == 0 {
        "n/a".into()
    } else {
        format!("{:.0}%", num as f64 * 100.0 / den as f64)
    }
}

fn millis(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

// ------------------------------------------------------------------ overview / guards

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRow {
    pub run_id: String,
    pub received_at: String,
    pub repo: String,
    pub pr_label: String,
    pub pr_url: Option<String>,
    pub ticket: String,
    pub verdict: &'static str,
    pub regressions: usize,
    pub modified: String,
    /// Bug key and its link (`None`: not an http(s) URL, shown as text).
    pub bugs: Vec<(String, Option<String>)>,
    pub report_url: Option<String>,
    pub ci_url: Option<String>,
}

/// A stored URL safe to put in `href`: it parses and its scheme is http(s).
/// Anything else (`javascript:`, `data:`) renders as plain text.
pub fn safe_link(raw: &str) -> Option<String> {
    url::Url::parse(raw.trim())
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|_| raw.trim().to_string())
}

pub fn gate_rows(results: &[StoredGateResult]) -> Vec<GateRow> {
    results
        .iter()
        .map(|g| {
            let r = &g.report;
            let mut modified: Vec<&str> = r
                .journey_changes
                .iter()
                .map(|j| j.journey.as_str())
                .collect();
            modified.dedup();
            GateRow {
                run_id: r.run_id.clone(),
                received_at: g.received_at.clone(),
                repo: r.repo.clone().unwrap_or_default(),
                pr_label: r
                    .pr
                    .as_ref()
                    .map_or("—".into(), |p| format!("#{} {}", p.number, p.title)),
                pr_url: r.pr.as_ref().and_then(|p| safe_link(&p.url)),
                ticket: r.ticket_key.clone().unwrap_or_else(|| "—".into()),
                verdict: verdict(r.verdict),
                regressions: r.counts.regressions,
                modified: modified.join(", "),
                bugs: g
                    .outcome
                    .bugs
                    .iter()
                    .map(|b| (b.key.clone(), safe_link(&b.url)))
                    .collect(),
                report_url: r.report_url.as_deref().and_then(safe_link),
                ci_url: r.ci_url.as_deref().and_then(safe_link),
            }
        })
        .collect()
}

// ------------------------------------------------------------------ catalog

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JourneyRow {
    pub id: String,
    pub title: String,
    pub tags: String,
    pub smoke: bool,
    pub quarantined: bool,
    pub last: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaGroup {
    pub area: String,
    pub journeys: Vec<JourneyRow>,
}

/// The newest result (results are newest first) that ran the journey:
/// `failed` beats `flaky` beats `passed` beats `skipped`.
fn last_result(results: &[StoredGateResult], journey: &str) -> String {
    let rank = |s: TestStatus| match s {
        TestStatus::Failed => 3,
        TestStatus::Flaky => 2,
        TestStatus::Passed => 1,
        TestStatus::Skipped => 0,
    };
    results
        .iter()
        .find_map(|g| {
            g.report
                .tests
                .iter()
                .filter(|t| t.journey.as_deref() == Some(journey))
                .map(|t| t.status)
                .max_by_key(|s| rank(*s))
        })
        .map_or("—".into(), |s| status_name(s).into())
}

pub fn catalog_groups(catalog: &Catalog, results: &[StoredGateResult]) -> Vec<AreaGroup> {
    let mut groups: BTreeMap<String, Vec<JourneyRow>> = BTreeMap::new();
    for j in &catalog.journeys {
        let mut tags = vec![j.tag(), j.area_tag()];
        if j.smoke {
            tags.push("@smoke".into());
        }
        groups.entry(j.area.clone()).or_default().push(JourneyRow {
            id: j.id.clone(),
            title: j.title.clone(),
            tags: tags.join(" "),
            smoke: j.smoke,
            quarantined: j.quarantined,
            last: last_result(results, &j.id),
        });
    }
    groups
        .into_iter()
        .map(|(area, journeys)| AreaGroup { area, journeys })
        .collect()
}

// ------------------------------------------------------------------ coverage

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageRow {
    pub area: String,
    pub description: String,
    pub active: usize,
    pub quarantined: usize,
}

/// `[areas]` with zero or one active journeys.
pub fn coverage_rows(config: &TargetConfig, catalog: &Catalog) -> Vec<CoverageRow> {
    config
        .areas
        .iter()
        .map(|(key, area)| {
            let of_area = || catalog.journeys.iter().filter(|j| &j.area == key);
            CoverageRow {
                area: key.clone(),
                description: area.description.clone(),
                active: of_area().filter(|j| !j.quarantined).count(),
                quarantined: of_area().filter(|j| j.quarantined).count(),
            }
        })
        .filter(|r| r.active <= 1)
        .collect()
}

// ------------------------------------------------------------------ flaky

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlakyRow {
    pub test_id: String,
    pub journey: String,
    pub runs: usize,
    pub flaky: usize,
    pub rate: String,
    pub quarantined: bool,
}

pub fn flaky_rows(catalog: &Catalog, results: &[StoredGateResult]) -> Vec<FlakyRow> {
    // test_id -> (journey, runs, flaky)
    let mut stats: BTreeMap<String, (Option<String>, usize, usize)> = BTreeMap::new();
    for g in results {
        for t in g
            .report
            .tests
            .iter()
            .filter(|t| t.status != TestStatus::Skipped)
        {
            let e = stats
                .entry(t.test_id.clone())
                .or_insert_with(|| (t.journey.clone(), 0, 0));
            e.1 += 1;
            let flaky = t.status == TestStatus::Flaky
                || (t.status == TestStatus::Failed && t.baseline == Some(BaselineOutcome::Flaky));
            if flaky {
                e.2 += 1;
            }
        }
    }
    let mut rows: Vec<FlakyRow> = stats
        .into_iter()
        .filter(|(_, (_, _, flaky))| *flaky > 0)
        .map(|(test_id, (journey, runs, flaky))| FlakyRow {
            quarantined: journey
                .as_deref()
                .and_then(|j| catalog.find(j))
                .is_some_and(|j| j.quarantined),
            journey: journey.unwrap_or_else(|| "—".into()),
            rate: percent(flaky, runs),
            test_id,
            runs,
            flaky,
        })
        .collect();
    rows.sort_by(|a, b| {
        (b.flaky * a.runs)
            .cmp(&(a.flaky * b.runs))
            .then(a.test_id.cmp(&b.test_id))
    });
    rows
}

// ------------------------------------------------------------------ durations

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurationRow {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Durations {
    /// Per gate run: total test time.
    pub gates: Vec<DurationRow>,
    /// Spec files by average time per run, slowest first.
    pub files: Vec<DurationRow>,
    /// Tests by average duration, most expensive first.
    pub tests: Vec<DurationRow>,
}

const TOP: usize = 20;

fn averaged(map: BTreeMap<String, (u64, u64)>) -> Vec<DurationRow> {
    let mut rows: Vec<(String, u64)> = map
        .into_iter()
        .map(|(k, (sum, n))| (k, sum / n.max(1)))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.into_iter()
        .take(TOP)
        .map(|(name, ms)| DurationRow {
            name,
            value: millis(ms),
        })
        .collect()
}

pub fn durations(results: &[StoredGateResult]) -> Durations {
    let mut files: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut tests: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut gates = Vec::new();
    for g in results {
        let mut per_file: BTreeMap<&str, u64> = BTreeMap::new();
        for t in &g.report.tests {
            *per_file.entry(&t.file).or_default() += t.duration_ms;
            let e = tests.entry(t.test_id.clone()).or_default();
            e.0 += t.duration_ms;
            e.1 += 1;
        }
        for (file, ms) in per_file {
            let e = files.entry(file.to_string()).or_default();
            e.0 += ms;
            e.1 += 1;
        }
        gates.push(DurationRow {
            name: g.report.run_id.clone(),
            value: millis(g.report.tests.iter().map(|t| t.duration_ms).sum()),
        });
    }
    Durations {
        gates,
        files: averaged(files),
        tests: averaged(tests),
    }
}

// ------------------------------------------------------------------ decisions

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    pub ticket: String,
    pub decided_at: String,
    pub mode: &'static str,
    pub decided: String,
    pub effective: String,
    pub area: String,
    pub human_lane: String,
    pub human_area: String,
    pub agrees: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Decisions {
    pub rows: Vec<DecisionRow>,
    /// Agent lane decided and a human lane label present.
    pub agent_labeled: usize,
    /// ... of which the human also chose the agent lane.
    pub agent_confirmed: usize,
    pub precision: String,
}

fn lane(l: &Lane) -> String {
    match l {
        Lane::Agent => "agent".into(),
        Lane::Human(reasons) if reasons.is_empty() => "human".into(),
        Lane::Human(reasons) => {
            let names: Vec<String> = reasons
                .iter()
                .map(|r| {
                    serde_json::to_value(r)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default()
                })
                .collect();
            format!("human ({})", names.join(", "))
        }
    }
}

pub fn decisions(rows: &[CalibrationRow]) -> Decisions {
    let mut out = Decisions::default();
    for row in rows {
        let d = &row.decision;
        let human_lane = row.human.as_ref().and_then(|h| h.lane);
        let agrees = human_lane.map(|h| (h == LaneKind::Agent) == d.decided.is_agent());
        if d.decided.is_agent() && human_lane.is_some() {
            out.agent_labeled += 1;
            if human_lane == Some(LaneKind::Agent) {
                out.agent_confirmed += 1;
            }
        }
        out.rows.push(DecisionRow {
            ticket: d.ticket_key.clone(),
            decided_at: d.decided_at.clone(),
            mode: if d.mode.is_shadow() {
                "shadow"
            } else {
                "active"
            },
            decided: lane(&d.decided),
            effective: lane(&d.effective),
            area: d
                .answers
                .as_ref()
                .map_or("—".into(), |a| a.area.value.as_str().to_string()),
            human_lane: match human_lane {
                Some(LaneKind::Agent) => "agent".into(),
                Some(LaneKind::Human) => "human".into(),
                None => "—".into(),
            },
            human_area: row
                .human
                .as_ref()
                .and_then(|h| h.area.as_ref())
                .map_or("—".into(), |a| a.as_str().to_string()),
            agrees,
        });
    }
    out.precision = percent(out.agent_confirmed, out.agent_labeled);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_formatting() {
        assert_eq!(percent(1, 2), "50%");
        assert_eq!(percent(0, 0), "n/a");
        assert_eq!(percent(2, 3), "67%");
    }

    #[test]
    fn human_lane_lists_reasons() {
        use guard_core::ports::HumanReason;
        assert_eq!(lane(&Lane::Agent), "agent");
        assert_eq!(
            lane(&Lane::Human(vec![
                HumanReason::HardRulePath,
                HumanReason::ShadowMode
            ])),
            "human (hard_rule_path, shadow_mode)"
        );
    }

    #[test]
    fn averages_sort_slowest_first() {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), (1000, 1));
        m.insert("b".to_string(), (9000, 3));
        let rows = averaged(m);
        assert_eq!(rows[0].name, "b");
        assert_eq!(rows[0].value, "3.0 s");
    }
}
