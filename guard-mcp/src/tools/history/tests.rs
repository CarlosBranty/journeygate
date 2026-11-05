use super::*;
use guard_core::ports::{EvidenceError, FactId, FactSource, FactsPage, ObjectRef};
use std::sync::Mutex;

const MEMORY_CONTRACT: &str = include_str!("../../../../docs/contracts/memory.md");
const REPO: &str = "acme/shop";

/// The JSON blocks of memory.md's `guard_history` section: input, output.
fn contract_blocks() -> Vec<Value> {
    let fence = "`".repeat(3);
    let section = MEMORY_CONTRACT
        .split("## `guard_history` MCP tool")
        .nth(1)
        .expect("guard_history section");
    let section = section.split("## Engram export").next().unwrap();
    section
        .split(&format!("{fence}json"))
        .skip(1)
        .map(|chunk| serde_json::from_str(chunk.split(fence.as_str()).next().unwrap()).unwrap())
        .collect()
}

/// A Vec-backed reader. It implements `MemoryFactsReader` only.
#[derive(Default)]
struct Facts {
    facts: Vec<MemoryFact>,
    queries: Mutex<Vec<FactsQuery>>,
}

#[async_trait::async_trait]
impl MemoryFactsReader for Facts {
    async fn list_facts(&self, query: &FactsQuery) -> Result<FactsPage, StoreError> {
        query.validate()?;
        self.queries.lock().unwrap().push(query.clone());
        let after: i64 = match &query.cursor {
            None => 0,
            Some(c) => c
                .parse()
                .map_err(|_| StoreError::Invalid(format!("bad cursor {c:?}")))?,
        };
        let statuses = query.effective_statuses();
        let mut matched: Vec<MemoryFact> = self
            .facts
            .iter()
            .filter(|f| f.id.0 > after && f.repo == query.repo && statuses.contains(&f.status))
            .filter(|f| query.scope.is_none_or(|s| s == f.scope))
            .filter(|f| query.scope_key.is_none() || query.scope_key == f.scope_key)
            .filter(|f| {
                query
                    .topic_prefix
                    .as_deref()
                    .is_none_or(|p| f.topic_key.starts_with(p))
            })
            .take(query.limit as usize + 1)
            .cloned()
            .collect();
        let next_cursor = (matched.len() > query.limit as usize).then(|| {
            matched.truncate(query.limit as usize);
            matched.last().unwrap().id.0.to_string()
        });
        Ok(FactsPage {
            facts: matched,
            next_cursor,
        })
    }

    async fn get_fact(&self, id: FactId) -> Result<Option<MemoryFact>, StoreError> {
        Ok(self.facts.iter().find(|f| f.id == id).cloned())
    }

    async fn topic_history(&self, repo: &str, topic: &str) -> Result<Vec<MemoryFact>, StoreError> {
        Ok(self
            .facts
            .iter()
            .filter(|f| f.repo == repo && f.topic_key == topic)
            .cloned()
            .collect())
    }
}

struct Presigner;

#[async_trait::async_trait]
impl EvidenceStore for Presigner {
    async fn put_object(&self, _: &str, _: Vec<u8>, _: &str) -> Result<ObjectRef, EvidenceError> {
        Err(EvidenceError::Unconfigured("read-only".into()))
    }

    async fn get_object(&self, _: &str) -> Result<Vec<u8>, EvidenceError> {
        Err(EvidenceError::Unconfigured("read-only".into()))
    }

    fn presign_get(&self, key: &str, expires_in: Duration) -> Result<String, EvidenceError> {
        Ok(format!(
            "https://evidence.test/{key}?expires={}",
            expires_in.as_secs()
        ))
    }
}

fn fact(id: i64, topic: &str, status: FactStatus) -> MemoryFact {
    MemoryFact {
        id: FactId(id),
        repo: REPO.into(),
        scope: FactScope::Journey,
        scope_key: Some("checkout.pay".into()),
        topic_key: topic.into(),
        body: format!("claim {id}"),
        commit_sha: "0123456789abcdef0123456789abcdef01234567".into(),
        run_id: Some("run-1".into()),
        evidence_uri: Some(format!("s3://guard-evidence/runs/run-1/{id}/trace.zip")),
        source: FactSource::GateResult,
        valid_from: "2026-10-01T00:00:00Z".into(),
        invalid_at: (status == FactStatus::Invalidated).then(|| "2026-10-02T00:00:00Z".into()),
        recorded_at: "2026-10-01T00:00:00Z".into(),
        review_after: None,
        status,
        review_reason: (status == FactStatus::NeedsReview)
            .then(|| "src/pay.ts changed in 89abcde".into()),
        supersedes: None,
        referenced_paths: vec!["src/pay.ts".into()],
    }
}

fn context(facts: Vec<MemoryFact>, evidence: bool) -> (HistoryContext, Arc<Facts>) {
    let reader = Arc::new(Facts {
        facts,
        ..Facts::default()
    });
    let ctx = HistoryContext {
        reader: reader.clone(),
        repo: REPO.into(),
        evidence: evidence.then(|| Arc::new(Presigner) as Arc<dyn EvidenceStore>),
    };
    (ctx, reader)
}

fn args(v: Value) -> Option<Map<String, Value>> {
    v.as_object().cloned()
}

#[test]
fn the_schemas_are_the_contract_blocks() {
    let blocks = contract_blocks();
    assert_eq!(blocks.len(), 2);
    let s = spec();
    assert_eq!(s.input_schema, blocks[0]);
    assert_eq!(s.output_schema, blocks[1]);
}

#[test]
fn the_tool_is_read_only_and_closed_world() {
    let tool = spec().to_tool();
    let a = tool.annotations.unwrap();
    assert_eq!(a.read_only_hint, Some(true));
    assert_eq!(a.destructive_hint, Some(false));
    assert_eq!(a.open_world_hint, Some(false));
    assert_eq!(tool.name, GUARD_HISTORY);
    let props = spec().input_schema["properties"]
        .as_object()
        .unwrap()
        .clone();
    for word in [
        "record", "approve", "write", "update", "delete", "confirm", "propose",
    ] {
        assert!(!GUARD_HISTORY.contains(word));
        assert!(props.keys().all(|k| !k.contains(word)), "{word}");
    }
    // `include_invalidated` is a read filter, not an action.
    assert_eq!(props["include_invalidated"]["type"], json!("boolean"));
}

#[test]
fn arguments_are_validated() {
    assert_eq!(parse_args(None).unwrap().limit, 20);
    let r = parse_args(args(
        json!({"scope": "area", "scope_key": "checkout", "limit": 50}),
    ))
    .unwrap();
    assert_eq!(r.scope, Some(FactScope::Area));
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 51}),
        json!({"scope": "galaxy"}),
        json!({"body": "x"}),
        json!({"include_invalidated": "yes"}),
    ] {
        assert!(parse_args(args(bad.clone())).is_err(), "{bad}");
    }
}

#[tokio::test]
async fn without_a_reader_the_tool_is_not_configured() {
    let e = run(None, HistoryRequest::default()).await.unwrap_err();
    assert_eq!(e.error, crate::error::ToolErrorCode::ServerNotConfigured);
}

#[tokio::test]
async fn pages_return_ids_evidence_pointers_and_review_flags() {
    let facts = vec![
        fact(1, "flaky/pay", FactStatus::Active),
        fact(2, "failure/guard-fp-1a2b3c4d", FactStatus::NeedsReview),
        fact(3, "old/claim", FactStatus::Invalidated),
        fact(4, "flaky/cart", FactStatus::Active),
    ];
    let (ctx, reader) = context(facts, true);
    let first = run(Some(&ctx), parse_args(args(json!({"limit": 2}))).unwrap())
        .await
        .unwrap();
    assert_eq!(first.repo, REPO);
    assert_eq!(
        first.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let flagged = &first.facts[1];
    assert!(flagged.needs_review);
    assert_eq!(flagged.status, "needs_review");
    assert_eq!(
        flagged.review_reason.as_deref(),
        Some("src/pay.ts changed in 89abcde")
    );
    assert!(!first.facts[0].needs_review);
    assert_eq!(
        first.facts[0].evidence_uri.as_deref(),
        Some("s3://guard-evidence/runs/run-1/1/trace.zip")
    );
    assert_eq!(
        first.facts[0].evidence_url.as_deref(),
        Some("https://evidence.test/runs/run-1/1/trace.zip?expires=3600")
    );
    let cursor = first.next_cursor.clone().expect("more");
    let second = run(
        Some(&ctx),
        parse_args(args(json!({"limit": 2, "cursor": cursor}))).unwrap(),
    )
    .await
    .unwrap();
    // The invalidated fact is not served by default.
    assert_eq!(
        second.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![4]
    );
    assert_eq!(second.next_cursor, None);

    let all = run(
        Some(&ctx),
        parse_args(args(
            json!({"include_invalidated": true, "topic_prefix": "old/"}),
        ))
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(all.facts.len(), 1);
    assert_eq!(
        all.facts[0].invalid_at.as_deref(),
        Some("2026-10-02T00:00:00Z")
    );
    // Filters reach the reader.
    let q = reader.queries.lock().unwrap().last().cloned().unwrap();
    assert_eq!(q.topic_prefix.as_deref(), Some("old/"));
    assert_eq!(q.statuses.len(), 3);

    let structured = serde_json::to_value(&first).unwrap();
    for key in ["repo", "facts", "next_cursor"] {
        assert!(structured.get(key).is_some(), "{key}");
    }
}

#[tokio::test]
async fn a_topic_key_returns_every_version_oldest_first() {
    let mut v2 = fact(5, "selector/pay", FactStatus::Active);
    v2.supersedes = Some(FactId(2));
    let facts = vec![
        fact(1, "flaky/pay", FactStatus::Active),
        fact(2, "selector/pay", FactStatus::Invalidated),
        v2,
    ];
    let (ctx, _) = context(facts, false);
    let out = run(
        Some(&ctx),
        parse_args(args(json!({"topic_key": "selector/pay"}))).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        out.facts.iter().map(|f| f.id).collect::<Vec<_>>(),
        vec![2, 5]
    );
    assert_eq!(out.facts[1].supersedes, Some(2));
    assert_eq!(
        out.facts[0].evidence_url, None,
        "no evidence store configured"
    );
    assert_eq!(out.next_cursor, None);
}

#[tokio::test]
async fn bad_cursors_are_config_errors() {
    let (ctx, _) = context(vec![fact(1, "a/b", FactStatus::Active)], false);
    let e = run(
        Some(&ctx),
        parse_args(args(json!({"cursor": "zzz"}))).unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.error, crate::error::ToolErrorCode::ConfigError);
}

#[tokio::test]
async fn the_response_is_hard_capped_well_under_ten_thousand_tokens() {
    // 50 facts with the largest bodies, full of characters JSON escapes.
    let seg = "\"quoted\"\n\t";
    let body = seg.repeat(1024 / seg.len()) + &"x".repeat(1024 % seg.len());
    assert_eq!(body.len(), 1024);
    let facts: Vec<MemoryFact> = (1..=50)
        .map(|i| {
            let mut f = fact(
                i,
                &format!("topic/{i:02}/{}", "k".repeat(100)),
                FactStatus::NeedsReview,
            );
            f.body = body.clone();
            f.review_reason = Some("r".repeat(200));
            f
        })
        .collect();
    let (ctx, _) = context(facts, true);
    let mut seen = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let mut a = json!({"limit": 50});
        if let Some(c) = &cursor {
            a["cursor"] = json!(c);
        }
        let out = run(Some(&ctx), parse_args(args(a)).unwrap()).await.unwrap();
        let bytes = result_bytes(&out);
        assert!(bytes <= MAX_HISTORY_RESULT_BYTES, "{bytes} bytes");
        // Even at 3 bytes per token this is far below 10k tokens.
        assert!(bytes / 3 < 10_000);
        assert!(!out.facts.is_empty());
        seen.extend(out.facts.iter().map(|f| f.id));
        match out.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    // Capping only defers facts to later pages: every fact, once, in order.
    assert_eq!(seen, (1..=50).collect::<Vec<_>>());
}
