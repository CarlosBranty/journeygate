//! The orchestrator's fact writers (docs/contracts/memory.md, "Writers") and
//! the agent-proposal candidate queue.
//!
//! Every writer turns an orchestrator observation into [`NewFact`]s and goes
//! through [`observe`]: re-observing a live claim keeps it instead of
//! recording a new version, so writers may replay; a `needs_review` claim
//! only goes back to `active` when it is seen again at a newer commit (as a
//! new version at that commit), never by re-reading the old evidence. A fact
//! the store rejects as invalid (e.g. a non-hex commit) is reported in
//! [`WriteOutcome::skipped`]; only infrastructure errors abort a writer.
//!
//! Agents never call these. Their `result.json` `memory_proposals` go through
//! [`enqueue_proposals`] into `memory_fact_candidates`, and only a human
//! approval (`MemoryFactsWriter::approve_candidate`) turns one into a fact.

use crate::flaky::FlakyStat;
use crate::util::{format_ts, lane_kind_name, parse_ts};
use guard_core::fingerprint::{Fingerprint, normalize_error};
use guard_core::ports::memory_facts::{MAX_FACT_BODY_BYTES, MAX_TOPIC_KEY_LEN};
use guard_core::ports::{
    CalibrationRow, CandidateId, EvidenceKind, FactCandidate, FactId, FactScope, FactSource,
    FactStatus, GateReport, GateTest, Lane, LaneKind, MemoryFactsWriter, MemoryProposal, NewFact,
    StoreError, StoredGateResult, TestStatus,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// `flaky/` facts are due for review this long after they are recorded.
pub const FLAKY_REVIEW_DAYS: i64 = 14;
/// A fingerprint becomes a `fingerprint/` fact once seen in this many runs.
pub const MIN_FINGERPRINT_RUNS: usize = 2;
/// Proposals beyond this many per agent result are rejected.
pub const MAX_PROPOSALS_PER_RUN: usize = 10;

/// What a writer did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Facts recorded or re-observed, in writing order.
    pub recorded: Vec<FactId>,
    /// Observations that produced no fact, with the reason.
    pub skipped: Vec<String>,
}

impl WriteOutcome {
    async fn write(
        &mut self,
        writer: &dyn MemoryFactsWriter,
        fact: NewFact,
    ) -> Result<(), StoreError> {
        match observe(writer, &fact).await {
            Ok(id) => self.recorded.push(id),
            Err(StoreError::Invalid(why)) => {
                self.skipped.push(format!("{}: {why}", fact.topic_key));
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }
}

/// Records `fact`, unless the live version of its topic already makes the
/// same claim (`body`):
///
/// - live `active`: that version is kept (and confirmed with
///   `fact.review_after` when given) and its id returned;
/// - live `needs_review`, observed again at the same `commit_sha`: nothing
///   changes. Re-reading old evidence (the recent gate results still holding
///   the runs the fact came from) is not a re-observation, so the flag stays
///   until a human confirms it;
/// - live `needs_review`, observed at another (the writers' newest) commit:
///   recorded as a new `active` version at that commit, superseding the
///   flagged one, so staleness measures from the new commit (memory.md:
///   "a writer re-observing it at a newer commit").
pub async fn observe(writer: &dyn MemoryFactsWriter, fact: &NewFact) -> Result<FactId, StoreError> {
    fact.validate().map_err(StoreError::Invalid)?;
    let live = writer
        .topic_history(&fact.repo, &fact.topic_key)
        .await?
        .into_iter()
        .find(|f| {
            f.status != FactStatus::Invalidated
                && f.scope == fact.scope
                && f.scope_key == fact.scope_key
        });
    match live {
        Some(f) if f.body == fact.body && f.status == FactStatus::NeedsReview => {
            if f.commit_sha == fact.commit_sha {
                Ok(f.id)
            } else {
                writer.record_fact(fact).await
            }
        }
        Some(f) if f.body == fact.body => {
            if fact.review_after.is_some() {
                writer.confirm(f.id, fact.review_after.as_deref()).await?;
            }
            Ok(f.id)
        }
        _ => writer.record_fact(fact).await,
    }
}

// ---------------------------------------------------------------- helpers

/// `prefix` + a topic-key-safe form of `raw`: lowercase, runs of characters
/// outside `[a-z0-9._/-]` become one `-`; when that changed anything (or the
/// result is too long) an 8-hex SHA-256 suffix of `raw` keeps distinct inputs
/// distinct. `prefix` must itself be a valid topic start (`"flaky/"`).
pub fn topic_segment(prefix: &str, raw: &str) -> String {
    let mut slug = String::with_capacity(raw.len());
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() || "._/".contains(c) {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let exact = slug == raw && !slug.is_empty();
    let budget = MAX_TOPIC_KEY_LEN - prefix.len();
    if exact && slug.len() <= budget {
        return format!("{prefix}{slug}");
    }
    let digest = Sha256::digest(raw.as_bytes());
    let hash = hex::encode(&digest[..4]);
    let keep = budget.saturating_sub(hash.len() + 1);
    let head: String = slug.chars().take(keep).collect();
    let head = head.trim_end_matches('-');
    if head.is_empty() {
        format!("{prefix}{hash}")
    } else {
        format!("{prefix}{head}-{hash}")
    }
}

/// `text` cut at a char boundary to at most `max` bytes.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

fn body(text: &str) -> String {
    clip(text.trim(), MAX_FACT_BODY_BYTES)
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn scope_of(journey: Option<&str>) -> (FactScope, Option<String>) {
    match journey {
        Some(j) if !j.trim().is_empty() => (FactScope::Journey, Some(j.to_string())),
        _ => (FactScope::Repo, None),
    }
}

fn fingerprint_topic(prefix: &str, fp: &Fingerprint) -> String {
    topic_segment(prefix, &fp.label())
}

fn evidence_uri(test: &GateTest, base: Option<&str>) -> Option<String> {
    let base = base?.trim_end_matches('/');
    let files = &test.evidence.as_ref()?.files;
    let pick = |kind: EvidenceKind| {
        files
            .iter()
            .find(|f| f.kind == kind)
            .and_then(|f| f.key.as_deref())
    };
    pick(EvidenceKind::Trace)
        .or_else(|| pick(EvidenceKind::Screenshot))
        .map(|key| format!("{base}/{key}"))
}

/// The file of a `<project>::<file>::<title>` test id.
fn test_file(test_id: &str) -> Option<&str> {
    let mut parts = test_id.splitn(3, "::");
    let _project = parts.next()?;
    let file = parts.next()?;
    parts.next()?;
    (!file.is_empty()).then_some(file)
}

// ---------------------------------------------------------------- writers

/// Gate results: every blocking failure with a fingerprint becomes
/// `failure/<fingerprint label>` on its journey (or the repo): what failed,
/// where (file:line), with the trace (else screenshot) as `evidence_uri`
/// (`<evidence_base>/<key>`, e.g. `s3://guard-evidence`).
pub async fn facts_from_gate_result(
    writer: &dyn MemoryFactsWriter,
    report: &GateReport,
    evidence_base: Option<&str>,
) -> Result<WriteOutcome, StoreError> {
    let mut out = WriteOutcome::default();
    let Some(repo) = report.repo.as_deref().filter(|r| !r.is_empty()) else {
        out.skipped
            .push(format!("gate result {} has no repo", report.run_id));
        return Ok(out);
    };
    for test in &report.tests {
        if test.status != TestStatus::Failed || !test.blocking {
            continue;
        }
        let Some(fp) = &test.fingerprint else {
            out.skipped
                .push(format!("{}: failure without fingerprint", test.test_id));
            continue;
        };
        let location = test
            .evidence
            .as_ref()
            .and_then(|e| e.location.clone())
            .unwrap_or_else(|| format!("{}:{}", test.file, test.line));
        let error = test
            .error
            .as_deref()
            .or(test.evidence.as_ref().map(|e| e.error.as_str()))
            .map(first_line)
            .unwrap_or("no error message");
        let (scope, scope_key) = scope_of(test.journey.as_deref());
        let fact = NewFact {
            repo: repo.to_string(),
            scope,
            scope_key,
            topic_key: fingerprint_topic("failure/", fp),
            body: body(&format!(
                "`{}` failed at {location} ({}): {error}",
                test.title,
                fp.label()
            )),
            commit_sha: report.head_sha.clone(),
            run_id: Some(report.run_id.clone()),
            evidence_uri: evidence_uri(test, evidence_base),
            source: FactSource::GateResult,
            valid_from: report.generated_at.clone(),
            review_after: None,
            referenced_paths: vec![test.file.clone()],
        };
        out.write(writer, fact).await?;
    }
    Ok(out)
}

/// Failure fingerprints: a fingerprint of `repo` seen in at least `min_runs`
/// of `results` (newest first, as `GateHistory::recent_gate_results` returns
/// them) becomes `fingerprint/<label>` with the normalized error, observed at
/// its newest run.
pub async fn facts_from_fingerprints(
    writer: &dyn MemoryFactsWriter,
    repo: &str,
    results: &[StoredGateResult],
    min_runs: usize,
) -> Result<WriteOutcome, StoreError> {
    struct Seen<'a> {
        runs: Vec<&'a GateReport>,
        test: &'a GateTest,
    }
    let mut seen: BTreeMap<(Option<&str>, &Fingerprint), Seen<'_>> = BTreeMap::new();
    for result in results
        .iter()
        .filter(|r| r.report.repo.as_deref() == Some(repo))
    {
        for test in &result.report.tests {
            let Some(fp) = &test.fingerprint else {
                continue;
            };
            if !matches!(test.status, TestStatus::Failed | TestStatus::Flaky) {
                continue;
            }
            let entry = seen
                .entry((test.journey.as_deref(), fp))
                .or_insert_with(|| Seen { runs: vec![], test });
            if !entry.runs.iter().any(|r| r.run_id == result.report.run_id) {
                entry.runs.push(&result.report);
            }
        }
    }
    let mut out = WriteOutcome::default();
    for ((journey, fp), s) in seen {
        if s.runs.len() < min_runs.max(1) {
            continue;
        }
        let newest = s.runs[0];
        let error = s
            .test
            .error
            .as_deref()
            .or(s.test.evidence.as_ref().map(|e| e.error.as_str()))
            .unwrap_or("");
        let (scope, scope_key) = scope_of(journey);
        let fact = NewFact {
            repo: repo.to_string(),
            scope,
            scope_key,
            topic_key: fingerprint_topic("fingerprint/", fp),
            body: body(&format!(
                "Recurring failure {} in `{}`: {}",
                fp.label(),
                s.test.test_id,
                normalize_error(error)
            )),
            commit_sha: newest.head_sha.clone(),
            run_id: Some(newest.run_id.clone()),
            evidence_uri: None,
            source: FactSource::FailureFingerprint,
            valid_from: newest.generated_at.clone(),
            review_after: None,
            referenced_paths: vec![s.test.file.clone()],
        };
        out.write(writer, fact).await?;
    }
    Ok(out)
}

/// Flaky history: tests with `flaky_rate >= min_flaky_rate` become
/// `flaky/<test id>` on their journey (or the repo), due for review
/// [`FLAKY_REVIEW_DAYS`] after `now`. `commit_sha` is the default branch head.
pub async fn facts_from_flaky(
    writer: &dyn MemoryFactsWriter,
    repo: &str,
    commit_sha: &str,
    stats: &[FlakyStat],
    now: &str,
    min_flaky_rate: f64,
) -> Result<WriteOutcome, StoreError> {
    let now_ts = parse_ts("now", now)?;
    let review_after = format_ts(now_ts + time::Duration::days(FLAKY_REVIEW_DAYS));
    let mut out = WriteOutcome::default();
    for stat in stats
        .iter()
        .filter(|s| s.flaky > 0 && s.flaky_rate >= min_flaky_rate)
    {
        let (scope, scope_key) = scope_of(stat.journey.as_deref());
        let fact = NewFact {
            repo: repo.to_string(),
            scope,
            scope_key,
            topic_key: topic_segment("flaky/", &stat.test_id),
            body: body(&format!(
                "`{}` is flaky in recent gate runs (flaky rate >= {:.0}%); expect intermittent \
                 failures and prefer web-first assertions over fixed waits.",
                stat.test_id,
                min_flaky_rate * 100.0
            )),
            commit_sha: commit_sha.to_string(),
            run_id: None,
            evidence_uri: None,
            source: FactSource::FlakyHistory,
            valid_from: format_ts(now_ts),
            review_after: Some(review_after.clone()),
            referenced_paths: test_file(&stat.test_id)
                .map(|f| vec![f.to_string()])
                .unwrap_or_default(),
        };
        out.write(writer, fact).await?;
    }
    Ok(out)
}

/// A merged PR that turned a recorded failure green.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixCommit {
    pub repo: String,
    /// `[areas]` key of the fix.
    pub area: String,
    pub fingerprint: Fingerprint,
    pub commit_sha: String,
    /// RFC 3339.
    pub merged_at: String,
    pub touched_files: Vec<String>,
    pub pr: Option<u64>,
    pub run_id: Option<String>,
    pub evidence_uri: Option<String>,
}

/// Fix commits: `area:<key>` `fix/<fingerprint label>` naming the commit and
/// its touched files, which become `referenced_paths` (sorted, deduplicated).
pub fn fact_from_fix_commit(fix: &FixCommit) -> NewFact {
    let mut files = fix.touched_files.clone();
    files.sort();
    files.dedup();
    let pr = fix.pr.map(|n| format!(" (PR #{n})")).unwrap_or_default();
    NewFact {
        repo: fix.repo.clone(),
        scope: FactScope::Area,
        scope_key: Some(fix.area.clone()),
        topic_key: fingerprint_topic("fix/", &fix.fingerprint),
        body: body(&format!(
            "{} was fixed by {}{pr}, touching {}",
            fix.fingerprint.label(),
            short(&fix.commit_sha),
            if files.is_empty() {
                "no files".to_string()
            } else {
                files.join(", ")
            }
        )),
        commit_sha: fix.commit_sha.clone(),
        run_id: fix.run_id.clone(),
        evidence_uri: fix.evidence_uri.clone(),
        source: FactSource::FixCommit,
        valid_from: fix.merged_at.clone(),
        review_after: None,
        referenced_paths: files,
    }
}

/// Approved decisions: when the latest human label of a ticket overrides its
/// triage decision (lane, area or kind differ), a `repo` `decision/<ticket>`
/// fact. `commit_sha` is the default branch head when the label was seen.
pub fn fact_from_label_override(
    row: &CalibrationRow,
    repo: &str,
    commit_sha: &str,
) -> Option<NewFact> {
    let human = row.human.as_ref()?;
    let decision = &row.decision;
    let decided_lane = match decision.effective {
        Lane::Agent => LaneKind::Agent,
        Lane::Human(_) => LaneKind::Human,
    };
    let mut overrides = vec![];
    if let Some(lane) = human.lane.filter(|l| *l != decided_lane) {
        overrides.push(format!(
            "lane {} (triage chose {})",
            lane_kind_name(lane),
            lane_kind_name(decided_lane)
        ));
    }
    if let Some(answers) = &decision.answers {
        if let Some(area) = human.area.as_ref().filter(|a| **a != answers.area.value) {
            overrides.push(format!(
                "area {} (triage chose {})",
                area.0, answers.area.value.0
            ));
        }
        if let Some(kind) = human.kind.filter(|k| *k != answers.kind.value) {
            overrides.push(format!(
                "kind {} (triage chose {})",
                kind.as_str(),
                answers.kind.value.as_str()
            ));
        }
    }
    if overrides.is_empty() {
        return None;
    }
    Some(NewFact {
        repo: repo.to_string(),
        scope: FactScope::Repo,
        scope_key: None,
        topic_key: topic_segment("decision/", &decision.ticket_key.to_ascii_lowercase()),
        body: body(&format!(
            "{}: a human set {}",
            decision.ticket_key,
            overrides.join("; ")
        )),
        commit_sha: commit_sha.to_string(),
        run_id: None,
        evidence_uri: None,
        source: FactSource::ApprovedDecision,
        valid_from: human.labeled_at.clone(),
        review_after: None,
        referenced_paths: vec![],
    })
}

// ---------------------------------------------------------------- proposals

/// Where an agent's proposals are about, set by the orchestrator (never by
/// the agent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalContext {
    pub repo: String,
    pub scope: FactScope,
    pub scope_key: Option<String>,
    /// The attempt's base commit.
    pub commit_sha: String,
    pub run_id: Option<String>,
    /// RFC 3339; when the attempt finished.
    pub valid_from: String,
    /// `<runtime>:<role>`.
    pub proposed_by: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposalOutcome {
    pub enqueued: Vec<CandidateId>,
    /// `(topic_key, reason)` of proposals that were not enqueued.
    pub rejected: Vec<(String, String)>,
}

/// Stores an agent result's `memory_proposals` as candidates for human
/// approval. Evidence that is a URI (`s3://...`) becomes `evidence_uri`;
/// free-text evidence is appended to the body when it fits. Invalid
/// proposals and those beyond [`MAX_PROPOSALS_PER_RUN`] are rejected one by
/// one.
pub async fn enqueue_proposals(
    writer: &dyn MemoryFactsWriter,
    ctx: &ProposalContext,
    proposals: &[MemoryProposal],
) -> Result<ProposalOutcome, StoreError> {
    let mut out = ProposalOutcome::default();
    for p in proposals {
        if out.enqueued.len() >= MAX_PROPOSALS_PER_RUN {
            out.rejected.push((
                p.topic_key.clone(),
                format!("more than {MAX_PROPOSALS_PER_RUN} proposals in one result"),
            ));
            continue;
        }
        let evidence = p
            .evidence
            .as_deref()
            .map(str::trim)
            .filter(|e| !e.is_empty());
        let (evidence_uri, text) = match evidence {
            Some(e) if e.contains("://") && !e.contains(char::is_whitespace) => {
                (Some(e.to_string()), p.body.clone())
            }
            Some(e) => {
                let with = format!("{} (evidence: {e})", p.body.trim());
                let text = if with.len() <= MAX_FACT_BODY_BYTES {
                    with
                } else {
                    p.body.clone()
                };
                (None, text)
            }
            None => (None, p.body.clone()),
        };
        let fact = NewFact {
            repo: ctx.repo.clone(),
            scope: ctx.scope,
            scope_key: ctx.scope_key.clone(),
            topic_key: p.topic_key.clone(),
            body: text,
            commit_sha: ctx.commit_sha.clone(),
            run_id: ctx.run_id.clone(),
            evidence_uri,
            source: FactSource::ApprovedCandidate,
            valid_from: ctx.valid_from.clone(),
            review_after: None,
            referenced_paths: vec![],
        };
        if let Err(why) = fact.validate() {
            out.rejected.push((p.topic_key.clone(), why));
            continue;
        }
        let candidate = FactCandidate {
            fact,
            proposed_by: ctx.proposed_by.clone(),
        };
        match writer.enqueue_candidate(&candidate).await {
            Ok(id) => out.enqueued.push(id),
            Err(StoreError::Invalid(why)) => out.rejected.push((p.topic_key.clone(), why)),
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_respects_char_boundaries() {
        let s = "é".repeat(10);
        let c = clip(&s, 8);
        assert!(c.len() <= 8 && c.ends_with("..."), "{c}");
        assert_eq!(clip("abc", 8), "abc");
    }

    #[test]
    fn test_file_reads_the_middle_part() {
        assert_eq!(
            test_file("chromium::tests/a.spec.ts::t > u"),
            Some("tests/a.spec.ts")
        );
        assert_eq!(test_file("no-separators"), None);
    }
}
