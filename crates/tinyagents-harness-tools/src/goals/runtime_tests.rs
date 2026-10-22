use super::*;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tinyagents_harness::store::{InMemoryStore, Store};

#[derive(Default)]
struct RecordingObserver(Mutex<Vec<String>>);

impl GoalObserver for RecordingObserver {
    fn on_updated(&self, goal: &ThreadGoal) {
        self.0.lock().unwrap().push(format!(
            "updated:{}:{}",
            goal.thread_id,
            goal.status.as_str()
        ));
    }

    fn on_cleared(&self, thread_id: &str) {
        self.0.lock().unwrap().push(format!("cleared:{thread_id}"));
    }
}

fn operations() -> (GoalStore, Arc<RecordingObserver>) {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    let observer = Arc::new(RecordingObserver::default());
    (GoalStore::with_observer(store, observer.clone()), observer)
}

#[tokio::test]
async fn observer_receives_resume_clear_account_and_complete_lifecycle_events() {
    let (goals, observer) = operations();
    let goal = goals.set("t", "finish", None).await.unwrap();
    observer.0.lock().unwrap().clear();

    super::store::pause(&goals.backing_store(), "t")
        .await
        .unwrap();
    goals.resume("t").await.unwrap();
    goals
        .account_turn("t", 1, 0, Duration::from_millis(500))
        .await
        .unwrap();
    goals.complete("t").await.unwrap();
    assert!(goals.clear("t").await.unwrap());

    assert_eq!(
        *observer.0.lock().unwrap(),
        [
            "updated:t:active",
            "updated:t:active",
            "updated:t:complete",
            "cleared:t",
        ]
    );
    assert_eq!(goal.thread_id, "t");
}

#[tokio::test]
async fn account_meter_publishes_after_cumulative_usage_reaches_threshold() {
    let (goals, observer) = operations();
    goals.set("t", "finish", Some(100)).await.unwrap();
    observer.0.lock().unwrap().clear();

    for turn in 0..4 {
        goals.account_turn("t", 1, 0, Duration::ZERO).await.unwrap();
        assert!(
            observer.0.lock().unwrap().is_empty(),
            "turn {turn} is below threshold"
        );
    }
    goals.account_turn("t", 1, 0, Duration::ZERO).await.unwrap();
    assert_eq!(observer.0.lock().unwrap().as_slice(), ["updated:t:active"]);
}

#[derive(Default)]
struct InspectingObserver {
    store: Mutex<Option<Arc<dyn Store>>>,
}

impl GoalObserver for InspectingObserver {
    fn on_updated(&self, goal: &ThreadGoal) {
        let store = self.store.lock().unwrap().as_ref().unwrap().clone();
        let persisted = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(super::store::get(&store, &goal.thread_id))
        })
        .unwrap()
        .unwrap();
        assert_eq!(persisted, *goal);
    }

    fn on_cleared(&self, _: &str) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_accounting_preserves_every_unbudgeted_turn() {
    let store: Arc<dyn Store> = Arc::new(YieldingStore::default());
    let goals = GoalStore::new(store);
    goals.set("t", "finish", None).await.unwrap();

    let mut turns = Vec::new();
    for _ in 0..16 {
        let goals = goals.clone();
        turns.push(tokio::spawn(async move {
            goals
                .account_turn("t", 1, 0, Duration::from_millis(125))
                .await
                .unwrap()
        }));
    }
    for turn in turns {
        turn.await.unwrap();
    }

    let goal = goals.get("t").await.unwrap().unwrap();
    assert_eq!(goal.tokens_used, 16);
    assert_eq!(goal.time_used_seconds, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_can_read_the_persisted_goal_during_callback() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    let observer = Arc::new(InspectingObserver::default());
    *observer.store.lock().unwrap() = Some(store.clone());
    let goals = GoalStore::with_observer(store, observer);
    tokio::time::timeout(Duration::from_secs(1), goals.set("t", "finish", None))
        .await
        .unwrap()
        .unwrap();
}

#[derive(Default)]
struct YieldingStore(InMemoryStore);

#[async_trait]
impl Store for YieldingStore {
    async fn get(
        &self,
        namespace: &str,
        key: &str,
    ) -> tinyagents_harness::error::Result<Option<Value>> {
        tokio::task::yield_now().await;
        self.0.get(namespace, key).await
    }

    async fn put(
        &self,
        namespace: &str,
        key: &str,
        value: Value,
    ) -> tinyagents_harness::error::Result<()> {
        tokio::task::yield_now().await;
        self.0.put(namespace, key, value).await
    }

    async fn delete(&self, namespace: &str, key: &str) -> tinyagents_harness::error::Result<()> {
        tokio::task::yield_now().await;
        self.0.delete(namespace, key).await
    }

    async fn list(&self, namespace: &str) -> tinyagents_harness::error::Result<Vec<String>> {
        tokio::task::yield_now().await;
        self.0.list(namespace).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_accounting_commits_once_and_notifies_once_on_budget_transition() {
    let store: Arc<dyn Store> = Arc::new(YieldingStore::default());
    let observer = Arc::new(RecordingObserver::default());
    let goals = GoalStore::with_observer(store, observer.clone());
    goals.set("t", "finish", Some(5)).await.unwrap();
    observer.0.lock().unwrap().clear();

    let mut turns = Vec::new();
    for _ in 0..16 {
        let goals = goals.clone();
        turns.push(tokio::spawn(async move {
            goals.account_turn("t", 1, 0, Duration::ZERO).await.unwrap()
        }));
    }
    for turn in turns {
        turn.await.unwrap();
    }

    let goal = goals.get("t").await.unwrap().unwrap();
    assert_eq!(goal.tokens_used, 5);
    assert_eq!(goal.status, ThreadGoalStatus::BudgetLimited);
    let events = observer.0.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| *event == "updated:t:budget_limited")
            .count(),
        1
    );
}

#[tokio::test]
async fn turn_preparation_preserves_paused_goals_until_model_resumes_them() {
    let (goals, _) = operations();
    goals.set("t", "finish", Some(20)).await.unwrap();
    goals.pause("t").await.unwrap();

    let prepared = goals.prepare_turn("t").await.unwrap();
    let goal = prepared.goal.unwrap();
    assert_eq!(goal.status, ThreadGoalStatus::Paused);
    assert!(
        prepared
            .context_block
            .as_deref()
            .is_some_and(|block| block.contains("goal_resume"))
    );
    assert!(prepared.budget_guard.is_some());
}

#[test]
fn small_turn_durations_accumulate_without_losing_subseconds() {
    let mut goal = ThreadGoal {
        thread_id: "t".into(),
        goal_id: "g".into(),
        objective: "finish".into(),
        status: ThreadGoalStatus::Active,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        time_used_subsec_nanos: 0,
        created_at_ms: 0,
        updated_at_ms: 0,
    };
    super::store::account_elapsed(&mut goal, Duration::from_millis(750));
    super::store::account_elapsed(&mut goal, Duration::from_millis(750));
    assert_eq!(goal.time_used_seconds, 1);
    assert_eq!(goal.time_used_subsec_nanos, 500_000_000);
}
