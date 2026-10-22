# graph::todos

A per-thread **todo list**: an ordered plan of steps per thread. The model
writes the whole list once, then ticks steps off one at a time, in order; each
item is a line of text in one of four states (plus a reason when cancelled),
and nothing else hangs off an item — no ids, no approvals, no assignment, no
run log.
Provider-neutral, offline-testable, with no app-specific coupling (no progress
events, RPC envelopes, or scratch fallback).

Distinct from [`graph::goals`](../goals) (a *single* durable objective per
thread): a goal is the completion contract the graph pursues; the list holds
the concrete steps.

## Data model (`types.rs`)

- `TodoStatus { Pending, InProgress, Completed, Cancelled }` (`as_str`,
  `is_open`, serde `snake_case`).
- `TodoItem { content, status, reason }` (serde `camelCase`; `status` defaults
  to `pending` when absent; `reason` only on a cancelled item).
- `TodoList { thread_id, items, updated_at }` — the stored value.
- `TodosSnapshot { thread_id, items, markdown, notes }` — every store op
  returns one; `notes` says what a write did beyond what was asked (the step
  it started, a second `in_progress` it set back to pending).
- `parse_status` (accepts aliases like `todo`→`Pending`, `done`→`Completed`,
  `skipped`→`Cancelled`), `render_markdown` (`[ ]`/`[~]`/`[x]`/`[-]` markers,
  one line per item),
  `normalise_list` (trimming, empty-content drop, `updated_at` stamp).

`updated_at` is unix-epoch millis as a string (dependency-free, no `chrono`).

## Persistence (`store.rs`)

One serialized `TodoList` per thread under the `graph.todos` namespace of a
`crate::harness::store::Store`, keyed by `hex(thread_id)`. Mutations run
`load → mutate → normalise → put` under a per-thread async mutex (a weak-value
`graph::thread_locks::ThreadLockMap`, so idle threads' mutexes are reclaimed
instead of leaking) — atomic within one process (same single-process caveat as
`graph::goals::store`). Ops: `replace` / `complete` / `cancel` / `clear` /
`list`, plus the raw `get` (absent versus present-empty, un-normalised) and
`delete` for hosts.

## Step order (`sequence.rs`)

Steps run one at a time, top to bottom. Every write keeps that order:

- A step may become `in_progress` only once every step above it is closed
  (`completed` or `cancelled`); it may become `completed` only if it was the
  step in progress or every step above it is closed. Otherwise the write is a
  `Validation` error naming every open step above it ("Finish or cancel step 1
  first.", or "Finish or cancel steps 1, 2 first, in order: ...") and the
  stored list is unchanged.
- With open steps and none in progress, the first open step starts. So
  writing a plan starts step 1, and `complete(N)` starts the next step.
- A whole-list write with several `in_progress` keeps the first and sets the
  rest to pending, and says so in `notes`.
- `cancel(N, reason)` may drop any open step (a later one too) and needs a
  reason.

## Tool (`tool.rs`)

`TodoTool` is a harness `Tool` named `todo`. `{"todos": [{"content",
"status"}]}` writes the whole list, `{"complete": N}` (1-based) completes step
N and starts the next (`{"complete": [N, M]}` closes several finished steps
top to bottom, all or none; `cancel` takes a list too, and both may go in one
call, cancels first; a list sent as a JSON string such as `"[1, 2]"` is read),
`{"cancel": N, "reason": "..."}` drops one, and no arguments reads the list. The
result carries the store's `notes`. Construct it directly with
`TodoTool::new(store)`. The target thread comes
from `ToolExecutionContext::thread_id` (never a tool argument); the bare
`Tool::call` entry point errors without a thread. Domain errors (blank
content, unknown status, an out-of-order step) are surfaced to the model as
tool errors rather than failing the run.

The same call, or one on its own, also takes the model's notes for the
thread's [scratchpad](../scratchpad/mod.rs): `facts`, `blockers`, `approach`
and `retract`. They are written first and taken out of the arguments, so each
plan mode keeps its own argument rules; notes alone read the list. A result
after a note write carries `scratchpad` (`facts`, `blockers`, `approach`); a
plan refusal still keeps the notes and says so. A call that closes two or
more steps (a `complete` list or a whole-list write) without `facts` or
`blockers` gets a note asking for what those steps established: a model that
ticks late, in a batch, otherwise keeps none of it.

## Middleware (`middleware.rs`)

`TodoSequenceMiddleware::new(store)` holds a run to the thread's plan. It is
inert while the thread has no plan with open steps.

- `before_tool`: with open steps and none in progress, every tool except
  `todo` is refused (`ToolFailed`) with the step to start.
- `after_model`: the first final answer while steps are open is held once
  (`continue_turn`) with an instruction to complete, cancel or explain them.
  `VerifyBeforeFinishMiddleware` skips a continued response, so register this
  one after it and the plan check comes first.
- `after_model` + `after_tool`: after `with_reminder_rounds(K)` tool rounds
  (default `DEFAULT_REMINDER_ROUNDS` = 8) with the same step in progress and
  no `todo` call, one note is appended to that round's first tool result.
  Once per step; a `todo` call restarts the count.
- `before_tool`: an ignored reminder becomes a gate. At twice the reminder
  rounds with no `todo` call, every other tool is refused ("Update your plan
  first: ...") until the model calls `todo`: `{"complete": N}` if the step is
  done, or a bare `{}` read to confirm it is still on it.

## Example

```rust,ignore
use std::sync::Arc;
use tinyagents_graph::{TodoItem, TodoStatus, TodoTool, todo_store};
use tinyagents_harness::store::{InMemoryStore, Store};

let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());

// Programmatic:
let snap = todo_store::replace(&store, "thread-1", vec![
    TodoItem::with_status("Write the RFC", TodoStatus::InProgress),
    TodoItem::new("Review it"),
]).await?;
println!("{}", snap.markdown); // - [~] Write the RFC\n- [ ] Review it

// Or register the `todo` tool for a model to drive:
let tool = TodoTool::new(store.clone());
```

## Files

| File | Role |
| --- | --- |
| `types.rs` | Item/list model, `parse_status`, `render_markdown`, `normalise_list`, `TodosSnapshot`. |
| `sequence.rs` | The step order: whole-list writes, `complete`, `cancel`. |
| `store.rs` | `Store`-backed `replace`/`complete`/`cancel`/`list`/`clear`, per-thread RMW lock. |
| `tool.rs` | The `todo` tool. |
| `middleware.rs` | `TodoSequenceMiddleware`: tool gate, finish hold, reminder. |
| `mod_tests.rs`, `sequence_tests.rs`, `middleware_tests.rs` | Unit tests. |
