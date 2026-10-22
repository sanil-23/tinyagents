# `state_block`: the run-state block

`StateBlockMiddleware` adds one short block to every model request: the
thread's goal, its plan (from the `todo` store), the model's own notes (from
the scratchpad) and facts the harness derived from the run's own tool calls. It never writes to the transcript and emits no
events, so a host UI sees none of it.

## Why

Between `todo` calls the model sees its plan only in an old tool result that
moves further back every round, and what failed calls taught it is spread over
raw output. On Terminal-Bench captures of DeepSeek V4 Flash (207 attempts), a
median of 14 model calls passed between plan updates, and half the attempts
that wrote a plan ended with steps still open. The block puts the current plan
and the hard facts at the tail of every request.

## What it shows

| Section | Source | Rule |
|---|---|---|
| Goal | `goals::store` for the run's thread | One line: status (active with budget left, paused, budget exhausted, complete) and the objective. |
| Plan | `todos::store` for the run's thread | Every step with its mark (`[x]` `[>]` `[ ]` `[-]`), the step in progress and the tool rounds it has run. At most 20 steps. |
| Current approach, Verified facts, Blockers | `scratchpad::store` for the run's thread | The model's notes, written through the `todo` tool's note arguments and labelled as its own. |
| From your last finished task | `Scratchpad::last_task` | The facts and blockers of the last plan that finished, moved there at the end of its turn. |
| Recent failures | `after_tool` results with `is_error` | The last 3, each with its age, tool, command (or argument summary) and the exit code plus first `[stderr]` line. Dropped after 4 rounds, or when the same command later succeeds. Exit codes 1, 2 or 141 with no `[stderr]` line from a search or compound command (including pipelines and multi-line scripts) are treated as exploration misses and omitted. |
| Blocker request | Recent failures since the current step began (or the run, without a plan) | With 2 or more and no blocker recorded since, one line asks the model to record what blocks the step with `blockers` and set a new `approach`. Gone once it does. |
| Reported not found | Shell wording in error output (`X: command not found`, `sh: 1: X: not found`, `command not found: X`) | Cleared when a later successful command runs `X`. |
| Files written this run | Paths extracted from filesystem tool calls, patch envelopes and unified-diff headers | The last 6, distinct. |

The harness sections are never a model's interpretation, so they cannot be
wrong in a way the tool results are not; the model's notes are labelled
"your notes" and the model can `retract` them. With no goal, plan, note or
fact, no block is added.

The saved part (goal, plan, notes) is read through `RunState::load`, the same
call a host uses to show the state outside a run; `RunState::render()` is the
block a turn starts with.

## Placement and caching

The block is added in `before_model_dispatch` through
`push_ephemeral_instruction`, and re-added in `after_model_rewrite` if a
request rewrite removed it or later guidance followed it. It therefore follows the model profile's
placement rules (a tail system message, or a `[harness note]` on the last tool
result or user turn for providers that hoist system messages), and is never persisted. It is included in the admission estimate when `BudgetMiddleware` runs after dispatch staging. `after_model_rewrite` also moves it back to the
end when a middleware pushed its own note after it in `before_model` (which runs after
`before_model_dispatch`), so the block is always the last thing the model reads: the next request is rebuilt from
the transcript, so the previous block is simply absent and the provider's
cached prefix is untouched.

## Wiring

```rust
use tinyagents_harness_tools::state_block::StateBlockMiddleware;

harness.push_middleware(Arc::new(StateBlockMiddleware::new(todo_store)));
```

Enable the crate's `state-block` feature to use this module.

For a run with no thread id, `with_fallback_thread(key)` names the scope the
host's `todo` tool falls back to (OpenHuman: the session id), so a headless run
still sees its plan. Pass the store the goal and `todo` tools write (in OpenHuman both live in the
session key-value store). Install it on the turns that have
the `todo` tool; it is an observer, so another middleware's control outcome
never hides a call from its ledger. Per-run state is keyed by the run
instance and dropped in `after_agent` / `on_error`.
