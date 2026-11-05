//! Ports: the async traits every adapter implements and the server is wired
//! through, plus the domain types they exchange.
//!
//! Index and ownership: `docs/contracts/ports.md`.
//!
//! | Port | Production adapter | Crate |
//! | --- | --- | --- |
//! | [`DecisionEngine`] | Jev `POST /v1/systemone` | `guard-jev` |
//! | [`IssueTracker`] | Jira Cloud REST v3 | `guard-tracker` |
//! | [`CodeHost`] | GitHub / GitLab REST | `guard-codehost` |
//! | [`DecisionLog`], [`GateHistory`] | Postgres (sqlx) | `guard-store` |
//! | [`EvidenceStore`] | S3 / MinIO (SigV4) | `guard-store` |
//! | [`ImpactAnalyzer`] | tree-sitter import graph | `guard-impact` |
//! | [`AgentRuntime`] | Claude Code, Codex, Copilot CLI, mini-swe-agent, OpenHands | `guard-runtimes` |
//! | [`SandboxProvider`] | rootless Docker (gVisor where available) | `guard-sandbox` |
//! | [`ModelGateway`] | LiteLLM proxy (`/key/*`, `/spend/logs/v2`) | `guard-runtimes` |
//! | [`TaskQueue`] | Postgres `task_queue`, `FOR UPDATE SKIP LOCKED` | `guard-store` |
//! | [`MemoryFactsWriter`] (orchestrator only), [`MemoryFactsReader`] | Postgres `memory_facts` (writer / read-only roles) | `guard-store` |
//!
//! With the `fakes` feature (and in this crate's tests), [`fakes`] provides an
//! in-memory implementation of every port.

pub mod agent;
pub mod code_host;
pub mod decision_engine;
pub mod decision_log;
pub mod evidence_store;
pub mod impact_analyzer;
pub mod issue_tracker;
pub mod memory_facts;
pub mod model_gateway;
pub mod sandbox;
pub mod secret;
pub mod task_queue;
pub mod types;

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

pub use code_host::{
    AGENT_LABEL, AgentPullRequest, BranchPush, CodeHost, CodeHostError, OpenedPullRequest,
    PullRequestPublisher, PushedBranch, WebhookError,
};
pub use decision_engine::{DecisionEngine, EngineError};
pub use decision_log::{DecisionLog, GateHistory, StoreError};
pub use evidence_store::{EvidenceError, EvidenceStore, MAX_PRESIGN_EXPIRY};
pub use impact_analyzer::{ImpactAnalyzer, ImpactError};
pub use issue_tracker::{IssueTracker, TrackerError, TrackerEvent, TrackerWebhookError};
pub use types::*;

pub use agent::{
    AgentOutcome, AgentRuntime, AgentTask, AllowedPaths, AttemptPolicy, GatewayBinding,
    MemoryProposal, Role, RuntimeError, RuntimeEvent, RuntimeEventKind, SealedSpec, TaskResult,
    TaskStatus, Usage,
};
pub use memory_facts::{
    CandidateId, FactCandidate, FactId, FactScope, FactSource, FactStatus, FactsBrief, FactsPage,
    FactsQuery, MemoryFact, MemoryFactsReader, MemoryFactsWriter, NewFact, PendingCandidate,
    read_only,
};
pub use model_gateway::{
    GatewayError, KeyRequest, ModelGateway, SpendPage, SpendQuery, SpendRecord, VirtualKey,
};
pub use sandbox::{
    EgressAllowlist, EgressPurpose, EgressRule, ExecOutput, ExecRequest, ExtractedDiff, Mount,
    MountMode, Sandbox, SandboxError, SandboxId, SandboxLayout, SandboxProvider, SandboxSpec,
};
pub use secret::Secret;
pub use task_queue::{
    ClaimedTask, NewTask, QueuedTask, TaskEnqueued, TaskId, TaskQueue, TaskState,
};

pub use crate::fingerprint::Fingerprint;
pub use crate::journey_changes::{ChangeReview, JourneyChange};
pub use crate::thresholds::{Mode, Thresholds};
