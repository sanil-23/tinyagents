//! Per-thread **todo list**: an ordered checklist of steps per thread.
//!
//! The todo list holds the concrete steps the run is working through: an
//! ordered list of [`TodoItem`]s, each `pending`, `in_progress`,
//! `completed` or `cancelled`. This module owns the data model and markdown
//! rendering ([`types`]), the step order (`sequence`: one step at a time,
//! top to bottom), harness-[`Store`](tinyagents_harness::store::Store)-backed
//! persistence that keeps it ([`store`]), the model-facing tool ([`tool`]),
//! and the middleware that holds a run to its plan ([`middleware`]).
//!
//! The model writes the whole list once, then ticks steps off with
//! `{"complete": N}`; nothing else hangs off an item — no ids, no approvals,
//! no assignment, no run log. A list is always `(Store, thread_id)`.

pub mod middleware;
mod sequence;
pub mod store;
mod tool;
mod types;

pub use middleware::{DEFAULT_REMINDER_ROUNDS, TodoSequenceMiddleware};
pub use tool::TodoTool;
pub(crate) use types::now_stamp;
pub use types::{
    TodoItem, TodoList, TodoStatus, TodosSnapshot, normalise_list, parse_status, render_markdown,
};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod test;
