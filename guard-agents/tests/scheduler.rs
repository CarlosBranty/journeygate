//! The three slots of section 16: host slots, provider slots, daily USD cap.

use guard_agents::config::{AgentsConfig, LoadOptions};
use guard_agents::scheduler::{FixedSpend, SchedulerError, Slots, until_next_utc_day};
use guard_core::ports::StoreError;
use std::sync::Arc;
use std::time::Duration;

fn config(host_slots: u32, providers: &str, cap: f64) -> AgentsConfig {
    let text = include_str!("fixtures/agents.toml")
        .replace("host_slots = 4", &format!("host_slots = {host_slots}"))
        .replace(
            "provider_slots = { anthropic = 3, openai = 3 }",
            &format!("provider_slots = {{ {providers} }}"),
        )
        .replace("daily_usd_cap = 50.0", &format!("daily_usd_cap = {cap:?}"))
        .replace(
            "[scheduler.best_of_n]\nagents = [\"coder\", \"coder-copilot\"]\nlabel = \"agent-best-of-n\"\nselect = \"first_green_smallest_diff\"\n",
            "",
        );
    AgentsConfig::parse(&text, &LoadOptions::default()).unwrap()
}

#[tokio::test]
async fn host_slots_bound_concurrent_tasks() {
    let slots = Slots::new(
        &config(2, "", 10.0).scheduler,
        Arc::new(FixedSpend::new(0.0)),
    );
    let a = slots.try_task_slot().expect("slot 1");
    let _b = slots.try_task_slot().expect("slot 2");
    assert!(slots.try_task_slot().is_none(), "host full");
    assert_eq!(slots.free_host_slots(), 0);
    drop(a);
    assert!(slots.try_task_slot().is_some(), "released on drop");
}

#[tokio::test(start_paused = true)]
async fn provider_slots_limit_concurrent_attempts() {
    let slots = Arc::new(Slots::new(
        &config(4, "openai = 1", 10.0).scheduler,
        Arc::new(FixedSpend::new(0.0)),
    ));
    let first = slots.attempt_permit("openai/gpt-5").await.unwrap();
    assert_eq!(first.provider(), "openai");
    let waiting = {
        let slots = slots.clone();
        tokio::spawn(async move { slots.attempt_permit("openai/gpt-4.1").await.map(|_| ()) })
    };
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(!waiting.is_finished(), "second openai attempt waits");
    // Another provider (and an unlisted one) is not limited by openai.
    let _claude = slots.attempt_permit("claude-opus-5-5").await.unwrap();
    let _x = slots.attempt_permit("gemini/gemini-3").await.unwrap();
    drop(first);
    waiting.await.unwrap().unwrap();
}

#[tokio::test]
async fn daily_cap_stops_new_attempts() {
    let spend = Arc::new(FixedSpend::new(9.99));
    let slots = Slots::new(&config(4, "", 10.0).scheduler, spend.clone());
    slots.check_budget().await.unwrap();
    assert!(slots.attempt_permit("openai/gpt-5").await.is_ok());
    spend.set(10.0);
    match slots.check_budget().await {
        Err(SchedulerError::DailyCapReached { spent, cap }) => {
            assert_eq!((spent, cap), (10.0, 10.0));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        slots.attempt_permit("openai/gpt-5").await,
        Err(SchedulerError::DailyCapReached { .. })
    ));
}

#[tokio::test]
async fn ledger_failure_fails_closed() {
    let spend = Arc::new(FixedSpend::new(0.0));
    spend.fail_with(StoreError::Unavailable("pg down".into()));
    let slots = Slots::new(&config(4, "", 10.0).scheduler, spend);
    assert!(matches!(
        slots.check_budget().await,
        Err(SchedulerError::Ledger(_))
    ));
}

#[test]
fn next_utc_day() {
    assert_eq!(until_next_utc_day(0), Duration::from_secs(86_400));
    assert_eq!(until_next_utc_day(86_400 - 1), Duration::from_secs(1));
    assert_eq!(
        until_next_utc_day(86_400 * 3 + 600),
        Duration::from_secs(86_400 - 600)
    );
}
