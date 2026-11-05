//! The task prompt every runtime receives, rendered from `task.json`.
//!
//! Order follows the deterministic-prefix rule (tokens.md): fixed role rules,
//! the tool list (in `task.tools` order) and the facts brief come first and are
//! byte-identical across attempts of a task; ticket content follows; volatile
//! content (attempt number, failure context) comes last. No timestamps, no ids
//! other than the ticket key.

use guard_core::ports::agent::{AgentTask, Role, TASK_FILE};
use guard_core::ports::sandbox::{SKILLS_DIR, WORKSPACE_DIR};
use std::fmt::Write as _;

pub fn render(task: &AgentTask) -> String {
    let mut p = String::new();
    let role = task.role.as_str();
    let _ = writeln!(
        p,
        "You are the {role} agent of guard. You work non-interactively in {WORKSPACE_DIR}; nobody will answer questions."
    );
    let _ = writeln!(
        p,
        "Instructions for this repository are in {SKILLS_DIR}/AGENTS.md and {SKILLS_DIR}/skills (read-only). The full task is {TASK_FILE}."
    );
    p.push_str("\n## Rules\n");
    match task.role {
        Role::Qa => {
            p.push_str("- Write exactly one Playwright spec, red-first: it must fail against the current code for the reason the ticket describes.\n");
            p.push_str("- Change no file other than the allowed path below.\n");
        }
        Role::Coder => {
            p.push_str("- Make the sealed spec pass by changing application code only.\n");
            p.push_str("- Never edit tests, the sealed spec, lockfiles, CI or guard configuration; the diff is checked against the paths below.\n");
        }
    }
    p.push_str("- Paths are repo-relative globs; deny wins over write.\n");
    let _ = writeln!(p, "- Write: {}", join_or_none(&task.allowed_paths.write));
    let _ = writeln!(p, "- Deny: {}", join_or_none(&task.allowed_paths.deny));
    if let Some(s) = &task.sealed_spec {
        let _ = writeln!(p, "- Sealed spec (read-only): {}", s.path);
    }
    p.push_str("\n## Tools\n");
    let _ = writeln!(p, "{}", join_or_none(&task.tools));
    if let Some(brief) = task.facts_brief.as_ref().filter(|b| !b.text.is_empty()) {
        p.push_str("\n## Memory\n");
        p.push_str(brief.text.trim_end());
        p.push('\n');
    }
    let _ = write!(p, "\n## Ticket {} ({})\n", task.ticket, task.repo);
    p.push_str(task.summary.trim());
    p.push('\n');
    if !task.acceptance_criteria.is_empty() {
        p.push_str("\n## Acceptance criteria\n");
        for c in &task.acceptance_criteria {
            let _ = writeln!(p, "- {}", c.trim());
        }
    }
    let _ = write!(
        p,
        "\n## Attempt\n{} of {}\n",
        task.attempt,
        task.attempt_policy.max_attempts()
    );
    if let Some(fc) = task
        .failure_context
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        p.push_str("\n## What failed in the previous attempt\n");
        p.push_str(fc.trim());
        p.push('\n');
    }
    p
}

fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".into()
    } else {
        items.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> AgentTask {
        serde_json::from_str(include_str!(
            "../../schemas/examples/agent-task.example.json"
        ))
        .unwrap()
    }

    #[test]
    fn stable_prefix_survives_a_retry() {
        let first = render(&task());
        let mut retry = task();
        retry.attempt = 2;
        retry.failure_context = Some("guard_test: 1 failing (badge hidden)".into());
        let second = render(&retry);
        let cut = first.find("## Attempt").unwrap();
        assert_eq!(&first[..cut], &second[..cut]);
        assert!(second.ends_with("guard_test: 1 failing (badge hidden)\n"));
        assert!(first.find("## Memory").unwrap() < first.find("## Ticket").unwrap());
        assert!(first.contains("guard_test, guard_failure, guard_history, guard_map"));
    }
}
