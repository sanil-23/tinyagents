//! Domain types for the per-thread scratchpad.

use serde::{Deserialize, Serialize};

use crate::todos::TodoItem;

/// What the model has learned in a thread, in its own words: facts it has
/// verified, approaches that failed, and the approach it is on now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scratchpad {
    /// The thread this scratchpad belongs to.
    pub thread_id: String,
    /// Facts the model verified, oldest first.
    #[serde(default)]
    pub facts: Vec<String>,
    /// What does not work or must be avoided here, and why, oldest first.
    #[serde(default)]
    pub blockers: Vec<String>,
    /// The approach the model is on now; a new one replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approach: Option<String>,
    /// Last-mutation timestamp (unix-epoch milliseconds, as a string).
    #[serde(default)]
    pub updated_at: String,
    /// The notes of the last plan that finished, moved here at the end of
    /// the turn it finished in, so the thread's next turns still see them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task: Option<TaskNotes>,
}

/// The facts and blockers of one finished plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskNotes {
    /// Facts verified while the plan ran.
    #[serde(default)]
    pub facts: Vec<String>,
    /// What blocked the work while the plan ran.
    #[serde(default)]
    pub blockers: Vec<String>,
}

impl TaskNotes {
    /// Whether there is nothing to keep.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty() && self.blockers.is_empty()
    }
}

impl Scratchpad {
    /// Whether the scratchpad holds nothing, counting the last task's notes.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
            && self.blockers.is_empty()
            && self.approach.is_none()
            && self.last_task.as_ref().is_none_or(TaskNotes::is_empty)
    }
}

/// One write to a scratchpad, as the `todo` tool's note arguments carry it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScratchpadUpdate {
    /// Facts to add.
    pub facts: Vec<String>,
    /// Blockers to add.
    pub blockers: Vec<String>,
    /// The current approach; `Some("")` clears it.
    pub approach: Option<String>,
    /// Remove every fact, blocker or approach containing one of these
    /// (case-insensitive), so a wrong note does not stay.
    pub retract: Vec<String>,
}

impl ScratchpadUpdate {
    /// Whether the update changes nothing.
    pub fn is_empty(&self) -> bool {
        self.facts.is_empty()
            && self.blockers.is_empty()
            && self.approach.is_none()
            && self.retract.is_empty()
    }
}

/// A plan that finished during a turn, with the notes taken while it ran:
/// what [`store::close_finished_plan`](super::store::close_finished_plan) hands
/// the host to keep past the thread (OpenHuman writes it to long-term memory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedPlan {
    /// The thread the plan belongs to.
    pub thread_id: String,
    /// The plan: every step completed or cancelled.
    pub items: Vec<TodoItem>,
    /// The facts and blockers; never both empty.
    pub notes: TaskNotes,
}
