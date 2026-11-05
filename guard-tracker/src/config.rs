//! Jira adapter configuration, read from environment variables
//! (`docs/contracts/server-api.md`, "Environment").

use guard_core::ports::TrackerError;
use secrecy::SecretString;

/// Default name of the variable holding the Jira API token.
pub const DEFAULT_TOKEN_VAR: &str = "JIRA_API_TOKEN";

/// Everything the Jira adapter needs. Secrets never appear in `Debug` output.
#[derive(Debug, Clone)]
pub struct JiraConfig {
    /// `JIRA_BASE_URL` without a trailing slash (e.g. `https://acme.atlassian.net`).
    pub base_url: String,
    /// `JIRA_EMAIL`: the Atlassian account of the API token (basic auth user).
    pub email: String,
    /// The API token, read from the variable named by the caller (default `JIRA_API_TOKEN`).
    pub api_token: SecretString,
    /// `[tracker].project` of `target.toml`: the project key Bugs are created in.
    pub project: String,
    /// `JIRA_SUGGESTED_FIELD`: labels-type custom field for Shadow Mode suggestions.
    pub suggested_field: Option<String>,
    /// `GUARD_JIRA_WEBHOOK_SECRET`: shared secret of `/webhooks/jira`.
    pub webhook_secret: Option<SecretString>,
    /// `JIRA_ACCEPTANCE_CRITERIA_FIELD`: custom field (e.g. `customfield_10050`)
    /// holding the acceptance criteria. Unset: criteria are extracted from the
    /// description later, by `guard_core::triage`.
    pub acceptance_criteria_field: Option<String>,
}

impl JiraConfig {
    /// Only what `GET /rest/api/3/myself` needs (`guard connect jira`): no
    /// project, no optional fields. The trailing slash of `base_url` is dropped.
    pub fn for_credentials(
        base_url: impl Into<String>,
        email: impl Into<String>,
        api_token: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into().trim().trim_end_matches('/').to_string(),
            email: email.into().trim().to_string(),
            api_token: SecretString::from(api_token.into()),
            project: String::new(),
            suggested_field: None,
            webhook_secret: None,
            acceptance_criteria_field: None,
        }
    }

    /// Reads the configuration from the process environment.
    pub fn from_env(project: &str, token_var: &str) -> Result<Self, TrackerError> {
        Self::from_lookup(project, token_var, |name| std::env::var(name).ok())
    }

    /// Reads the configuration through `lookup` (tests inject a map). Empty
    /// or whitespace-only values count as unset.
    pub fn from_lookup(
        project: &str,
        token_var: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, TrackerError> {
        let get = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let token_var = if token_var.trim().is_empty() {
            DEFAULT_TOKEN_VAR
        } else {
            token_var
        };
        let base_url = get("JIRA_BASE_URL");
        let email = get("JIRA_EMAIL");
        let token = get(token_var);
        let mut missing = Vec::new();
        if base_url.is_none() {
            missing.push("JIRA_BASE_URL");
        }
        if email.is_none() {
            missing.push("JIRA_EMAIL");
        }
        if token.is_none() {
            missing.push(token_var);
        }
        if project.trim().is_empty() {
            missing.push("[tracker].project");
        }
        if !missing.is_empty() {
            return Err(TrackerError::Unconfigured(format!(
                "missing {}",
                missing.join(", ")
            )));
        }
        Ok(Self {
            base_url: base_url
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_string(),
            email: email.unwrap_or_default(),
            api_token: SecretString::from(token.unwrap_or_default()),
            project: project.trim().to_string(),
            suggested_field: get("JIRA_SUGGESTED_FIELD"),
            webhook_secret: get("GUARD_JIRA_WEBHOOK_SECRET").map(SecretString::from),
            acceptance_criteria_field: get("JIRA_ACCEPTANCE_CRITERIA_FIELD"),
        })
    }
}
