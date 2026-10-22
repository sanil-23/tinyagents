//! Per-thread **scratchpad**: what the model has learned, in its own words.
//!
//! The run-state block's own facts (`state_block`) are mechanical:
//! a failed command, an executable reported missing, a file written. What a
//! result *means* ("the table is `auth_users`", "adding a NOT NULL column with
//! no default times out", "rewriting the parser as a state machine") only the
//! model can say. The scratchpad keeps those notes per thread, next to the
//! plan, so they survive the end of a turn and a restart:
//!
//! - **facts**: things the model verified, at most [`store::MAX_FACTS`];
//! - **blockers**: what does not work or must be avoided here, and why (a
//!   failed approach, a limit, a constraint), at most
//!   [`store::MAX_BLOCKERS`];
//! - **approach**: the approach the model is on now; a new one replaces it.
//!
//! The model writes them through optional arguments of the `todo` tool
//! (`facts`, `blockers`, `approach`, `retract`), so there is no extra tool to
//! learn; the run-state block (`state_block`, feature `state-block`) shows them on
//! every call.
//! `retract` removes any note containing the given text, so a note that turns
//! out wrong does not stay. Notes are de-duplicated and clipped to one line.
//!
//! **Lifetime.** Notes belong to a piece of work. After a turn's final reply
//! the host calls [`store::close_finished_plan`]: if the plan finished (steps,
//! none open), its facts and blockers move to [`Scratchpad::last_task`], the
//! block's "From your last finished task" section, so the thread's next turns
//! see them at once (active memory). The working notes are cleared for the next
//! task, and the plan with its notes is returned for the host to keep past
//! the thread (long-term memory). An open plan keeps its notes into the next
//! turn.

pub mod store;
mod types;

pub use types::{ClosedPlan, Scratchpad, ScratchpadUpdate, TaskNotes};

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
