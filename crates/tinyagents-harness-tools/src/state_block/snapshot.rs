//! [`RunState`]: the saved half of the block, read in one place.

use std::sync::Arc;

use tinyagents_harness::error::Result;
use tinyagents_harness::store::Store;

use crate::goals::store as goal_store;
use crate::scratchpad::store as scratchpad_store;
use crate::todos::store as todo_store;

use super::render::render;
use super::types::{RunLedger, RunState};

impl RunState {
    /// Reads `thread_id`'s goal, plan and notes from `store` (the store the
    /// goal and `todo` tools write).
    pub async fn load(store: &Arc<dyn Store>, thread_id: &str) -> Result<Self> {
        Ok(Self {
            thread_id: thread_id.to_string(),
            goal: goal_store::get(store, thread_id).await?,
            todos: todo_store::get(store, thread_id)
                .await?
                .map(|list| list.items)
                .unwrap_or_default(),
            notes: scratchpad_store::get(store, thread_id)
                .await?
                .filter(|pad| !pad.is_empty()),
        })
    }

    /// The block as a turn's first call would show it: the saved state with
    /// no tool results yet. `None` when there is nothing saved.
    pub fn render(&self) -> Option<String> {
        render(
            self.goal.as_ref(),
            self.notes.as_ref(),
            &self.todos,
            &RunLedger::default(),
        )
    }
}

#[cfg(test)]
#[path = "snapshot_tests.rs"]
mod tests;
