//! Observed goal operations and the host turn-preparation helper.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;

use super::budget::GoalBudgetGuard;
use super::observer::{GoalObserver, GoalUpdateThrottle};
use super::prompt::active_goal_context_block;
use super::store;
use super::types::ThreadGoal;
use tinyagents_harness::error::Result;
use tinyagents_harness::store::Store;

struct IgnoreGoalObserver;

impl GoalObserver for IgnoreGoalObserver {
    fn on_updated(&self, _: &ThreadGoal) {}
    fn on_cleared(&self, _: &str) {}
}

/// Observed lifecycle operations over a harness key-value store.
#[derive(Clone)]
pub struct GoalStore {
    store: Arc<dyn Store>,
    observer: Arc<dyn GoalObserver>,
    throttle: Arc<GoalUpdateThrottle>,
    notifications: Arc<AsyncMutex<()>>,
}

impl GoalStore {
    /// Creates goal operations without an external lifecycle observer.
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self::with_observer(store, Arc::new(IgnoreGoalObserver))
    }

    /// Creates goal operations that report persisted lifecycle changes.
    pub fn with_observer(store: Arc<dyn Store>, observer: Arc<dyn GoalObserver>) -> Self {
        Self {
            store,
            observer,
            throttle: Arc::new(GoalUpdateThrottle::default()),
            notifications: Arc::new(AsyncMutex::new(())),
        }
    }

    /// Clones the backing store for budget-stop hooks and related host services.
    pub fn backing_store(&self) -> Arc<dyn Store> {
        Arc::clone(&self.store)
    }

    /// Reads the thread's goal.
    pub async fn get(&self, thread_id: &str) -> Result<Option<ThreadGoal>> {
        store::get(&self.store, thread_id).await
    }

    /// Creates or replaces the thread's goal and notifies the observer.
    pub async fn set(
        &self,
        thread_id: &str,
        objective: &str,
        token_budget: Option<u64>,
    ) -> Result<ThreadGoal> {
        let _notifications = self.notifications.lock().await;
        let goal = store::set(&self.store, thread_id, objective, token_budget).await?;
        self.notify_updated(&goal);
        Ok(goal)
    }

    /// Marks the thread's goal complete and notifies the observer.
    pub async fn complete(&self, thread_id: &str) -> Result<ThreadGoal> {
        let _notifications = self.notifications.lock().await;
        let goal = store::complete(&self.store, thread_id).await?;
        self.notify_updated(&goal);
        Ok(goal)
    }

    /// Pauses the thread's active goal and notifies the observer when changed.
    pub async fn pause(&self, thread_id: &str) -> Result<ThreadGoal> {
        let _notifications = self.notifications.lock().await;
        let (before, goal) = store::pause_with_before(&self.store, thread_id).await?;
        if before.status != goal.status {
            self.notify_updated(&goal);
        }
        Ok(goal)
    }

    /// Resumes a paused goal and notifies the observer when changed.
    pub async fn resume(&self, thread_id: &str) -> Result<ThreadGoal> {
        let _notifications = self.notifications.lock().await;
        let (before, goal) = store::resume_with_before(&self.store, thread_id).await?;
        if before.status != goal.status {
            self.notify_updated(&goal);
        }
        Ok(goal)
    }

    /// Removes a goal and notifies the observer only when a row existed.
    pub async fn clear(&self, thread_id: &str) -> Result<bool> {
        let _notifications = self.notifications.lock().await;
        let cleared = store::clear(&self.store, thread_id).await?;
        if cleared {
            self.throttle.clear(thread_id);
            self.observer.on_cleared(thread_id);
        }
        Ok(cleared)
    }

    /// Atomically accounts usage against an active goal and reports a budget
    /// meter update only when its cumulative threshold or status changes.
    pub async fn account_turn(
        &self,
        thread_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        elapsed: Duration,
    ) -> Result<Option<ThreadGoal>> {
        let _notifications = self.notifications.lock().await;
        let Some((before, updated)) = store::account_turn_atomic(
            &self.store,
            thread_id,
            input_tokens,
            output_tokens,
            elapsed,
        )
        .await?
        else {
            return Ok(None);
        };
        if self.throttle.should_publish(&before, &updated) {
            self.observer.on_updated(&updated);
        }
        Ok(Some(updated))
    }

    /// Loads the goal for a turn without changing its lifecycle status.
    ///
    /// Paused goals stay paused so the model can decide whether the incoming
    /// user message continues the goal and call `goal_resume` when appropriate.
    pub async fn prepare_turn(&self, thread_id: &str) -> Result<GoalTurnPreparation> {
        let goal = store::get(&self.store, thread_id).await?;
        let context_block = goal.as_ref().and_then(active_goal_context_block);
        let budget_guard = goal.as_ref().and_then(GoalBudgetGuard::for_goal);
        Ok(GoalTurnPreparation {
            goal,
            context_block,
            budget_guard,
        })
    }

    fn notify_updated(&self, goal: &ThreadGoal) {
        self.throttle.reset(goal);
        self.observer.on_updated(goal);
    }
}

/// Goal context and budget enforcement prepared for one host turn.
#[derive(Debug, Clone)]
pub struct GoalTurnPreparation {
    /// Current goal, if this thread has one.
    pub goal: Option<ThreadGoal>,
    /// Prompt block to prepend for an active, paused, or budget-limited goal.
    pub context_block: Option<String>,
    /// Guard for a budgeted active goal, ready for a host stop hook.
    pub budget_guard: Option<GoalBudgetGuard>,
}
