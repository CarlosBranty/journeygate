//! `IssueTracker` adapter for Jira Cloud (REST v3) and the gate reporting use case.
//!
//! Implements `guard_core::ports::IssueTracker` and the reporting flow of
//! `docs/contracts/reporting.md`: fingerprint label dedup via JQL, Bug creation,
//! ADF comments and PNG attachments (`X-Atlassian-Token: no-check`, multipart
//! field `file`).
//!
//! | Module | What |
//! | --- | --- |
//! | [`adf`] | Atlassian Document Format builders and the plain-text flattener |
//! | [`config`] | [`JiraConfig`] from `JIRA_BASE_URL`, `JIRA_EMAIL`, the token variable, ... |
//! | [`jira`] | [`JiraTracker`]: the `IssueTracker` HTTP adapter |
//! | [`webhook`] | `/webhooks/jira` authentication and payload parsing |
//! | [`report`] | `report_gate` and the ADF layouts of Bugs and summaries |

pub mod adf;
pub mod config;
pub mod jira;
pub mod report;
pub mod webhook;

pub use config::{DEFAULT_TOKEN_VAR, JiraConfig};
pub use jira::{JiraAccount, JiraTracker};
pub use webhook::{JiraTicket, JiraWebhook, JiraWebhookEvent};
