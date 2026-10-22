//! [`TodoSequenceMiddleware`]: keeps a run working through its plan one step
//! at a time.
//!
//! The `todo` tool keeps the list's own order (see `sequence`). What it cannot
//! see is the work between ticks, so the middleware holds three rules at the
//! run's edges, all keyed to the thread's stored plan and all inert while the
//! thread has no plan with open steps:
//!
//! - **No work without an open step** (`before_tool`): while the plan has open
//!   steps and none is in progress, every tool except `todo` is refused with
//!   the step to start. The tool keeps a step in progress whenever one is
//!   open, so this catches a list written before that rule, not a fresh one.
//! - **No finish with open steps** (`after_model`): the first final answer of
//!   a run whose plan still has pending or in-progress steps is held once with
//!   a harness instruction to close or explain them. It rides
//!   [`ModelResponse::continue_turn`], the same mechanism as
//!   [`VerifyBeforeFinishMiddleware`](tinyagents_harness::middleware::VerifyBeforeFinishMiddleware),
//!   which skips a response that is already continued: register this
//!   middleware *after* that one (`after_model` runs in reverse registration
//!   order) and the plan check comes first.
//! - **A backstop reminder** (`after_model` + `after_tool`): after
//!   [`reminder_rounds`](TodoSequenceMiddleware::with_reminder_rounds) tool
//!   rounds with the same step in progress and no `todo` call, one note is
//!   appended to that round's first tool result naming the step and how to
//!   tick it. Once per step; a `todo` call restarts the count.
//! - **An ignored reminder becomes a gate** (`before_tool`): at twice that
//!   many rounds without a `todo` call, every other tool is refused until the
//!   model calls `todo` (`{"complete": N}`, or a bare `{}` to confirm it is
//!   still on the step).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tinyagents_harness::context::RunContext;
use tinyagents_harness::error::{Result, TinyAgentsError};
use tinyagents_harness::events::AgentEvent;
use tinyagents_harness::middleware::{
    AgentRun, Middleware, ToolInvocationIdentity, wrap_harness_instruction,
};
use tinyagents_harness::store::Store;
use tinyagents_harness::tinyinference_llm::{model::ModelResponse, tool::ToolCall};
use tinytools::{ToolContent, ToolResult};

use super::sequence;
use super::store;
use super::tool::TODO_TOOL_NAME;
use super::types::{TodoItem, TodoStatus};

/// Tool rounds a step may stay in progress without a `todo` call before the
/// reminder.
pub const DEFAULT_REMINDER_ROUNDS: usize = 8;

/// The held answer needs one more call to be replaced.
const MIN_REMAINING_MODEL_CALLS: usize = 2;

/// ponytail: a run that ends without `after_agent`/`on_error` (cancelled)
/// leaves its entry behind; past this many the map is cleared. Prune by run
/// liveness if long-lived hosts ever hit it.
const MAX_RETAINED_RUNS: usize = 1_024;

/// Per-run bookkeeping, keyed by [`RunContext::instance_id`].
#[derive(Default)]
struct RunState {
    finish_held: bool,
    /// The step in progress at the last tool round, as (number, text).
    step: Option<(usize, String)>,
    rounds_on_step: usize,
    reminded: HashSet<(usize, String)>,
    pending_note: Option<String>,
}

/// Holds a run to its plan's order. See the module docs.
pub struct TodoSequenceMiddleware {
    store: Arc<dyn Store>,
    reminder_rounds: usize,
    /// The plan key for a run with no thread id (see `with_fallback_thread`).
    fallback_thread: Option<String>,
    runs: Mutex<HashMap<u64, RunState>>,
}

impl TodoSequenceMiddleware {
    /// A middleware reading each thread's plan from `store` (the store the
    /// `todo` tool writes).
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            reminder_rounds: DEFAULT_REMINDER_ROUNDS,
            fallback_thread: None,
            runs: Mutex::default(),
        }
    }

    /// Reads the plan under `key` for a run that has no thread id. Pass the
    /// scope the host's `todo` tool falls back to in that case (OpenHuman: the
    /// session id); without it a headless run's rules never see its plan.
    pub fn with_fallback_thread(mut self, key: impl Into<String>) -> Self {
        self.fallback_thread = Some(key.into());
        self
    }

    /// Remind after `rounds` tool rounds on one step (at least 1).
    pub fn with_reminder_rounds(mut self, rounds: usize) -> Self {
        self.reminder_rounds = rounds.max(1);
        self
    }

    /// The thread's plan; empty for a run without a thread or a plan, or when
    /// the store cannot be read (the rules then stay out of the way). Takes the
    /// thread id, not the context, so no context borrow is held across the read.
    async fn items(&self, thread_id: Option<&str>) -> Vec<TodoItem> {
        let Some(thread_id) = thread_id else {
            return Vec::new();
        };
        match store::get(&self.store, thread_id).await {
            Ok(list) => list.map(|list| list.items).unwrap_or_default(),
            Err(error) => {
                tracing::debug!(%error, "[tinyagents::mw] todo_sequence: plan unreadable, not enforcing");
                Vec::new()
            }
        }
    }

    fn with_run<T>(&self, instance: u64, f: impl FnOnce(&mut RunState) -> T) -> T {
        let mut runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if runs.len() >= MAX_RETAINED_RUNS && !runs.contains_key(&instance) {
            runs.clear();
        }
        f(runs.entry(instance).or_default())
    }

    /// Counts a tool round toward the reminder; `ticked` when the round
    /// called `todo`.
    async fn count_round(&self, instance: u64, thread_id: Option<&str>, ticked: bool) {
        if ticked {
            self.with_run(instance, |run| run.rounds_on_step = 0);
            return;
        }
        let items = self.items(thread_id).await;
        let current = items
            .iter()
            .position(|item| item.status == TodoStatus::InProgress)
            .map(|index| (index + 1, items[index].content.clone()));
        let rounds = self.reminder_rounds;
        self.with_run(instance, |run| {
            let Some(step) = current else {
                run.step = None;
                run.rounds_on_step = 0;
                return;
            };
            if run.step.as_ref() != Some(&step) {
                run.step = Some(step.clone());
                run.rounds_on_step = 0;
            }
            run.rounds_on_step += 1;
            if run.rounds_on_step >= rounds && run.reminded.insert(step.clone()) {
                tracing::info!(
                    step = step.0,
                    rounds,
                    "[tinyagents::mw] todo_sequence: step in progress for {rounds} rounds without a tick"
                );
                run.pending_note = Some(wrap_harness_instruction(&format!(
                    "Step {} \"{}\" has been in progress for {rounds} rounds. If it's done, call \
                     todo {{\"complete\": {}}}; if you are still on it, call todo {{}} to confirm. \
                     After {} rounds without a plan update, other tools are refused.",
                    step.0,
                    step.1,
                    step.0,
                    rounds * 2
                )));
            }
        });
    }

    /// Why a final answer must not be held, or `None` when it may be.
    fn skip_finish<C>(ctx: &RunContext<C>, response: &ModelResponse) -> Option<&'static str> {
        // The spellings `tinyagents_harness`'s `is_length_stop` matches.
        if matches!(
            response.finish_reason.as_deref(),
            Some("length" | "max_tokens" | "MAX_TOKENS")
        ) {
            return Some("truncated");
        }
        if response.text().trim().is_empty() {
            return Some("empty_answer");
        }
        if response.continue_turn.is_some() {
            return Some("already_continued");
        }
        if ctx.limits.remaining_model_calls() < MIN_REMAINING_MODEL_CALLS {
            return Some("model_call_budget");
        }
        None
    }
}

impl TodoSequenceMiddleware {
    /// The run's plan key, owned so no context borrow crosses an `.await`: its
    /// thread id, else the fallback key.
    fn thread_of<C>(&self, ctx: &RunContext<C>) -> Option<String> {
        ctx.thread_id()
            .map(|thread_id| thread_id.as_str().to_owned())
            .or_else(|| self.fallback_thread.clone())
    }
}

/// The instruction that holds a final answer while steps are open.
fn finish_instruction(items: &[TodoItem]) -> Option<String> {
    let open: Vec<String> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status.is_open())
        .map(|(index, item)| {
            let state = if item.status == TodoStatus::InProgress {
                " (in progress)"
            } else {
                ""
            };
            format!("step {} \"{}\"{state}", index + 1, item.content)
        })
        .collect();
    if open.is_empty() {
        return None;
    }
    Some(wrap_harness_instruction(&format!(
        "Your plan still has open steps: {}. Before finishing, complete each step whose work is \
         done with todo {{\"complete\": N}}, cancel each step you will not do with todo \
         {{\"cancel\": N, \"reason\": \"...\"}}, or say in your answer why it is left open. Then \
         give your final answer in full, since it replaces your previous reply.",
        open.join(", ")
    )))
}

#[async_trait]
impl<S: Send + Sync, C: Send + Sync> Middleware<S, C> for TodoSequenceMiddleware {
    fn name(&self) -> &str {
        "todo_sequence"
    }

    async fn before_tool(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        call: &mut ToolCall,
    ) -> Result<()> {
        if call.name == TODO_TOOL_NAME {
            return Ok(());
        }
        // A reminder the model ignored for another `reminder_rounds` rounds:
        // any `todo` call (even a bare read) resets the count and lifts this.
        let stale = self.reminder_rounds * 2;
        if let Some((step, text)) = self.with_run(ctx.instance_id(), |run| {
            run.step.clone().filter(|_| run.rounds_on_step >= stale)
        }) {
            tracing::info!(
                tool = %call.name,
                step,
                "[tinyagents::mw] todo_sequence: refusing a tool, plan not updated for {stale} rounds"
            );
            return Err(TinyAgentsError::ToolFailed(format!(
                "Update your plan first: step {step} \"{text}\" has been in progress for {stale} \
                 rounds without a todo call. Call todo {{\"complete\": {step}}} if it is done, or \
                 todo {{}} if you are still on it, then retry."
            )));
        }
        let thread_id = self.thread_of(ctx);
        let items = self.items(thread_id.as_deref()).await;
        if items
            .iter()
            .any(|item| item.status == TodoStatus::InProgress)
        {
            return Ok(());
        }
        let Some(next) = sequence::first_open(&items) else {
            return Ok(());
        };
        let step = next + 1;
        tracing::info!(
            tool = %call.name,
            step,
            "[tinyagents::mw] todo_sequence: refusing a tool while no plan step is in progress"
        );
        Err(TinyAgentsError::ToolFailed(format!(
            "Start the next step of your plan first: step {step} \"{}\" is not in progress. Call \
             todo {{\"complete\": {step}}} if it is already done, or re-send the plan with todo to \
             start it.",
            items[next].content
        )))
    }

    async fn after_model(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        response: &mut ModelResponse,
    ) -> Result<()> {
        let instance = ctx.instance_id();
        let thread_id = self.thread_of(ctx);
        let calls = response.tool_calls();
        if !calls.is_empty() {
            let ticked = calls.iter().any(|call| call.name == TODO_TOOL_NAME);
            self.count_round(instance, thread_id.as_deref(), ticked)
                .await;
            return Ok(());
        }
        if self.with_run(instance, |run| run.finish_held) {
            return Ok(());
        }
        if let Some(reason) = Self::skip_finish(ctx, response) {
            tracing::debug!(
                reason,
                "[tinyagents::mw] todo_sequence: finish check skipped"
            );
            return Ok(());
        }
        let Some(instruction) = finish_instruction(&self.items(thread_id.as_deref()).await) else {
            return Ok(());
        };
        self.with_run(instance, |run| run.finish_held = true);
        response.continue_turn = Some(instruction);
        tracing::info!(
            "[tinyagents::mw] todo_sequence: holding a final answer while plan steps are open"
        );
        ctx.emit(AgentEvent::ControlApplied {
            control: "todo_sequence".to_string(),
            detail: "final answer held once: the plan still has open steps".to_string(),
        });
        Ok(())
    }

    async fn after_tool(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        invocation: &ToolInvocationIdentity,
        result: &mut ToolResult,
    ) -> Result<()> {
        if invocation.tool_name() == TODO_TOOL_NAME {
            return Ok(());
        }
        if let Some(note) = self.with_run(ctx.instance_id(), |run| run.pending_note.take()) {
            result.content.push(ToolContent::Text {
                text: format!("\n{note}"),
            });
        }
        Ok(())
    }

    async fn on_error(&self, ctx: &mut RunContext<C>, _error: &TinyAgentsError) -> Result<()> {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&ctx.instance_id());
        }
        Ok(())
    }

    async fn after_agent(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        _run: &mut AgentRun,
    ) -> Result<()> {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&ctx.instance_id());
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_tests.rs"]
mod tests;
