//! Prompt-rendering for the durable [`ThreadGoal`]: the per-iteration
//! context block a caller prepends to a work node's prompt.
//!
//! Split out of `goals/types.rs`; kept separate from the plain type
//! definitions since this is presentation, not state.

use crate::goals::types::{ThreadGoal, ThreadGoalStatus};

/// Renders the per-iteration context block a caller can prepend to a work
/// node's prompt so the model knows the objective and how to close it out.
/// Returns `None` only for a completed goal.
pub fn active_goal_context_block(goal: &ThreadGoal) -> Option<String> {
    match goal.status {
        ThreadGoalStatus::Active => {
            let budget = match goal.budget_remaining() {
                Some(remaining) => format!(" (~{remaining} tokens of budget remain)"),
                None => String::new(),
            };
            Some(format!(
                "[thread goal] You are working toward this thread's durable goal{budget}.\n\n\
                 Goal: {objective}\n\n\
                 Assess progress against concrete evidence, then take the next useful step. \
                 If the goal is already satisfied, call `goal_complete` now.",
                objective = goal.objective,
            ))
        }
        ThreadGoalStatus::BudgetLimited => Some(format!(
            "[thread goal] The token budget for this goal is exhausted. Summarise progress \
             and stop; do not start new substantive work.\n\nGoal: {}",
            goal.objective,
        )),
        ThreadGoalStatus::Paused => Some(format!(
            "[thread goal] This goal is paused. Read the user's message and decide whether it continues this objective. If it does, call `goal_resume`; if it starts a different task, call `goal_set`; otherwise leave the goal paused.\n\nGoal: {}",
            goal.objective
        )),
        ThreadGoalStatus::Complete => None,
    }
}
