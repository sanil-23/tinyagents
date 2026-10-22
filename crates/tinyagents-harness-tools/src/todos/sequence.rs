//! The order a plan's steps run in.
//!
//! Steps run one at a time, top to bottom. A step may start only once every
//! step above it is closed (`completed` or `cancelled`), and may be completed
//! only if it was the one in progress or every step above it is closed. A
//! write that breaks the order is refused with the step to finish first; the
//! rules that cannot be broken by accident are applied instead of refused:
//!
//! - a list with open steps always has one in progress: whenever none is, the
//!   first open step starts, so creating a plan starts step 1 and completing
//!   a step starts the next;
//! - a whole-list write with several steps `in_progress` keeps the first and
//!   sets the rest back to pending.
//!
//! Every function here works on the item list alone and returns the notes the
//! model should read (what was started or fixed), or the refusal. Steps are
//! numbered from 1 in every message, the way the model addresses them.

use super::types::{TodoItem, TodoStatus};

/// Applies a whole-list write on top of `previous` (the list it replaces).
///
/// A step that was already completed, or that was the one in progress, may
/// stay or become completed wherever it sits; any other step that is
/// `in_progress` or newly `completed` needs every step above it closed.
pub(super) fn apply_write(
    previous: &[TodoItem],
    items: &mut [TodoItem],
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let mut demoted = Vec::new();
    let mut seen_in_progress = false;
    for (index, item) in items.iter_mut().enumerate() {
        if item.status == TodoStatus::InProgress {
            if seen_in_progress {
                item.status = TodoStatus::Pending;
                demoted.push((index + 1).to_string());
            }
            seen_in_progress = true;
        }
    }
    if !demoted.is_empty() {
        notes.push(format!(
            "Only one step may be in progress: kept the first and set step(s) {} back to pending.",
            demoted.join(", ")
        ));
    }
    for (index, item) in items.iter().enumerate() {
        let ordered = match item.status {
            TodoStatus::InProgress => true,
            TodoStatus::Completed => !previous.iter().any(|old| {
                old.content == item.content
                    && matches!(old.status, TodoStatus::Completed | TodoStatus::InProgress)
            }),
            TodoStatus::Pending | TodoStatus::Cancelled => false,
        };
        if ordered {
            ensure_earlier_closed(items, index)?;
        }
    }
    notes.extend(start_next(items));
    Ok(notes)
}

/// Marks step `step` (1-based) completed and starts the next open step.
pub(super) fn complete(items: &mut [TodoItem], step: usize) -> Result<Vec<String>, String> {
    let index = index_of(items, step)?;
    let mut notes = Vec::new();
    match items[index].status {
        TodoStatus::Completed => notes.push(format!("Step {step} was already completed.")),
        TodoStatus::Cancelled => {
            return Err(format!(
                "Step {step} was cancelled; re-send the plan with `todos` to reopen it."
            ));
        }
        TodoStatus::InProgress => items[index].status = TodoStatus::Completed,
        TodoStatus::Pending => {
            ensure_earlier_closed(items, index)?;
            items[index].status = TodoStatus::Completed;
        }
    }
    notes.extend(start_next(items));
    Ok(notes)
}

/// Completes several steps (1-based) top to bottom, all or none: each must be
/// allowed by [`complete`] once the ones before it in the list are done, so
/// `[1, 2, 3]` closes three steps in order and `[1, 3]` with step 2 open is
/// refused. Only the last step's "now in progress" note is kept.
pub(super) fn complete_steps(
    items: &mut [TodoItem],
    steps: &[usize],
) -> Result<Vec<String>, String> {
    let mut steps = steps.to_vec();
    steps.sort_unstable();
    steps.dedup();
    let Some(&last) = steps.last() else {
        return Err("`complete` needs at least one step number.".to_string());
    };
    let mut notes = Vec::new();
    for step in steps {
        let step_notes = complete(items, step)?;
        if step == last {
            notes.extend(step_notes);
        } else {
            notes.extend(
                step_notes
                    .into_iter()
                    .filter(|note| !note.ends_with("is now in progress.")),
            );
        }
    }
    Ok(notes)
}

/// Cancels `cancel` (all with `reason`), then completes `complete`, in one
/// write, all or none. Cancelling first lets "drop step 2, finish step 3" and
/// "finish steps 4-5, drop step 6" both go through. Only the last "now in
/// progress" note is kept.
pub(super) fn close_steps(
    items: &mut [TodoItem],
    complete: &[usize],
    cancel: &[usize],
    reason: &str,
) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let mut cancel = cancel.to_vec();
    cancel.sort_unstable();
    cancel.dedup();
    for step in cancel {
        notes.extend(self::cancel(items, step, reason)?);
    }
    if !complete.is_empty() {
        notes.extend(complete_steps(items, complete)?);
    }
    let last_start = notes
        .iter()
        .rposition(|note| note.ends_with("is now in progress."));
    Ok(notes
        .into_iter()
        .enumerate()
        .filter(|(index, note)| {
            !note.ends_with("is now in progress.") || Some(*index) == last_start
        })
        .map(|(_, note)| note)
        .collect())
}

/// Marks step `step` (1-based) cancelled with `reason` and, when it was the
/// step in progress, starts the next open one. Any open step may be
/// cancelled: dropping a later step is planning, not skipping ahead.
pub(super) fn cancel(
    items: &mut [TodoItem],
    step: usize,
    reason: &str,
) -> Result<Vec<String>, String> {
    let index = index_of(items, step)?;
    let reason = reason.trim();
    if reason.is_empty() {
        return Err("`cancel` needs a `reason`.".to_string());
    }
    let item = &mut items[index];
    if !item.status.is_open() {
        return Err(format!("Step {step} is already {}.", item.status.as_str()));
    }
    item.status = TodoStatus::Cancelled;
    item.reason = Some(reason.to_string());
    Ok(start_next(items).into_iter().collect())
}

/// The first open step (0-based), the one that should be in progress.
pub(super) fn first_open(items: &[TodoItem]) -> Option<usize> {
    items.iter().position(|item| item.status.is_open())
}

/// Starts the first open step when no step is in progress, returning a note
/// naming it, or one saying the plan is finished when nothing is open.
fn start_next(items: &mut [TodoItem]) -> Option<String> {
    if items
        .iter()
        .any(|item| item.status == TodoStatus::InProgress)
    {
        return None;
    }
    match first_open(items) {
        Some(index) => {
            items[index].status = TodoStatus::InProgress;
            Some(format!(
                "Step {} \"{}\" is now in progress.",
                index + 1,
                items[index].content
            ))
        }
        None if !items.is_empty() => Some("Every step is closed.".to_string()),
        None => None,
    }
}

/// Names every open step above `index`, so a model that skipped ahead closes
/// them all instead of ticking only the first and moving on.
fn ensure_earlier_closed(items: &[TodoItem], index: usize) -> Result<(), String> {
    let open: Vec<String> = items[..index]
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status.is_open())
        .map(|(i, _)| (i + 1).to_string())
        .collect();
    match open.as_slice() {
        [] => Ok(()),
        [one] => Err(format!("Finish or cancel step {one} first.")),
        many => Err(format!(
            "Finish or cancel steps {} first, in order: todo {{\"complete\": [N, ...]}} for the ones \
             that are done, or todo {{\"cancel\": N, \"reason\": \"...\"}}.",
            many.join(", ")
        )),
    }
}

fn index_of(items: &[TodoItem], step: usize) -> Result<usize, String> {
    if step == 0 || step > items.len() {
        return Err(format!(
            "There is no step {step}: the plan has {} step(s), numbered from 1.",
            items.len()
        ));
    }
    Ok(step - 1)
}

#[cfg(test)]
#[path = "sequence_tests.rs"]
mod tests;
