use super::*;
use std::sync::Arc;
use tinyagents_harness::store::{InMemoryStore, Store};
use tinyagents_harness_tools::goals::store;

#[tokio::test]
async fn active_goal_budget_stops_when_in_flight_usage_reaches_the_cap() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
    let goal = store::set(&store, "thread", "finish", Some(10))
        .await
        .unwrap();
    let hook = GoalBudgetStopHook::for_goal(Arc::clone(&store), &goal).unwrap();
    let mut cost = TurnCost::new();
    cost.input_tokens = 8;
    cost.output_tokens = 2;
    let state = TurnState {
        iteration: 1,
        max_iterations: 10,
        cost: &cost,
        model: "test",
    };
    assert!(matches!(
        hook.check(&state).await,
        StopDecision::Stop { .. }
    ));
}

#[tokio::test]
async fn paused_goal_hook_stays_dormant_until_the_goal_resumes() {
    use tinyagents_harness_tools::goals::{ThreadGoalStatus, store as goal_store};
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
    let active = goal_store::set(&store, "thread", "finish", Some(10))
        .await
        .unwrap();
    let paused = goal_store::pause(&store, "thread").await.unwrap();
    assert_eq!(paused.status, ThreadGoalStatus::Paused);

    let hook = GoalBudgetStopHook::for_goal(Arc::clone(&store), &paused)
        .expect("paused budgeted goal keeps its dormant guard");
    let mut cost = TurnCost::new();
    cost.input_tokens = 10;
    let state = TurnState {
        iteration: 1,
        max_iterations: 10,
        cost: &cost,
        model: "test",
    };
    assert!(matches!(hook.check(&state).await, StopDecision::Continue));

    let resumed = goal_store::resume(&store, "thread").await.unwrap();
    assert_eq!(resumed.goal_id, active.goal_id);
    assert!(matches!(
        hook.check(&state).await,
        StopDecision::Stop { .. }
    ));
}

#[tokio::test]
async fn terminal_or_unbudgeted_goals_do_not_install_a_hook() {
    use tinyagents_harness_tools::goals::{ThreadGoalStatus, store as goal_store};
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::new());
    let unbudgeted = goal_store::set(&store, "thread", "finish", None)
        .await
        .unwrap();
    assert!(GoalBudgetStopHook::for_goal(Arc::clone(&store), &unbudgeted).is_none());
    let completed = goal_store::complete(&store, "thread").await.unwrap();
    assert_eq!(completed.status, ThreadGoalStatus::Complete);
    assert!(GoalBudgetStopHook::for_goal(store, &completed).is_none());
}
