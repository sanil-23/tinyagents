//! `todo` — the harness [`Tool`] over the per-thread todo list.
//!
//! One call writes the whole list: `{"todos": [{"content", "status"}]}`, and
//! starts its first open step. After that the model ticks steps off in order:
//! `{"complete": N}` completes step N (1-based) and starts the next,
//! `{"cancel": N, "reason": "..."}` drops one. The order rules are the
//! `sequence` module's. No arguments reads the current list. The list is bound to the
//! caller's [`ToolExecutionContext::thread_id`] (never a tool argument), so a
//! model can't address another thread's list; the bare [`Tool::call`] entry
//! point (no context) errors. Returns the updated items plus a markdown
//! rendering.
//!
//! The same call (or one on its own) takes the model's notes for the thread's
//! [scratchpad](crate::scratchpad): `facts`, `blockers`, `approach` and
//! `retract`. They are written first, then the plan arguments run as if the
//! notes were not there; notes alone read the list.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::store;
use super::types::{TodoItem, TodoStatus, TodosSnapshot, parse_status};
use crate::scratchpad::{Scratchpad, ScratchpadUpdate, store as scratchpad_store};
use tinyagents_harness::error::{Result, TinyAgentsError};
use tinyagents_harness::store::Store;
use tinytools::{Tool, ToolPolicy, ToolResult, ToolRunContext, ToolSideEffects};

pub(super) const TODO_TOOL_NAME: &str = "todo";

/// Paid on every request of every agent that carries the tool, so each rule
/// is stated once, tersely. The rules themselves are load-bearing: see the
/// `description_states_…` test for the failure each one prevents.
const TODO_DESCRIPTION: &str = "This thread's plan, for work with 3+ steps. Write it once with \
    `todos` (the complete list; it replaces the old one, and step 1 starts). Steps run one at a \
    time, in order. Tick a step with `{\"complete\": N}` (N from 1) only after its work has run \
    and its result is in the conversation, before starting the next: that call starts step N+1 \
    (several finished steps: `{\"complete\": [N, M]}`). \
    Drop a step with `{\"cancel\": N, \"reason\": \"...\"}` (a list too; it may go with \
    `complete`). No arguments reads the list. \
    Ticking is bookkeeping, not work: in the same response (or your next turn, if you cannot call \
    tools in parallel) do the next step unless the list is finished. Replace the list at most \
    once per assistant response. Notes, in any call: `facts` (what a result proved), `blockers` \
    (what does not work or must be avoided here, and why), `approach` (what you are trying now), `retract` (text of a note that \
    was wrong). They stay in [run state] on every call; write one when you learn it.";

/// The note arguments, taken out before the plan arguments are read.
const NOTE_ARGS: [&str; 4] = ["facts", "blockers", "approach", "retract"];

/// The `todo` harness [`Tool`], backed by a [`Store`](tinyagents_harness::store::Store).
pub struct TodoTool {
    store: Arc<dyn Store>,
}

/// One item as the model writes it. `status` accepts the Claude-style
/// `pending` / `in_progress` / `completed` plus the aliases [`parse_status`]
/// knows (`todo`, `done`, ...).
///
/// `content` is canonical, but models also name the item's text `title`,
/// `item`, `text`, `task` or `description` (all seen in production). Each is
/// its own field rather than a serde alias so an item carrying two of them is
/// not a "duplicate field" error: the first non-blank one in
/// [`TodoArg::text`]'s order wins. Unknown keys (`id`, `priority`) are
/// ignored.
#[derive(Deserialize)]
struct TodoArg {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    item: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

impl TodoArg {
    /// The item's text: `content`, else the first non-blank alias.
    fn text(&self) -> &str {
        [
            &self.content,
            &self.title,
            &self.item,
            &self.text,
            &self.task,
            &self.description,
        ]
        .into_iter()
        .filter_map(|field| field.as_deref().map(str::trim))
        .find(|text| !text.is_empty())
        .unwrap_or_default()
    }
}

impl TodoTool {
    /// Creates the `todo` tool backed by `store`.
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }

    async fn dispatch(&self, thread_id: &str, args: &Value) -> Result<TodoOutcome> {
        let Some(args) = args.as_object() else {
            return Ok(TodoOutcome::Error(
                "arguments must be an object".to_string(),
            ));
        };
        let (update, args) = match split_notes(args) {
            Ok(split) => split,
            Err(message) => return Ok(TodoOutcome::Error(message)),
        };
        let pad = if update.is_empty() {
            None
        } else {
            match scratchpad_store::apply(&self.store, thread_id, &update).await {
                Ok(pad) => Some(pad),
                Err(TinyAgentsError::Validation(message)) => {
                    return Ok(TodoOutcome::Error(message));
                }
                Err(e) => return Ok(TodoOutcome::Error(e.to_string())),
            }
        };
        let before = closed_steps(&self.store, thread_id).await;
        let mut outcome = self.dispatch_plan(thread_id, &args).await?;
        if let TodoOutcome::Ok(payload) = &mut outcome
            && update.facts.is_empty()
            && update.blockers.is_empty()
        {
            note_unrecorded_closes(payload, &before);
        }
        Ok(match (outcome, pad) {
            (TodoOutcome::Ok(mut payload), Some(pad)) => {
                payload["scratchpad"] = notes_payload(&pad);
                TodoOutcome::Ok(payload)
            }
            (TodoOutcome::Error(message), Some(_)) => {
                TodoOutcome::Error(format!("{message} (your notes were saved)"))
            }
            (outcome, None) => outcome,
        })
    }

    async fn dispatch_plan(
        &self,
        thread_id: &str,
        args: &Map<String, Value>,
    ) -> Result<TodoOutcome> {
        let unknown = |allowed: &[&str]| {
            let extra: Vec<&String> = args
                .keys()
                .filter(|key| !allowed.contains(&key.as_str()))
                .collect();
            (!extra.is_empty()).then(|| {
                TodoOutcome::Error(format!(
                    "unknown arguments {extra:?} next to {:?}",
                    allowed[0]
                ))
            })
        };
        if args.contains_key("complete") || args.contains_key("cancel") {
            if let Some(error) = unknown(&["complete", "cancel", "reason"]) {
                return Ok(error);
            }
            let steps = |name: &str| match args.get(name) {
                Some(raw) => step_numbers(name, raw),
                None => Ok(Vec::new()),
            };
            let (complete, cancel) = match (steps("complete"), steps("cancel")) {
                (Ok(complete), Ok(cancel)) => (complete, cancel),
                (Err(message), _) | (_, Err(message)) => return Ok(TodoOutcome::Error(message)),
            };
            let reason = args
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if cancel.is_empty() && args.contains_key("reason") {
                return Ok(TodoOutcome::Error(
                    "`reason` goes with `cancel`".to_string(),
                ));
            }
            return Ok(outcome(
                store::close_steps(&self.store, thread_id, &complete, &cancel, reason).await,
            ));
        }
        let snap = match args.get("todos") {
            None if args.is_empty() => store::list(&self.store, thread_id).await,
            None => {
                return Ok(TodoOutcome::Error(format!(
                    "unknown arguments {:?}: pass `todos`, `complete`, `cancel`, or no arguments \
                     to read the list",
                    args.keys().collect::<Vec<_>>()
                )));
            }
            Some(Value::Null) if args.len() == 1 => store::list(&self.store, thread_id).await,
            Some(Value::Null) => {
                return Ok(TodoOutcome::Error(format!(
                    "unknown arguments {:?}: pass only `todos` to read the list",
                    args.keys().collect::<Vec<_>>()
                )));
            }
            Some(raw) => {
                let items = match parse_items(raw) {
                    Ok(items) => items,
                    Err(message) => return Ok(TodoOutcome::Error(message)),
                };
                store::replace(&self.store, thread_id, items).await
            }
        };
        Ok(outcome(snap))
    }
}

/// Splits the note arguments from the plan arguments.
fn split_notes(
    args: &Map<String, Value>,
) -> std::result::Result<(ScratchpadUpdate, Map<String, Value>), String> {
    let mut rest = args.clone();
    let mut update = ScratchpadUpdate::default();
    for name in NOTE_ARGS {
        let Some(value) = rest.remove(name) else {
            continue;
        };
        match name {
            "facts" => update.facts = note_list(name, value)?,
            "blockers" => update.blockers = note_list(name, value)?,
            "retract" => update.retract = note_list(name, value)?,
            _ => {
                update.approach = match value {
                    Value::Null => None,
                    Value::String(text) => Some(text),
                    other => {
                        return Err(format!("`approach` is a string, got {other}"));
                    }
                }
            }
        }
    }
    Ok((update, rest))
}

/// A list of notes; a lone string is one note.
fn note_list(name: &str, value: Value) -> std::result::Result<Vec<String>, String> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(text) => Ok(vec![text]),
        Value::Array(values) => values
            .into_iter()
            .map(|value| match value {
                Value::String(text) => Ok(text),
                other => Err(format!("`{name}` holds strings, got {other}")),
            })
            .collect(),
        other => Err(format!("`{name}` is a list of strings, got {other}")),
    }
}

/// The working notes as the `todo` result shows them.
fn notes_payload(pad: &Scratchpad) -> Value {
    let mut payload = json!({
        "facts": pad.facts,
        "blockers": pad.blockers,
    });
    if let Some(approach) = &pad.approach {
        payload["approach"] = json!(approach);
    }
    payload
}

/// A store outcome as the model reads it. A domain error (an order the plan
/// refuses) is surfaced to the model rather than failing the whole run.
fn outcome(snap: Result<TodosSnapshot>) -> TodoOutcome {
    match snap {
        Ok(snap) => {
            let mut payload = json!({
                "threadId": snap.thread_id,
                "todos": snap.items,
                "markdown": snap.markdown,
            });
            if !snap.notes.is_empty() {
                payload["notes"] = json!(snap.notes);
            }
            TodoOutcome::Ok(payload)
        }
        Err(TinyAgentsError::Validation(message)) => TodoOutcome::Error(message),
        Err(e) => TodoOutcome::Error(e.to_string()),
    }
}

/// One 1-based step number, or a list of them. A list or number sent as a
/// JSON string (`"[1, 2]"`, `"3"`) is read too: models write it that way often
/// enough that refusing it costs a call each time.
fn step_numbers(name: &str, raw: &Value) -> std::result::Result<Vec<usize>, String> {
    if let Value::String(text) = raw {
        return match serde_json::from_str::<Value>(text.trim()) {
            Ok(parsed @ (Value::Array(_) | Value::Number(_))) => step_numbers(name, &parsed),
            _ => Err(format!(
                "`{name}` is a step number or a list of them, got {raw}"
            )),
        };
    }
    match raw {
        Value::Array(values) if values.is_empty() => {
            Err(format!("`{name}` needs at least one step number"))
        }
        Value::Array(values) => values.iter().map(step_number).collect(),
        single => step_number(single).map(|step| vec![step]),
    }
}

/// The thread's closed steps, by content, before a write; empty when unreadable.
async fn closed_steps(store: &Arc<dyn Store>, thread_id: &str) -> Vec<String> {
    store::get(store, thread_id)
        .await
        .ok()
        .flatten()
        .map(|list| {
            list.items
                .into_iter()
                .filter(|item| item.status == TodoStatus::Completed)
                .map(|item| item.content)
                .collect()
        })
        .unwrap_or_default()
}

/// A call that completed several steps and recorded nothing gets a note
/// asking for what they established: a model that ticks late, in a batch,
/// otherwise keeps none of what those steps found.
fn note_unrecorded_closes(payload: &mut Value, before: &[String]) {
    let Some(todos) = payload.get("todos").and_then(Value::as_array) else {
        return;
    };
    let newly: Vec<String> = todos
        .iter()
        .enumerate()
        .filter(|(_, item)| item["status"] == "completed")
        .filter(|(_, item)| {
            item["content"]
                .as_str()
                .is_some_and(|content| !before.iter().any(|old| old == content))
        })
        .map(|(index, _)| (index + 1).to_string())
        .collect();
    if newly.len() < 2 {
        return;
    }
    let note = format!(
        "Steps {} were closed in one call with no notes. Record what they established (and \
         anything that blocked you) now: todo {{\"facts\": [...], \"blockers\": [...]}}.",
        newly.join(", ")
    );
    match payload.get_mut("notes").and_then(Value::as_array_mut) {
        Some(notes) => notes.push(json!(note)),
        None => payload["notes"] = json!([note]),
    }
}

/// A 1-based step number.
fn step_number(raw: &Value) -> std::result::Result<usize, String> {
    match raw.as_u64() {
        Some(step) if step >= 1 => Ok(step as usize),
        _ => Err(format!("a step number is a positive integer, got {raw}")),
    }
}

/// Internal dispatch outcome: a structured payload or a model-facing error.
enum TodoOutcome {
    Ok(Value),
    Error(String),
}

/// Decodes the model's `todos` array into items, rejecting blank content and
/// unknown statuses with a message the model can act on.
fn parse_items(raw: &Value) -> std::result::Result<Vec<TodoItem>, String> {
    let args: Vec<TodoArg> =
        serde_json::from_value(raw.clone()).map_err(|e| format!("invalid `todos`: {e}"))?;
    let mut items = Vec::with_capacity(args.len());
    for arg in args {
        let content = arg.text();
        if content.is_empty() {
            return Err("every todo needs non-empty `content`".to_string());
        }
        let status = match arg.status.as_deref() {
            None => TodoStatus::Pending,
            Some(raw) => parse_status(raw)?,
        };
        items.push(TodoItem::with_status(content, status));
    }
    Ok(items)
}

fn parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "todos": {
                "type": ["array", "null"],
                "description": "The full plan, in order. Pass null to read the current list.",
                "items": {
                    "type": "object",
                    // `content` is the advertised field. It is not
                    // `required`: the tool also reads the `title` / `task` /
                    // ... spellings models drift to (see `TodoArg`), and a
                    // schema rejection here would stop the call before the
                    // tool could. A blank item is still refused, by the tool.
                    "properties": {
                        "content": { "type": "string" },
                        "status": {
                            "type": "string",
                            "enum": [
                                "pending", "todo", "open", "not_started",
                                "in_progress", "in-progress", "inprogress", "started", "active",
                                "completed", "complete", "done", "finished",
                                "cancelled", "canceled", "skipped"
                            ]
                        }
                    }
                }
            },
            "complete": {
                "type": ["integer", "array", "string"],
                "minimum": 1,
                "items": { "type": "integer", "minimum": 1 },
                "description": "Complete step N (from 1), or several finished steps in order, and start the next one."
            },
            "cancel": {
                "type": ["integer", "array", "string"],
                "minimum": 1,
                "items": { "type": "integer", "minimum": 1 },
                "description": "Cancel step N (from 1) or several; needs `reason`. May go with `complete` in one call."
            },
            "reason": { "type": "string" },
            "facts": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Facts a tool result proved, one line each."
            },
            "blockers": {
                "type": "array",
                "items": { "type": "string" },
                "description": "What does not work or must be avoided here, and why (a failed approach, a limit, a constraint), one line each."
            },
            "approach": {
                "type": "string",
                "description": "What you are trying now; replaces the last one, \"\" clears it."
            },
            "retract": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Drop every note containing this text."
            }
        },
        "additionalProperties": false
    })
}

fn error_result(message: impl Into<String>) -> ToolResult {
    ToolResult::error(message)
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        TODO_TOOL_NAME
    }

    fn description(&self) -> &str {
        TODO_DESCRIPTION
    }

    fn parameters_schema(&self) -> Value {
        parameters_schema()
    }

    fn policy(&self) -> ToolPolicy {
        ToolPolicy {
            classified: true,
            side_effects: ToolSideEffects {
                read_only: false,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        Ok(error_result(
            "todo tool requires an active thread (no thread_id in tool context)",
        ))
    }

    async fn execute_with_context(
        &self,
        args: Value,
        _options: tinytools::ToolCallOptions,
        context: Option<&dyn ToolRunContext>,
    ) -> anyhow::Result<ToolResult> {
        let Some(thread_id) = context.and_then(ToolRunContext::thread_id) else {
            return Ok(error_result(
                "todo tool requires an active thread (no thread_id in tool context)",
            ));
        };
        match self.dispatch(thread_id, &args).await? {
            TodoOutcome::Ok(payload) => Ok(ToolResult::json(payload)),
            TodoOutcome::Error(message) => Ok(error_result(message)),
        }
    }
}

#[cfg(test)]
#[path = "tool_notes_tests.rs"]
mod notes_tests;
