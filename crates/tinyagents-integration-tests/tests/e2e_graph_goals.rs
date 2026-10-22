//! Public integration coverage for harness goal preparation and lifecycle.

use std::sync::Arc;
use std::time::Duration;

use tinyagents_harness::store::{InMemoryStore, Store};
use tinyagents_harness_tools::goals::{GoalStore, ThreadGoalStatus};

#[tokio::test]
async fn prepared_goal_exposes_context_and_budget_for_a_host_turn() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    let goals = GoalStore::new(store);
    let goal = goals
        .set("thread-goal", "process every item", Some(1_000))
        .await
        .expect("set goal");
    goals.pause("thread-goal").await.expect("pause goal");

    let prepared = goals.prepare_turn("thread-goal").await.expect("prepare");
    assert_eq!(prepared.goal.as_ref().unwrap().goal_id, goal.goal_id);
    assert_eq!(
        prepared.goal.as_ref().unwrap().status,
        ThreadGoalStatus::Paused
    );
    assert!(
        prepared
            .context_block
            .as_deref()
            .is_some_and(|block| block.contains("goal_resume"))
    );
    let budget_guard = prepared.budget_guard.expect("paused goal budget guard");

    goals.resume("thread-goal").await.expect("resume goal");
    assert!(
        budget_guard
            .check(&goals.backing_store(), 1_000)
            .await
            .expect("check resumed goal budget")
            .is_stop()
    );

    goals
        .account_turn("thread-goal", 10, 5, Duration::from_millis(500))
        .await
        .unwrap();
    goals.complete("thread-goal").await.expect("complete");
    assert_eq!(
        goals.get("thread-goal").await.unwrap().unwrap().status,
        ThreadGoalStatus::Complete
    );
}
