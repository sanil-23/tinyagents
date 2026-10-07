//! Turn and message lifecycle events for the superstep loop.
//!
//! The loop pushes to its working transcript from many places (assistant
//! replies, tool results, recovery nudges, steering, queued messages). Rather
//! than instrument each push, [`TurnTracker`] watches the transcript length and
//! announces whatever was appended since the last look, in order, at the points
//! the loop already treats as boundaries. That keeps every present and future
//! push site covered with no per-site code.

use crate::context::RunContext;
use crate::events::AgentEvent;
use crate::ids::CallId;
use crate::runtime::PayloadCapture;
use tinyinference_llm::message::Message;

/// Tracks which transcript messages have been announced and which turn is open.
#[derive(Debug)]
pub(super) struct TurnTracker {
    /// Messages `[0, announced)` have been announced (or are the seed input).
    announced: usize,
    /// Number of the most recently opened turn.
    turn: u32,
    /// The open turn and the transcript index it started at.
    open: Option<(u32, usize)>,
}

fn role_of(message: &Message) -> &'static str {
    match message {
        Message::System(_) => "system",
        Message::User(_) => "user",
        Message::Assistant(_) => "assistant",
        Message::Tool(_) => "tool",
        Message::Custom(_) => "custom",
    }
}

impl TurnTracker {
    /// A tracker for a transcript that starts with `seed_len` input messages,
    /// which are not announced.
    pub(super) fn new(seed_len: usize) -> Self {
        Self {
            announced: seed_len,
            turn: 0,
            open: None,
        }
    }

    /// Announces every message appended since the last call, in order.
    pub(super) fn flush<Ctx: Send + Sync>(
        &mut self,
        ctx: &RunContext<Ctx>,
        capture: PayloadCapture,
        messages: &[Message],
    ) {
        // A transcript that shrank (trimmed or replaced) re-bases the cursor.
        self.announced = self.announced.min(messages.len());
        for (index, message) in messages.iter().enumerate().skip(self.announced) {
            let (call_id, captured) = match message {
                Message::Tool(tool) => (Some(CallId::new(tool.tool_call_id.clone())), capture.tool_io),
                _ => (None, capture.model_io),
            };
            ctx.emit(AgentEvent::MessageAppended {
                role: role_of(message).to_string(),
                index,
                call_id,
                message: captured
                    .then(|| serde_json::to_value(message).unwrap_or(serde_json::Value::Null)),
            });
        }
        self.announced = messages.len();
    }

    /// Opens the next turn, first announcing pending messages and closing any
    /// turn still open (a recovery retry re-enters the model call without
    /// finishing its predecessor). Returns the new turn number.
    pub(super) fn start_turn<Ctx: Send + Sync>(
        &mut self,
        ctx: &RunContext<Ctx>,
        capture: PayloadCapture,
        messages: &[Message],
    ) -> u32 {
        self.close_turn(ctx, capture, messages);
        self.turn += 1;
        self.open = Some((self.turn, messages.len()));
        ctx.emit(AgentEvent::TurnStarted { turn: self.turn });
        self.turn
    }

    /// Announces pending messages and closes the open turn, if any, reporting
    /// the tool results it added to the transcript.
    pub(super) fn close_turn<Ctx: Send + Sync>(
        &mut self,
        ctx: &RunContext<Ctx>,
        capture: PayloadCapture,
        messages: &[Message],
    ) {
        self.flush(ctx, capture, messages);
        let Some((turn, start)) = self.open.take() else {
            return;
        };
        let tool_call_ids: Vec<CallId> = messages
            .get(start..)
            .unwrap_or_default()
            .iter()
            .filter_map(|message| match message {
                Message::Tool(tool) => Some(CallId::new(tool.tool_call_id.clone())),
                _ => None,
            })
            .collect();
        tracing::debug!(
            target: "tinyagents::agent_loop",
            run_id = %ctx.run_id(),
            turn,
            tool_results = tool_call_ids.len(),
            "[agent_loop] turn completed"
        );
        ctx.emit(AgentEvent::TurnCompleted {
            turn,
            tool_result_count: tool_call_ids.len(),
            tool_call_ids,
        });
    }
}
