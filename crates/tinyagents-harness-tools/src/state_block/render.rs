//! Rendering the run-state block from the goal, the plan and the ledger.

use crate::goals::{ThreadGoal, ThreadGoalStatus};
use crate::scratchpad::Scratchpad;
use crate::todos::{TodoItem, TodoStatus};

use super::ledger::one_line;
use super::types::RunLedger;

/// First line of every block; also how tests and logs recognise one.
pub const STATE_BLOCK_HEADER: &str = "[run state]";
/// Longest goal objective shown, in characters.
const MAX_OBJECTIVE_CHARS: usize = 300;
/// Longest plan step shown, in characters.
const MAX_STEP_CHARS: usize = 160;
/// Plan steps shown before the rest are summarised as a count.
const MAX_STEPS_SHOWN: usize = 20;

/// The block for this request, or `None` when there is no goal, plan, note or
/// fact to show.
pub(super) fn render(
    goal: Option<&ThreadGoal>,
    pad: Option<&Scratchpad>,
    items: &[TodoItem],
    ledger: &RunLedger,
) -> Option<String> {
    let mut sections = Vec::new();
    if let Some(goal) = goal {
        sections.push(goal_line(goal));
    }
    if let Some(plan) = plan_section(items, ledger) {
        sections.push(plan);
    }
    if let Some(pad) = pad {
        sections.extend(notes_sections(pad));
    }
    if !ledger.failures.is_empty() {
        let mut lines = vec!["Recent failures (oldest first):".to_string()];
        for failure in &ledger.failures {
            lines.push(format!(
                "  - {}, {} `{}`: {}",
                age(ledger.round, failure.round),
                failure.tool,
                failure.summary,
                failure.error
            ));
        }
        sections.push(lines.join("\n"));
    }
    if let Some(nudge) = blocker_nudge(pad, ledger) {
        sections.push(nudge);
    }
    if !ledger.missing.is_empty() {
        let names: Vec<&str> = ledger
            .missing
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        sections.push(format!("Reported not found: {}", names.join(", ")));
    }
    if !ledger.files.is_empty() {
        sections.push(format!(
            "Files written this run: {}",
            ledger.files.join(", ")
        ));
    }
    if sections.is_empty() {
        return None;
    }
    Some(format!(
        "{STATE_BLOCK_HEADER} Rebuilt by the harness for this call from your goal, plan, notes \
         and tool results; it is not part of the conversation.\n{}",
        sections.join("\n")
    ))
}

/// The thread goal on one line, with its status and any budget left.
fn goal_line(goal: &ThreadGoal) -> String {
    let status = match goal.status {
        ThreadGoalStatus::Active => match goal.budget_remaining() {
            Some(remaining) => format!("active, ~{remaining} tokens of budget left"),
            None => "active".to_string(),
        },
        ThreadGoalStatus::Paused => {
            return format!(
                "Goal (paused): {}\n  If the user's message continues it, call goal_resume. If it starts a \
                 different task, call goal_set. Otherwise leave it paused.",
                one_line(&goal.objective, MAX_OBJECTIVE_CHARS)
            );
        }
        ThreadGoalStatus::BudgetLimited => "budget exhausted: summarise and stop".to_string(),
        ThreadGoalStatus::Complete => "complete".to_string(),
    };
    format!(
        "Goal ({status}): {}",
        one_line(&goal.objective, MAX_OBJECTIVE_CHARS)
    )
}

fn plan_section(items: &[TodoItem], ledger: &RunLedger) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let total = items.len();
    let header = match items
        .iter()
        .position(|item| item.status == TodoStatus::InProgress)
    {
        Some(index) => {
            let rounds = ledger.round.saturating_sub(ledger.step_since);
            format!(
                "Plan: step {} of {total} in progress ({rounds} tool round{} on it).",
                index + 1,
                if rounds == 1 { "" } else { "s" }
            )
        }
        None => match items.iter().position(|item| item.status.is_open()) {
            Some(index) => format!(
                "Plan: no step in progress; step {} of {total} is next.",
                index + 1
            ),
            None => format!("Plan: all {total} steps closed."),
        },
    };
    let mut lines = vec![header];
    let next_open = items
        .iter()
        .position(|item| item.status == TodoStatus::InProgress)
        .or_else(|| items.iter().position(|item| item.status.is_open()));
    let mut displayed: Vec<usize> = (0..items.len().min(MAX_STEPS_SHOWN)).collect();
    if let Some(next_open) = next_open
        && next_open >= MAX_STEPS_SHOWN
    {
        displayed.pop();
        displayed.push(next_open);
    }
    for index in displayed.iter().copied() {
        let item = &items[index];
        let mark = match item.status {
            TodoStatus::Completed => "[x]",
            TodoStatus::InProgress => "[>]",
            TodoStatus::Pending => "[ ]",
            TodoStatus::Cancelled => "[-]",
        };
        let mut line = format!(
            "  {mark} {}. {}",
            index + 1,
            one_line(&item.content, MAX_STEP_CHARS)
        );
        if let Some(reason) = item
            .reason
            .as_deref()
            .filter(|_| item.status == TodoStatus::Cancelled)
        {
            line.push_str(&format!(
                " (cancelled: {})",
                one_line(reason, MAX_STEP_CHARS)
            ));
        }
        lines.push(line);
    }
    let omitted = total - displayed.len();
    if omitted > 0 {
        lines.push(format!("  … {omitted} more"));
    }
    Some(lines.join("\n"))
}

/// The model's own notes: the approach it is on, what it verified, what failed.
/// Kept apart from the ledger below, which the harness derives itself.
fn notes_sections(pad: &Scratchpad) -> Vec<String> {
    let mut sections = Vec::new();
    if let Some(approach) = &pad.approach {
        sections.push(format!("Current approach (your note): {approach}"));
    }
    let list = |title: &str, notes: &[String]| {
        let mut lines = vec![format!("{title} (your notes, oldest first):")];
        lines.extend(notes.iter().map(|note| format!("  - {note}")));
        lines.join("\n")
    };
    if !pad.facts.is_empty() {
        sections.push(list("Verified facts", &pad.facts));
    }
    if !pad.blockers.is_empty() {
        sections.push(list("Blockers", &pad.blockers));
    }
    if let Some(last) = pad.last_task.as_ref().filter(|last| !last.is_empty()) {
        let mut lines = vec!["From your last finished task:".to_string()];
        lines.extend(last.facts.iter().map(|fact| format!("  - fact: {fact}")));
        lines.extend(
            last.blockers
                .iter()
                .map(|blocker| format!("  - blocker: {blocker}")),
        );
        sections.push(lines.join("\n"));
    }
    sections
}

/// Failures since the step began before the block asks for a blocker.
const BLOCKER_NUDGE_FAILURES: usize = 2;

/// A request to record what blocks the step: shown while the current step (or,
/// without a plan, the run) has [`BLOCKER_NUDGE_FAILURES`] or more recent
/// failures and the model has written no blocker since it began. The model
/// otherwise notes facts when it ticks a step and never what did not work.
fn blocker_nudge(pad: Option<&Scratchpad>, ledger: &RunLedger) -> Option<String> {
    let failing = ledger
        .failures
        .iter()
        .filter(|failure| failure.round >= ledger.step_since)
        .count();
    if failing < BLOCKER_NUDGE_FAILURES {
        return None;
    }
    let newest = pad.and_then(|pad| pad.blockers.last()).cloned();
    if ledger
        .blocker_anchor
        .as_ref()
        .is_some_and(|anchor| *anchor != newest)
    {
        return None;
    }
    let what = if ledger.step.is_some() {
        "This step keeps failing"
    } else {
        "Calls keep failing"
    };
    Some(format!(
        "{what} ({failing} recent failures). Record what blocks it before changing \
         course: todo {{\"blockers\": [\"what does not work and why\"], \"approach\": \
         \"what you try next\"}}."
    ))
}

/// How long ago round `then` was, counting the latest round as 1: the block is
/// rendered after the latest round's results are in.
fn age(now: usize, then: usize) -> String {
    match now.saturating_sub(then) + 1 {
        1 => "last round".to_string(),
        rounds => format!("{rounds} rounds ago"),
    }
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
