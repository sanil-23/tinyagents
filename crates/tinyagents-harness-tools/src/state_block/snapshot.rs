//! [`RunState`]: the saved half of the block, read in one place.

use std::sync::Arc;

use tinyagents_harness::store::Store;

use crate::goals::store as goal_store;
use crate::scratchpad::store as scratchpad_store;
use crate::todos::store as todo_store;

use super::render::render;
use super::types::{RunLedger, RunState};

impl RunState {
    /// Reads `thread_id`'s goal, plan and notes from `store` (the store the
    /// goal and `todo` tools write).
    ///
    /// Each part is read on its own: one that cannot be read (a store error,
    /// a record that no longer deserializes) is logged and left out, and the
    /// others still come back. An unreadable scratchpad must not hide the
    /// goal and the plan, or reset the block's step tracking.
    pub async fn load(store: &Arc<dyn Store>, thread_id: &str) -> Self {
        let goal = goal_store::get(store, thread_id)
            .await
            .unwrap_or_else(|error| {
                tracing::debug!(thread_id, %error, "[tinyagents::state_block] goal unreadable");
                None
            });
        let todos = match todo_store::get(store, thread_id).await {
            Ok(list) => list.map(|list| list.items).unwrap_or_default(),
            Err(error) => {
                tracing::debug!(thread_id, %error, "[tinyagents::state_block] plan unreadable");
                Vec::new()
            }
        };
        let notes = scratchpad_store::get(store, thread_id)
            .await
            .unwrap_or_else(|error| {
                tracing::debug!(thread_id, %error, "[tinyagents::state_block] notes unreadable");
                None
            })
            .filter(|pad| !pad.is_empty());
        Self {
            thread_id: thread_id.to_string(),
            goal,
            todos,
            notes,
        }
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
