//! Rendering the run-state block from the goal, the plan and the ledger.

use crate::goals::{ThreadGoal, ThreadGoalStatus};
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

/// The block for this request, or `None` when there is no goal, plan or fact
/// to show.
pub(super) fn render(
    goal: Option<&ThreadGoal>,
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
        "{STATE_BLOCK_HEADER} Rebuilt by the harness for this call from your goal, plan and \
         tool results; it is not part of the conversation.\n{}",
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
