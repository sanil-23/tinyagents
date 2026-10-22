//! Charging a turn against a goal, and stopping one that would overrun it.
//!
//! [`store::account_usage`] is the raw write.
//! This module is the policy around it — the two halves of budget enforcement a
//! host otherwise reimplements:
//!
//! - [`account_turn`] — *after* a turn: fold its usage into the thread's active
//!   goal, and clear the one-shot continuation suppression when the turn was
//!   user-initiated (a person re-engaging means a later idle period may
//!   auto-continue again).
//! - [`GoalBudgetGuard`] — *during* a turn: given the tokens spent so far,
//!   decide whether to stop now. Checked mid-turn, this bounds an autonomous
//!   run to a small overshoot past its ceiling instead of discovering the
//!   overrun only once the turn is over.
//!
//! Neither aborts anything itself. `account_turn` reports the goal as it stands
//! afterwards and the guard returns a [`BudgetVerdict`]; wiring a stop into a
//! turn is the host's call, because only the host knows whether a graceful
//! wrap-up or a hard cut is wanted.

use std::sync::Arc;

use super::store;
use super::types::{ThreadGoal, ThreadGoalStatus};
use tinyagents_harness::error::Result;
use tinyagents_harness::store::Store;

/// Total tokens a turn spent — the quantity charged against a goal's budget.
pub fn turn_tokens(input: u64, output: u64) -> u64 {
    input.saturating_add(output)
}

/// Whether an in-flight turn should be stopped to stay inside its goal's budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// Keep going.
    Continue,
    /// The projected spend meets or exceeds the budget.
    Stop {
        /// Human-readable reason, suitable for a transcript or a log line.
        reason: String,
    },
}

impl BudgetVerdict {
    /// Whether this verdict calls for stopping.
    pub fn is_stop(&self) -> bool {
        matches!(self, Self::Stop { .. })
    }
}

/// Atomically fold a finished active turn into the thread's goal.
///
/// Inactive goals are not charged. The returned value is the goal after
/// accounting, or `None` when no active goal exists. Elapsed time retains
/// subsecond precision.
pub async fn account_turn(
    store: &Arc<dyn Store>,
    thread_id: &str,
    input_tokens: u64,
    output_tokens: u64,
    elapsed: std::time::Duration,
) -> Result<Option<ThreadGoal>> {
    Ok(
        store::account_turn_atomic(store, thread_id, input_tokens, output_tokens, elapsed)
            .await?
            .map(|(_, updated)| updated),
    )
}

/// Mid-turn budget check, armed for one specific version of a goal.
///
/// Built from a goal that is active or paused and has a budget; a goal with
/// neither is nothing to enforce. Including paused goals lets the host keep a
/// guard installed when the model resumes a goal during the turn. The captured
/// `goal_id` is what makes the guard safe to hold across a long turn: if the
/// objective is replaced while the turn runs, the new goal has a new id, the
/// guard stops matching, and it quietly stands down instead of enforcing a
/// budget that no longer applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalBudgetGuard {
    thread_id: String,
    goal_id: String,
    budget: u64,
}

impl GoalBudgetGuard {
    /// A guard for an active or paused `goal`, or `None` when it is complete
    /// or has no budget.
    pub fn for_goal(goal: &ThreadGoal) -> Option<Self> {
        if !matches!(
            goal.status,
            ThreadGoalStatus::Active | ThreadGoalStatus::Paused
        ) {
            return None;
        }
        Some(Self {
            thread_id: goal.thread_id.clone(),
            goal_id: goal.goal_id.clone(),
            budget: goal.token_budget?,
        })
    }

    /// The thread this guard watches.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// The goal version this guard was armed for.
    pub fn goal_id(&self) -> &str {
        &self.goal_id
    }

    /// The ceiling being enforced.
    pub fn budget(&self) -> u64 {
        self.budget
    }

    /// Verdict for a turn that has spent `in_flight_tokens` so far.
    ///
    /// Reads the goal's already-accounted usage, adds the in-flight spend, and
    /// stops once the total reaches the budget. Returns
    /// [`Continue`](BudgetVerdict::Continue) whenever there is nothing left to
    /// enforce: the goal is gone, was replaced, or is no longer active.
    pub async fn check(
        &self,
        store: &Arc<dyn Store>,
        in_flight_tokens: u64,
    ) -> Result<BudgetVerdict> {
        let Some(goal) = store::get(store, &self.thread_id).await? else {
            return Ok(BudgetVerdict::Continue);
        };
        if goal.goal_id != self.goal_id || !goal.status.is_active() {
            return Ok(BudgetVerdict::Continue);
        }
        Ok(self.verdict_for(goal.tokens_used, in_flight_tokens))
    }

    /// The pure half of [`check`](Self::check): the verdict for a given
    /// accounted and in-flight spend, with no store read.
    pub fn verdict_for(&self, accounted_tokens: u64, in_flight_tokens: u64) -> BudgetVerdict {
        let projected = accounted_tokens.saturating_add(in_flight_tokens);
        if projected >= self.budget {
            BudgetVerdict::Stop {
                reason: format!(
                    "thread goal budget reached: {projected} tokens >= {} budget — stopping to \
                     summarise progress",
                    self.budget
                ),
            }
        } else {
            BudgetVerdict::Continue
        }
    }
}

/// Whether `status` still accrues usage. Kept next to the accounting policy so
/// callers do not re-derive the rule.
pub fn accrues_usage(status: ThreadGoalStatus) -> bool {
    status.is_active()
}
