//! [`StateBlockMiddleware`]'s hooks.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tinyagents_harness::context::RunContext;
use tinyagents_harness::error::{Result, TinyAgentsError};
use tinyagents_harness::middleware::{
    AgentRun, Middleware, ToolInvocationIdentity, push_ephemeral_instruction,
};
use tinyagents_harness::store::Store;
use tinyinference_llm::message::{ContentBlock, Message};
use tinyinference_llm::model::{ModelRequest, ModelResponse};
use tinyinference_llm::tool::ToolCall;
use tinytools::ToolResult;

use crate::todos::{TodoItem, TodoStatus};

use super::ledger;
use super::render::{STATE_BLOCK_HEADER, render};
use super::types::{CallRecord, RunLedger, RunState, StateBlockMiddleware};

/// The `todo` tool's name: its own result already shows the plan.
const TODO_TOOL_NAME: &str = "todo";
/// ponytail: a run that ends without `after_agent`/`on_error` (cancelled)
/// leaves its entry behind; past this many the map is cleared.
const MAX_RETAINED_RUNS: usize = 1_024;

impl StateBlockMiddleware {
    /// A middleware reading each thread's goal and plan from `store` (the
    /// store the goal and `todo` tools write).
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            fallback_thread: None,
            runs: Mutex::default(),
        }
    }

    /// Reads the goal and plan under `key` for a run that has no thread id.
    /// Pass the scope the host's `todo` tool uses in that case, so a headless
    /// run (no chat thread) still sees the plan it wrote.
    pub fn with_fallback_thread(mut self, key: impl Into<String>) -> Self {
        self.fallback_thread = Some(key.into());
        self
    }

    fn with_run<T>(&self, instance: u64, f: impl FnOnce(&mut RunLedger) -> T) -> T {
        let mut runs = self
            .runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if runs.len() >= MAX_RETAINED_RUNS && !runs.contains_key(&instance) {
            runs.clear();
        }
        f(runs.entry(instance).or_default())
    }

    fn forget(&self, instance: u64) {
        self.runs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&instance);
    }

    /// The thread's saved state; empty without a thread, or when the store
    /// cannot be read (the block then shows the run's facts alone).
    async fn saved(&self, thread_id: Option<&str>) -> RunState {
        let Some(thread_id) = thread_id else {
            return RunState::default();
        };
        RunState::load(&self.store, thread_id)
            .await
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "[tinyagents::mw] state_block: saved state unreadable");
                RunState::default()
            })
    }
}

/// Takes the block out of whichever message carries it, dropping a message that
/// held nothing else (a tail system message or trailing user turn of its own).
fn remove_block(request: &mut ModelRequest) {
    request.messages.retain_mut(|message| {
        let content = match message {
            Message::System(system) => &mut system.content,
            Message::User(user) => &mut user.content,
            Message::Tool(tool) => &mut tool.content,
            _ => return true,
        };
        let before = content.len();
        content.retain(
            |part| !matches!(part, ContentBlock::Text(text) if text.contains(STATE_BLOCK_HEADER)),
        );
        !(content.len() != before && content.is_empty())
    });
}

/// Notes the step in progress, so the block can say how long it has run, and
/// the model's newest blocker when that step began.
fn track_step(ledger: &mut RunLedger, items: &[TodoItem], newest_blocker: Option<&String>) {
    let current = items
        .iter()
        .position(|item| item.status == TodoStatus::InProgress)
        .map(|index| (index + 1, items[index].content.clone()));
    if ledger.step != current {
        ledger.step = current;
        ledger.step_since = ledger.round;
        ledger.blocker_anchor = Some(newest_blocker.cloned());
    }
    if ledger.blocker_anchor.is_none() {
        ledger.blocker_anchor = Some(newest_blocker.cloned());
    }
}

#[async_trait]
impl<S: Send + Sync, C: Send + Sync> Middleware<S, C> for StateBlockMiddleware {
    fn name(&self) -> &str {
        "state_block"
    }

    /// Its ledger must see every call, whatever another middleware decides.
    fn is_observer(&self) -> bool {
        true
    }

    async fn before_model_dispatch(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        request: &mut ModelRequest,
    ) -> Result<()> {
        let thread_id = ctx
            .thread_id()
            .map(|id| id.as_str().to_owned())
            .or_else(|| self.fallback_thread.clone());
        let saved = self.saved(thread_id.as_deref()).await;
        let block = self.with_run(ctx.instance_id(), |ledger| {
            ledger::age_out(ledger);
            track_step(
                ledger,
                &saved.todos,
                saved.notes.as_ref().and_then(|pad| pad.blockers.last()),
            );
            ledger.staged = render(
                saved.goal.as_ref(),
                saved.notes.as_ref(),
                &saved.todos,
                ledger,
            );
            ledger.staged.clone()
        });
        if let Some(block) = block {
            tracing::trace!(chars = block.len(), "[tinyagents::mw] state_block: added");
            push_ephemeral_instruction(request, block, ctx.model_profile.as_ref());
        }
        Ok(())
    }

    async fn after_model_rewrite(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        request: &mut ModelRequest,
    ) -> Result<()> {
        let Some(block) = self.with_run(ctx.instance_id(), |ledger| ledger.staged.clone()) else {
            return Ok(());
        };
        // `before_model` runs after `before_model_dispatch`, so a middleware that
        // pushes its own note there lands after the block. Keep the block last:
        // it is the current state, and the tail is where a model attends most.
        if request.messages.last().is_some_and(|message| match message {
            Message::System(system) => system.content.last().is_some_and(
                |part| matches!(part, ContentBlock::Text(text) if text.trim_end().ends_with(&block)),
            ),
            Message::User(user) => user.content.last().is_some_and(
                |part| matches!(part, ContentBlock::Text(text) if text.trim_end().ends_with(&block)),
            ),
            Message::Tool(tool) => tool.content.last().is_some_and(
                |part| matches!(part, ContentBlock::Text(text) if text.trim_end().ends_with(&block)),
            ),
            _ => false,
        }) {
            return Ok(());
        }
        remove_block(request);
        push_ephemeral_instruction(request, block, ctx.model_profile.as_ref());
        Ok(())
    }

    async fn after_model(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        response: &mut ModelResponse,
    ) -> Result<()> {
        if !response.tool_calls().is_empty() {
            self.with_run(ctx.instance_id(), |ledger| ledger.round += 1);
        }
        Ok(())
    }

    async fn before_tool(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        call: &mut ToolCall,
    ) -> Result<()> {
        if call.name != TODO_TOOL_NAME {
            let record = ledger::call_record(call);
            self.with_run(ctx.instance_id(), |ledger| {
                ledger.calls.insert(call.id.clone(), record);
            });
        }
        Ok(())
    }

    async fn after_tool(
        &self,
        _ctx: &mut RunContext<C>,
        _state: &S,
        _invocation: &ToolInvocationIdentity,
        _result: &mut ToolResult,
    ) -> Result<()> {
        Ok(())
    }

    async fn after_tool_output_screened(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        invocation: &ToolInvocationIdentity,
        result: &ToolResult,
        model_output: &str,
    ) -> Result<()> {
        if invocation.tool_name() == TODO_TOOL_NAME {
            return Ok(());
        }
        let is_error = result.is_error;
        self.with_run(ctx.instance_id(), |ledger| {
            // A call refused before `before_tool` reached this middleware has
            // no record; its name is all the ledger knows.
            let record = ledger
                .calls
                .remove(invocation.call_id().as_str())
                .unwrap_or_else(|| CallRecord {
                    tool: invocation.tool_name().to_string(),
                    summary: String::new(),
                    command: None,
                    written_paths: Vec::new(),
                });
            ledger::record_result(ledger, record, is_error, model_output);
        });
        Ok(())
    }

    async fn observe_nested_result(
        &self,
        ctx: &RunContext<C>,
        _state: &S,
        call: &ToolCall,
        result: &ToolResult,
    ) {
        if call.name == TODO_TOOL_NAME {
            return;
        }
        let record = ledger::call_record(call);
        // Nested tool results do not pass through finish_tool_call's host
        // output screening hook. Keep their failure fact, but never retain
        // their raw content in the model-facing ledger.
        let output = if result.is_error {
            "nested tool returned an error"
        } else {
            ""
        };
        self.with_run(ctx.instance_id(), |ledger| {
            ledger::record_result(ledger, record, result.is_error, output);
        });
    }

    async fn on_error(&self, ctx: &mut RunContext<C>, _error: &TinyAgentsError) -> Result<()> {
        self.forget(ctx.instance_id());
        Ok(())
    }

    async fn after_agent(
        &self,
        ctx: &mut RunContext<C>,
        _state: &S,
        _run: &mut AgentRun,
    ) -> Result<()> {
        self.forget(ctx.instance_id());
        Ok(())
    }
}

#[cfg(test)]
#[path = "middleware_tests.rs"]
mod tests;
