//! Explicit fake adapters of `guard serve` (server-api.md, "Fake mode"):
//! end-to-end runs and demos without a Jev key or a Jira site. They are only
//! used when `GUARD_JEV_FAKE=1` / `GUARD_JIRA_FAKE=1` (or `--fake`) is set.

use guard_core::ports::fakes::{FAKE_MODEL, InMemoryTracker};
use guard_core::ports::{
    Area, Attachment, AttachmentRef, Classified, CommentRef, DecisionEngine, DiffSummary,
    EngineError, Issue, IssueRef, IssueTracker, JourneyQuestion, Kind, NewBug, StatusCategory,
    TrackerError, TrackerEvent, TrackerWebhookError, TriageAnswers, TriageRequest, WebhookRequest,
};
use std::collections::BTreeMap;

/// Account id the fake Jira treats as guard's own API user.
pub const FAKE_SELF_ACCOUNT: &str = "guard-fake";

/// A deterministic `DecisionEngine`.
///
/// - Triage: the first area option whose key the ticket text names (summary,
///   description, acceptance criteria; case-insensitive), else the first
///   option; kind `bugfix`, low risk, every Noul on the agent side of the
///   default thresholds (confidence 0.9).
/// - Impact: `0.9` when a changed file path contains the journey's area (the
///   part of its id before the first `.`), else `0.05`.
pub struct FakeEngine;

#[async_trait::async_trait]
impl DecisionEngine for FakeEngine {
    fn model(&self) -> &str {
        FAKE_MODEL
    }

    async fn triage(&self, request: &TriageRequest) -> Result<TriageAnswers, EngineError> {
        let text = format!(
            "{} {} {}",
            request.state.summary,
            request.state.description,
            request.state.acceptance_criteria.join(" ")
        )
        .to_ascii_lowercase();
        let named = request.areas.iter().find(|a| {
            text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                .any(|w| w == a.key.as_str().to_ascii_lowercase())
        });
        let area = named.or(request.areas.first()).ok_or_else(|| {
            EngineError::Unconfigured("fake engine: target.toml has no [areas]".into())
        })?;
        Ok(TriageAnswers {
            model: FAKE_MODEL.into(),
            area: Classified {
                value: Area::new(area.key.as_str()),
                confidence: 0.9,
            },
            kind: Classified {
                value: Kind::Bugfix,
                confidence: 0.9,
            },
            risk: Classified {
                value: 0.5,
                confidence: 0.9,
            },
            clear_e2e_criteria: 0.9,
            touches_money_or_security: 0.05,
            agent_suitable: 0.9,
            changes_existing_flow: 0.05,
            raw: serde_json::json!({"fake": true}),
        })
    }

    async fn impact(
        &self,
        diff: &DiffSummary,
        journeys: &[JourneyQuestion],
    ) -> Result<BTreeMap<String, f64>, EngineError> {
        Ok(journeys
            .iter()
            .map(|j| {
                let area = j.id.split('.').next().unwrap_or(&j.id);
                let hit = diff.files.iter().any(|f| f.path.contains(area));
                (j.id.clone(), if hit { 0.9 } else { 0.05 })
            })
            .collect())
    }
}

/// An in-memory Jira whose issues come from the webhook payloads themselves:
/// `parse_webhook` authenticates with `GUARD_JIRA_WEBHOOK_SECRET` like the
/// real adapter, then stores the delivered issue so `get_issue` finds it.
pub struct FakeJira {
    inner: InMemoryTracker,
    base_url: String,
    secret: Option<String>,
}

impl FakeJira {
    pub fn new(project: &str, secret: Option<String>) -> Self {
        Self {
            inner: InMemoryTracker::new(project),
            base_url: "https://jira.fake.invalid".into(),
            secret,
        }
    }
}

#[async_trait::async_trait]
impl IssueTracker for FakeJira {
    async fn find_by_label(&self, label: &str) -> Result<Vec<IssueRef>, TrackerError> {
        self.inner.find_by_label(label).await
    }

    async fn create_bug(&self, bug: &NewBug) -> Result<IssueRef, TrackerError> {
        self.inner.create_bug(bug).await
    }

    async fn comment_adf(
        &self,
        key: &str,
        body: &serde_json::Value,
    ) -> Result<CommentRef, TrackerError> {
        self.inner.comment_adf(key, body).await
    }

    async fn attach_file(
        &self,
        key: &str,
        file: &Attachment,
    ) -> Result<AttachmentRef, TrackerError> {
        self.inner.attach_file(key, file).await
    }

    async fn add_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        self.inner.add_labels(key, labels).await
    }

    async fn suggest_labels(&self, key: &str, labels: &[String]) -> Result<(), TrackerError> {
        self.inner.suggest_labels(key, labels).await
    }

    async fn get_issue(&self, key: &str) -> Result<Issue, TrackerError> {
        self.inner.get_issue(key).await
    }

    fn parse_webhook(&self, request: &WebhookRequest) -> Result<TrackerEvent, TrackerWebhookError> {
        guard_tracker::webhook::authenticate(request, self.secret.as_deref())?;
        let hook = guard_tracker::webhook::parse_payload(&request.body, None)?;
        if let Some(ticket) = &hook.ticket {
            self.inner.insert_issue(Issue {
                key: ticket.key.clone(),
                url: format!("{}/browse/{}", self.base_url, ticket.key),
                summary: ticket.summary.clone(),
                description: ticket.description.clone(),
                issue_type: ticket.issue_type.clone().unwrap_or_else(|| "Story".into()),
                status: "To Do".into(),
                status_category: StatusCategory::ToDo,
                labels: ticket.labels.clone(),
            });
        }
        Ok(guard_tracker::webhook::to_event(
            &hook,
            Some(FAKE_SELF_ACCOUNT),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guard_core::ports::{AreaOption, BlastRadius, DiffFile, FileStatus, TriageState};

    fn request() -> TriageRequest {
        TriageRequest {
            ticket_key: "SHOP-7".into(),
            state: TriageState {
                summary: "s".into(),
                description: "d".into(),
                acceptance_criteria: vec![],
                mentioned_modules: vec![],
                blast_radius: BlastRadius::Isolated,
            },
            areas: vec![AreaOption {
                key: Area::new("catalog"),
                description: "x".into(),
            }],
            permutation_seed: None,
        }
    }

    #[tokio::test]
    async fn fake_engine_prefers_the_area_the_ticket_names() {
        let mut r = request();
        r.areas.insert(
            0,
            AreaOption {
                key: Area::new("account"),
                description: "a".into(),
            },
        );
        assert_eq!(
            FakeEngine.triage(&r).await.unwrap().area.value,
            Area::new("account")
        );
        r.state.summary = "Catalog: low stock warning".into();
        assert_eq!(
            FakeEngine.triage(&r).await.unwrap().area.value,
            Area::new("catalog")
        );
    }

    #[tokio::test]
    async fn fake_engine_is_deterministic() {
        let answers = FakeEngine.triage(&request()).await.unwrap();
        assert_eq!(answers.area.value, Area::new("catalog"));
        assert_eq!(answers.model, "jev-1.13.0");
        let diff = DiffSummary {
            files: vec![DiffFile {
                path: "examples/demo-shop/src/checkout/pay.mjs".into(),
                status: FileStatus::Modified,
                additions: 1,
                deletions: 0,
            }],
            ..DiffSummary::default()
        };
        let questions = [
            JourneyQuestion {
                id: "checkout.customer-card".into(),
                impact_question: "q".into(),
            },
            JourneyQuestion {
                id: "account.customer-session".into(),
                impact_question: "q".into(),
            },
        ];
        let impact = FakeEngine.impact(&diff, &questions).await.unwrap();
        assert_eq!(impact["checkout.customer-card"], 0.9);
        assert_eq!(impact["account.customer-session"], 0.05);
    }

    fn delivery(body: &str, bearer: Option<&str>) -> WebhookRequest {
        let mut request = WebhookRequest {
            body: body.as_bytes().to_vec(),
            ..WebhookRequest::default()
        };
        if let Some(b) = bearer {
            request
                .headers
                .insert("authorization".into(), format!("Bearer {b}"));
        }
        request
    }

    const CREATED: &str = r#"{"webhookEvent": "jira:issue_created",
      "user": {"accountId": "human"},
      "issue": {"key": "SHOP-7", "fields": {"summary": "Sold out badge",
        "issuetype": {"name": "Story"}, "labels": ["catalog"], "description": "text"}}}"#;

    #[tokio::test]
    async fn fake_jira_authenticates_and_remembers_delivered_issues() {
        let jira = FakeJira::new("SHOP", Some("s3cret".into()));
        assert_eq!(
            jira.parse_webhook(&delivery(CREATED, Some("wrong"))),
            Err(TrackerWebhookError::Unauthorized)
        );
        let event = jira
            .parse_webhook(&delivery(CREATED, Some("s3cret")))
            .unwrap();
        assert_eq!(
            event,
            TrackerEvent::IssueCreated {
                key: "SHOP-7".into()
            }
        );
        let issue = jira.get_issue("SHOP-7").await.unwrap();
        assert_eq!(issue.summary, "Sold out badge");
        assert_eq!(issue.labels, vec!["catalog"]);
    }

    #[test]
    fn fake_jira_without_a_secret_refuses_every_delivery() {
        let jira = FakeJira::new("SHOP", None);
        assert_eq!(
            jira.parse_webhook(&delivery(CREATED, None)),
            Err(TrackerWebhookError::Unauthorized)
        );
    }
}
