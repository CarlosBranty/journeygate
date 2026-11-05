//! The 4-arm token A/B report (docs/contracts/tokens.md, "4-arm A/B
//! protocol"): `baseline`, `+memory`, `+map` and `both`, computed from
//! `agent_runs` and `spend_records` only.
//!
//! A trial is one (experiment, arm, task, repetition), identified by the
//! virtual-key alias `<experiment>:<arm>:<task>:<rep>` that both tables carry.
//! Per trial: resolved = some coder attempt ended `passed` (guard green with
//! the sealed spec); tokens and USD come from the alias's spend rows; steps
//! and memory reads are summed over its runs; citations are counted on the
//! final coder attempt (a brief fact is cited when one of its
//! `referenced_paths` is in that attempt's `diff_paths`). Runs that failed for
//! infrastructure reasons stay in as unresolved trials, never dropped.
//!
//! Confidence intervals are percentile bootstraps over tasks (all repetitions
//! of a task move together), deterministic for a given seed. The verdict rules
//! are the pre-registered thresholds of [`Preregistration::default`].
//!
//! Not covered (no column yet): `guard_map` reads and map-entry citations.

use crate::pg::map_sqlx_error;
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Baseline,
    Memory,
    Map,
    Both,
}

impl Arm {
    pub const ALL: [Arm; 4] = [Arm::Baseline, Arm::Memory, Arm::Map, Arm::Both];

    pub fn as_str(self) -> &'static str {
        match self {
            Arm::Baseline => "baseline",
            Arm::Memory => "memory",
            Arm::Map => "map",
            Arm::Both => "both",
        }
    }

    /// Accepts `baseline`, `memory`/`+memory`, `map`/`+map`, `both`.
    pub fn parse(name: &str) -> Option<Arm> {
        match name.trim().trim_start_matches('+') {
            "baseline" => Some(Arm::Baseline),
            "memory" => Some(Arm::Memory),
            "map" => Some(Arm::Map),
            "both" => Some(Arm::Both),
            _ => None,
        }
    }
}

/// The pre-registered analysis plan (tokens.md). Defaults are the contract
/// numbers; only `seed` (recorded in `preregistration.toml`) varies.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Preregistration {
    /// An arm may lose at most this many percentage points of resolution rate.
    pub max_resolution_drop_pp: f64,
    /// ... and must cut USD per resolved task by at least this fraction.
    pub min_cost_reduction: f64,
    /// `both` must beat the better single arm by this further fraction.
    pub min_interaction_reduction: f64,
    /// Fewer paired tasks than this: inconclusive.
    pub min_tasks: usize,
    pub bootstrap_resamples: u32,
    pub confidence: f64,
    pub seed: u64,
}

impl Default for Preregistration {
    fn default() -> Self {
        Self {
            max_resolution_drop_pp: 2.0,
            min_cost_reduction: 0.15,
            min_interaction_reduction: 0.05,
            min_tasks: 30,
            bootstrap_resamples: 10_000,
            confidence: 0.95,
            seed: 0x5eed_0000_0000_0001,
        }
    }
}

/// One `agent_runs` row (the columns the report uses).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AbRunRow {
    pub key_alias: String,
    pub role: String,
    pub attempt: i32,
    pub outcome: Option<String>,
    pub steps: Option<i64>,
    pub memory_reads: i64,
    pub facts_brief_ids: Vec<i64>,
    pub diff_paths: Vec<String>,
}

/// `spend_records` summed per key alias.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AbSpendRow {
    pub key_alias: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub spend_usd: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AbInputs {
    pub experiment: String,
    pub runs: Vec<AbRunRow>,
    pub spend: Vec<AbSpendRow>,
    /// `referenced_paths` of every fact id in the runs' briefs.
    pub fact_paths: BTreeMap<i64, Vec<String>>,
}

/// One (arm, task, repetition): the raw per-trial export of the report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trial {
    pub arm: Arm,
    pub task: String,
    pub rep: String,
    pub runs: u32,
    pub resolved: bool,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub usd: f64,
    pub steps: u64,
    pub memory_reads: u64,
    pub brief_facts: u64,
    pub cited_facts: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Interval {
    pub low: f64,
    pub high: f64,
}

/// A point estimate and its bootstrap CI; `None` when undefined (e.g. USD
/// per resolved with nothing resolved).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Metric {
    pub value: Option<f64>,
    pub ci: Option<Interval>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ArmMetrics {
    pub arm: Arm,
    pub trials: usize,
    pub tasks: usize,
    pub resolved: usize,
    pub total_usd: f64,
    pub total_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub resolution_rate: Metric,
    pub tokens_per_resolved: Metric,
    pub usd_per_resolved: Metric,
    pub mean_steps: Metric,
    /// `cached_tokens / prompt_tokens`.
    pub cache_hit_rate: Metric,
    /// `guard_history` calls per trial.
    pub mean_memory_reads: Metric,
    /// Cited brief facts / brief facts, on final coder attempts.
    pub cited_in_diff_share: Metric,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Wins,
    DoesNotWin,
    Inconclusive,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Wins => "wins",
            Verdict::DoesNotWin => "does not win",
            Verdict::Inconclusive => "inconclusive",
        }
    }
}

/// An arm against `baseline`, on the tasks both ran.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    pub arm: Arm,
    pub paired_tasks: usize,
    /// (arm − baseline) resolution rate, percentage points.
    pub resolution_delta_pp: Option<f64>,
    /// `1 − arm USD per resolved / baseline USD per resolved`.
    pub cost_reduction: Option<f64>,
    /// arm − baseline USD per resolved.
    pub cost_diff_usd: Option<f64>,
    pub cost_diff_ci: Option<Interval>,
    pub verdict: Verdict,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Interaction {
    /// The single arm with the lower USD per resolved.
    pub best_single: Option<Arm>,
    /// `1 − both / best single` USD per resolved.
    pub extra_reduction: Option<f64>,
    pub enable_both: bool,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AbReport {
    pub experiment: String,
    pub preregistration: Preregistration,
    /// Every arm, in [`Arm::ALL`] order (empty arms included).
    pub arms: Vec<ArmMetrics>,
    /// `memory`, `map`, `both` against `baseline`.
    pub comparisons: Vec<Comparison>,
    pub interaction: Interaction,
    pub trials: Vec<Trial>,
    /// Runs whose key alias is not `<experiment>:<arm>:<task>:<rep>`.
    pub unattributed_runs: usize,
}

// ---------------------------------------------------------------- trials

fn parse_alias<'a>(alias: &'a str, experiment: &str) -> Option<(Arm, &'a str, &'a str)> {
    let rest = alias.strip_prefix(experiment)?.strip_prefix(':')?;
    let (rest, rep) = rest.rsplit_once(':')?;
    let (arm, task) = rest.split_once(':')?;
    let arm = Arm::parse(arm)?;
    (!task.is_empty() && !rep.is_empty()).then_some((arm, task, rep))
}

fn build_trials(inputs: &AbInputs) -> (Vec<Trial>, usize) {
    let mut by_alias: BTreeMap<&str, Vec<&AbRunRow>> = BTreeMap::new();
    let mut unattributed = 0;
    for run in &inputs.runs {
        if parse_alias(&run.key_alias, &inputs.experiment).is_some() {
            by_alias
                .entry(run.key_alias.as_str())
                .or_default()
                .push(run);
        } else {
            unattributed += 1;
        }
    }
    let mut spend: BTreeMap<&str, (u64, u64, u64, u64, f64)> = BTreeMap::new();
    for s in &inputs.spend {
        let e = spend.entry(s.key_alias.as_str()).or_default();
        e.0 += s.prompt_tokens;
        e.1 += s.completion_tokens;
        e.2 += s.cached_tokens;
        e.3 += s.cache_creation_input_tokens;
        e.4 += s.spend_usd;
    }
    let mut trials = vec![];
    for (alias, runs) in by_alias {
        let Some((arm, task, rep)) = parse_alias(alias, &inputs.experiment) else {
            continue;
        };
        let coder: Vec<&&AbRunRow> = runs.iter().filter(|r| r.role == "coder").collect();
        let resolved = coder.iter().any(|r| r.outcome.as_deref() == Some("passed"));
        let (mut brief, mut cited) = (0, 0);
        if let Some(last) = coder.iter().max_by_key(|r| r.attempt) {
            let diff: BTreeSet<&str> = last.diff_paths.iter().map(String::as_str).collect();
            for id in &last.facts_brief_ids {
                brief += 1;
                let hit = inputs
                    .fact_paths
                    .get(id)
                    .is_some_and(|paths| paths.iter().any(|p| diff.contains(p.as_str())));
                if hit {
                    cited += 1;
                }
            }
        }
        let (prompt, completion, cached, created, usd) =
            spend.get(alias).copied().unwrap_or_default();
        trials.push(Trial {
            arm,
            task: task.to_string(),
            rep: rep.to_string(),
            runs: runs.len() as u32,
            resolved,
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_tokens: cached,
            cache_creation_input_tokens: created,
            usd,
            steps: runs
                .iter()
                .map(|r| r.steps.unwrap_or(0).max(0) as u64)
                .sum(),
            memory_reads: runs.iter().map(|r| r.memory_reads.max(0) as u64).sum(),
            brief_facts: brief,
            cited_facts: cited,
        });
    }
    trials.sort_by(|a, b| (a.arm, &a.task, &a.rep).cmp(&(b.arm, &b.task, &b.rep)));
    (trials, unattributed)
}

// ---------------------------------------------------------------- statistics

#[derive(Debug, Clone, Copy, Default)]
struct Sums {
    trials: f64,
    resolved: f64,
    tokens: f64,
    usd: f64,
    steps: f64,
    prompt: f64,
    cached: f64,
    reads: f64,
    brief: f64,
    cited: f64,
}

const METRICS: usize = 7;

impl Sums {
    fn add(&mut self, t: &Trial) {
        self.trials += 1.0;
        self.resolved += f64::from(u8::from(t.resolved));
        self.tokens += (t.prompt_tokens + t.completion_tokens) as f64;
        self.usd += t.usd;
        self.steps += t.steps as f64;
        self.prompt += t.prompt_tokens as f64;
        self.cached += t.cached_tokens as f64;
        self.reads += t.memory_reads as f64;
        self.brief += t.brief_facts as f64;
        self.cited += t.cited_facts as f64;
    }

    fn of<'a>(trials: impl IntoIterator<Item = &'a Trial>) -> Self {
        let mut s = Self::default();
        for t in trials {
            s.add(t);
        }
        s
    }

    fn ratio(a: f64, b: f64) -> Option<f64> {
        (b > 0.0).then(|| a / b)
    }

    fn usd_per_resolved(&self) -> Option<f64> {
        Self::ratio(self.usd, self.resolved)
    }

    fn resolution_rate(&self) -> Option<f64> {
        Self::ratio(self.resolved, self.trials)
    }

    /// resolution, tokens/resolved, USD/resolved, steps, cache hit, reads, cited.
    fn metrics(&self) -> [Option<f64>; METRICS] {
        [
            self.resolution_rate(),
            Self::ratio(self.tokens, self.resolved),
            self.usd_per_resolved(),
            Self::ratio(self.steps, self.trials),
            Self::ratio(self.cached, self.prompt),
            Self::ratio(self.reads, self.trials),
            Self::ratio(self.cited, self.brief),
        ]
    }
}

/// SplitMix64: small, fast, deterministic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Percentile interval of `samples` at `confidence`; `None` when fewer than
/// half the resamples produced a defined value.
fn interval(mut samples: Vec<f64>, resamples: u32, confidence: f64) -> Option<Interval> {
    if samples.is_empty() || samples.len() * 2 < resamples as usize {
        return None;
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let alpha = (1.0 - confidence) / 2.0;
    let at = |p: f64| samples[((samples.len() - 1) as f64 * p).round() as usize];
    Some(Interval {
        low: at(alpha),
        high: at(1.0 - alpha),
    })
}

/// Trials of `arm` grouped by task, tasks in sorted order.
fn by_task(trials: &[Trial], arm: Arm) -> BTreeMap<&str, Vec<&Trial>> {
    let mut out: BTreeMap<&str, Vec<&Trial>> = BTreeMap::new();
    for t in trials.iter().filter(|t| t.arm == arm) {
        out.entry(t.task.as_str()).or_default().push(t);
    }
    out
}

fn arm_metrics(trials: &[Trial], arm: Arm, p: &Preregistration, rng: &mut Rng) -> ArmMetrics {
    let tasks: Vec<Vec<&Trial>> = by_task(trials, arm).into_values().collect();
    let all = Sums::of(tasks.iter().flatten().copied());
    let point = all.metrics();
    let mut samples: [Vec<f64>; METRICS] = Default::default();
    if !tasks.is_empty() {
        for _ in 0..p.bootstrap_resamples {
            let mut s = Sums::default();
            for _ in 0..tasks.len() {
                for t in &tasks[rng.below(tasks.len())] {
                    s.add(t);
                }
            }
            for (i, v) in s.metrics().into_iter().enumerate() {
                if let Some(v) = v {
                    samples[i].push(v);
                }
            }
        }
    }
    let mut metric = samples
        .into_iter()
        .zip(point)
        .map(|(samples, value)| Metric {
            value,
            ci: value.and_then(|_| interval(samples, p.bootstrap_resamples, p.confidence)),
        });
    let mut next = || metric.next().expect("METRICS metrics");
    let members: Vec<&Trial> = tasks.iter().flatten().copied().collect();
    ArmMetrics {
        arm,
        trials: members.len(),
        tasks: tasks.len(),
        resolved: members.iter().filter(|t| t.resolved).count(),
        total_usd: all.usd,
        total_tokens: members
            .iter()
            .map(|t| t.prompt_tokens + t.completion_tokens)
            .sum(),
        cache_creation_input_tokens: members.iter().map(|t| t.cache_creation_input_tokens).sum(),
        resolution_rate: next(),
        tokens_per_resolved: next(),
        usd_per_resolved: next(),
        mean_steps: next(),
        cache_hit_rate: next(),
        mean_memory_reads: next(),
        cited_in_diff_share: next(),
    }
}

fn compare(trials: &[Trial], arm: Arm, p: &Preregistration, rng: &mut Rng) -> Comparison {
    let base = by_task(trials, Arm::Baseline);
    let other = by_task(trials, arm);
    let paired: Vec<(&Vec<&Trial>, &Vec<&Trial>)> = base
        .iter()
        .filter_map(|(task, b)| other.get(task).map(|o| (b, o)))
        .collect();
    let n = paired.len();
    let b_all = Sums::of(paired.iter().flat_map(|(b, _)| b.iter().copied()));
    let o_all = Sums::of(paired.iter().flat_map(|(_, o)| o.iter().copied()));
    let delta_pp = match (o_all.resolution_rate(), b_all.resolution_rate()) {
        (Some(o), Some(b)) => Some((o - b) * 100.0),
        _ => None,
    };
    let (o_usd, b_usd) = (o_all.usd_per_resolved(), b_all.usd_per_resolved());
    let cost_reduction = match (o_usd, b_usd) {
        (Some(o), Some(b)) if b > 0.0 => Some(1.0 - o / b),
        _ => None,
    };
    let cost_diff = o_usd.zip(b_usd).map(|(o, b)| o - b);

    let mut diffs = vec![];
    if n > 0 {
        for _ in 0..p.bootstrap_resamples {
            let (mut sb, mut so) = (Sums::default(), Sums::default());
            for _ in 0..n {
                let (b, o) = paired[rng.below(n)];
                b.iter().for_each(|t| sb.add(t));
                o.iter().for_each(|t| so.add(t));
            }
            if let (Some(o), Some(b)) = (so.usd_per_resolved(), sb.usd_per_resolved()) {
                diffs.push(o - b);
            }
        }
    }
    let ci = cost_diff.and_then(|_| interval(diffs, p.bootstrap_resamples, p.confidence));

    let mut reasons = vec![];
    let verdict = if n == 0 {
        reasons.push(format!(
            "no tasks ran in both baseline and {}",
            arm.as_str()
        ));
        Verdict::Inconclusive
    } else if n < p.min_tasks {
        reasons.push(format!("fewer than {} paired tasks ({n})", p.min_tasks));
        Verdict::Inconclusive
    } else if b_usd.is_none() || o_usd.is_none() {
        reasons.push("USD per resolved task is undefined (nothing resolved)".to_string());
        Verdict::Inconclusive
    } else {
        let mut wins = true;
        if let Some(d) = delta_pp.filter(|d| *d < -p.max_resolution_drop_pp) {
            wins = false;
            reasons.push(format!(
                "resolution rate dropped {:.1} pp (more than {} pp allowed)",
                -d, p.max_resolution_drop_pp
            ));
        }
        if let Some(r) = cost_reduction.filter(|r| *r < p.min_cost_reduction) {
            wins = false;
            reasons.push(format!(
                "USD per resolved task fell {:.1}% (at least {:.0}% required)",
                r * 100.0,
                p.min_cost_reduction * 100.0
            ));
        }
        match ci {
            Some(ci) if ci.high < 0.0 => {}
            Some(ci) => {
                wins = false;
                reasons.push(format!(
                    "{:.0}% CI of the cost difference [{:.4}, {:.4}] does not exclude 0",
                    p.confidence * 100.0,
                    ci.low,
                    ci.high
                ));
            }
            None => {
                wins = false;
                reasons.push("no bootstrap CI for the cost difference".to_string());
            }
        }
        if wins {
            Verdict::Wins
        } else {
            Verdict::DoesNotWin
        }
    };
    Comparison {
        arm,
        paired_tasks: n,
        resolution_delta_pp: delta_pp,
        cost_reduction,
        cost_diff_usd: cost_diff,
        cost_diff_ci: ci,
        verdict,
        reasons,
    }
}

fn interaction(
    arms: &[ArmMetrics],
    comparisons: &[Comparison],
    p: &Preregistration,
) -> Interaction {
    let usd = |a: Arm| {
        arms.iter()
            .find(|m| m.arm == a)
            .and_then(|m| m.usd_per_resolved.value)
    };
    let best_single = [Arm::Memory, Arm::Map]
        .into_iter()
        .filter_map(|a| usd(a).map(|v| (a, v)))
        .min_by(|x, y| x.1.total_cmp(&y.1));
    let extra = match (best_single, usd(Arm::Both)) {
        (Some((_, best)), Some(both)) if best > 0.0 => Some(1.0 - both / best),
        _ => None,
    };
    let both_wins = comparisons
        .iter()
        .any(|c| c.arm == Arm::Both && c.verdict == Verdict::Wins);
    let (enable_both, reason) = match extra {
        None => (
            false,
            "not computable (an arm has no resolved task)".to_string(),
        ),
        Some(_) if !both_wins => (false, "both does not win over baseline".to_string()),
        Some(e) if e < p.min_interaction_reduction => (
            false,
            format!(
                "both is only {:.1}% cheaper than the better single arm (at least {:.0}% required)",
                e * 100.0,
                p.min_interaction_reduction * 100.0
            ),
        ),
        Some(e) => (
            true,
            format!(
                "both is {:.1}% cheaper than the better single arm",
                e * 100.0
            ),
        ),
    };
    Interaction {
        best_single: best_single.map(|(a, _)| a),
        extra_reduction: extra,
        enable_both,
        reason,
    }
}

/// Computes the report. Deterministic for the same inputs and seed.
pub fn compute_ab_report(inputs: &AbInputs, p: &Preregistration) -> AbReport {
    let (trials, unattributed_runs) = build_trials(inputs);
    let mut rng = Rng(p.seed);
    let arms: Vec<ArmMetrics> = Arm::ALL
        .into_iter()
        .map(|a| arm_metrics(&trials, a, p, &mut rng))
        .collect();
    let comparisons: Vec<Comparison> = [Arm::Memory, Arm::Map, Arm::Both]
        .into_iter()
        .map(|a| compare(&trials, a, p, &mut rng))
        .collect();
    let interaction = interaction(&arms, &comparisons, p);
    AbReport {
        experiment: inputs.experiment.clone(),
        preregistration: p.clone(),
        arms,
        comparisons,
        interaction,
        trials,
        unattributed_runs,
    }
}

// ---------------------------------------------------------------- rendering

fn fmt_metric(m: &Metric, digits: usize) -> String {
    match (m.value, m.ci) {
        (None, _) => "n/a".to_string(),
        (Some(v), None) => format!("{v:.digits$}"),
        (Some(v), Some(ci)) => format!("{v:.digits$} [{:.digits$}, {:.digits$}]", ci.low, ci.high),
    }
}

fn fmt_opt(v: Option<f64>, digits: usize, scale: f64, suffix: &str) -> String {
    v.map(|v| format!("{:.digits$}{suffix}", v * scale))
        .unwrap_or_else(|| "n/a".to_string())
}

/// A Markdown summary: per-arm metrics with CIs, the comparisons and the
/// interaction decision.
pub fn render_markdown(r: &AbReport) -> String {
    let p = &r.preregistration;
    let mut out = format!(
        "# Token A/B: {}\n\n{} trials; {} unattributed runs. {:.0}% bootstrap CIs over tasks ({} resamples, seed {}).\n\n",
        r.experiment,
        r.trials.len(),
        r.unattributed_runs,
        p.confidence * 100.0,
        p.bootstrap_resamples,
        p.seed
    );
    out.push_str(
        "| arm | trials | tasks | resolution rate | tokens / resolved | USD / resolved | steps | cache hit rate | memory reads | cited-in-diff |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for m in &r.arms {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            m.arm.as_str(),
            m.trials,
            m.tasks,
            fmt_metric(&m.resolution_rate, 3),
            fmt_metric(&m.tokens_per_resolved, 0),
            fmt_metric(&m.usd_per_resolved, 4),
            fmt_metric(&m.mean_steps, 1),
            fmt_metric(&m.cache_hit_rate, 3),
            fmt_metric(&m.mean_memory_reads, 2),
            fmt_metric(&m.cited_in_diff_share, 3),
        ));
    }
    out.push_str(
        "\n| arm vs baseline | paired tasks | resolution delta (pp) | cost reduction | cost diff CI (USD) | verdict |\n\
         | --- | --- | --- | --- | --- | --- |\n",
    );
    for c in &r.comparisons {
        let ci = c
            .cost_diff_ci
            .map(|ci| format!("[{:.4}, {:.4}]", ci.low, ci.high))
            .unwrap_or_else(|| "n/a".to_string());
        out.push_str(&format!(
            "| {} | {} | {} | {} | {ci} | {} |\n",
            c.arm.as_str(),
            c.paired_tasks,
            fmt_opt(c.resolution_delta_pp, 1, 1.0, ""),
            fmt_opt(c.cost_reduction, 1, 100.0, "%"),
            c.verdict.as_str(),
        ));
    }
    for c in r.comparisons.iter().filter(|c| !c.reasons.is_empty()) {
        out.push_str(&format!("\n- {}: {}", c.arm.as_str(), c.reasons.join("; ")));
    }
    out.push_str(&format!(
        "\n\nInteraction: enable both = {} ({}).\n",
        r.interaction.enable_both, r.interaction.reason
    ));
    out
}

// ---------------------------------------------------------------- loading

fn u64_of(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// Reads the experiment's `agent_runs`, its `spend_records` (summed per key
/// alias) and the `referenced_paths` of every brief fact.
pub async fn load_ab_inputs(
    pool: &PgPool,
    experiment: &str,
) -> Result<AbInputs, guard_core::ports::StoreError> {
    let prefix = format!("{experiment}:");
    let runs = sqlx::query(
        "SELECT key_alias, role, attempt, outcome, steps::bigint AS steps,
                memory_reads::bigint AS memory_reads, facts_brief_ids, diff_paths
         FROM agent_runs WHERE starts_with(key_alias, $1) ORDER BY id",
    )
    .bind(&prefix)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)?
    .iter()
    .map(|row| {
        Ok(AbRunRow {
            key_alias: row.try_get("key_alias")?,
            role: row.try_get("role")?,
            attempt: row.try_get("attempt")?,
            outcome: row.try_get("outcome")?,
            steps: row.try_get("steps")?,
            memory_reads: row.try_get("memory_reads")?,
            facts_brief_ids: row.try_get("facts_brief_ids")?,
            diff_paths: row.try_get("diff_paths")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_sqlx_error)?;

    let spend = sqlx::query(
        "SELECT key_alias, sum(prompt_tokens)::bigint AS p, sum(completion_tokens)::bigint AS c,
                sum(cached_tokens)::bigint AS ca, sum(cache_creation_input_tokens)::bigint AS cc,
                sum(spend_usd)::float8 AS usd
         FROM spend_records WHERE starts_with(key_alias, $1)
         GROUP BY key_alias ORDER BY key_alias",
    )
    .bind(&prefix)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx_error)?
    .iter()
    .map(|row| {
        Ok(AbSpendRow {
            key_alias: row.try_get("key_alias")?,
            prompt_tokens: u64_of(row.try_get("p")?),
            completion_tokens: u64_of(row.try_get("c")?),
            cached_tokens: u64_of(row.try_get("ca")?),
            cache_creation_input_tokens: u64_of(row.try_get("cc")?),
            spend_usd: row.try_get("usd")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_sqlx_error)?;

    let ids: Vec<i64> = runs
        .iter()
        .flat_map(|r| r.facts_brief_ids.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut fact_paths = BTreeMap::new();
    if !ids.is_empty() {
        let rows = sqlx::query("SELECT id, referenced_paths FROM memory_facts WHERE id = ANY($1)")
            .bind(&ids)
            .fetch_all(pool)
            .await
            .map_err(map_sqlx_error)?;
        for row in rows {
            let id: i64 = row.try_get("id").map_err(map_sqlx_error)?;
            let paths: Vec<String> = row.try_get("referenced_paths").map_err(map_sqlx_error)?;
            fact_paths.insert(id, paths);
        }
    }
    Ok(AbInputs {
        experiment: experiment.to_string(),
        runs,
        spend,
        fact_paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_split_into_arm_task_and_rep() {
        assert_eq!(
            parse_alias("exp:+memory:SHOP-1:2", "exp"),
            Some((Arm::Memory, "SHOP-1", "2"))
        );
        // Tasks may contain colons; the repetition is the last part.
        assert_eq!(
            parse_alias("exp:map:swe:slice:3:0", "exp"),
            Some((Arm::Map, "swe:slice:3", "0"))
        );
        assert_eq!(parse_alias("exp2:map:t:0", "exp"), None);
        assert_eq!(parse_alias("exp:nope:t:0", "exp"), None);
        assert_eq!(parse_alias("exp:map:0", "exp"), None);
    }

    #[test]
    fn the_rng_is_deterministic() {
        let (mut a, mut b) = (Rng(7), Rng(7));
        assert_eq!((a.next(), a.next()), (b.next(), b.next()));
        assert!((0..1000).all(|_| a.below(5) < 5));
    }

    #[test]
    fn intervals_need_enough_defined_resamples() {
        assert_eq!(interval(vec![1.0], 10, 0.95), None);
        let ci = interval((0..100).map(f64::from).collect(), 100, 0.9).unwrap();
        assert_eq!((ci.low, ci.high), (5.0, 94.0));
    }
}
