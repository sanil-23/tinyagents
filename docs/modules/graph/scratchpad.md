# Per-thread scratchpad

`tinyagents_harness_tools::scratchpad` (feature `todos`) keeps what the model has learned in a thread, in its own
words, next to the [todo list](todos.md): the facts it verified, what
blocks it (what does not work or must be avoided, and why), and the approach it is on now. The run-state block
(`tinyagents_harness_tools::state_block`, feature `state-block`) shows them on every model call, so they do not scroll
away with the tool results they came from.

## Why

The block's own facts are mechanical: a failed command, an executable reported
missing, a file written. What a result *means* only the model can say ("the
table is `auth_users`", "adding a NOT NULL column with no default times out").
Without a place to put it, that knowledge lives in one old message and in the
model's reasoning, and a long turn loses it.

## Writing

Through optional arguments of the `todo` tool, so there is no new tool:

| Argument | Effect |
|---|---|
| `facts` | Add facts (a list of strings, or one string). |
| `blockers` | Add what does not work or must be avoided here, with why: a failed approach, a limit, a constraint. |
| `approach` | Replace the current approach; `""` clears it. |
| `retract` | Remove every note (facts, blockers, approach, last task) containing this text, case-insensitively. Runs before the additions. |

A `todo` call that closes several steps at once with no notes is answered with
a request for them, and the plan-order reminder and gate suggest
`{"complete": N, "facts": [...]}`: notes come with the ticks the model makes
anyway.

Each note is one line of at most 300 characters; a restated note moves to the
end instead of repeating. At most 12 facts and 8 blockers are kept, the oldest
dropped first.

## Lifetime

Notes belong to a piece of work. After a turn's final reply is committed the
host calls `scratchpad::store::close_finished_plan(store, thread)`:

- **The plan finished** (it has steps and none is open), and there are notes:
  - The facts and blockers move to `last_task`, which the block shows as
    "From your last finished task", so the thread's next turns see them at
    once.
  - The working notes and approach are cleared for the next task.
  - The call returns a `ClosedPlan { thread_id, items, notes }` for the
    host to keep past the thread. OpenHuman writes it to long-term memory.
  - A second call finds nothing to hand over.
- **The plan is open, or there is no plan:** nothing changes, and the notes
  carry into the next turn.

## Reading

`RunState::load(store, thread)` (in `state_block`) reads the goal, plan
and notes together; `RunState::render()` is the block a turn would start with.
The block middleware and a host's debug view use the same call. Each part is
read on its own, so an unreadable scratchpad leaves the goal and plan in place.

## Persistence

One serialized `Scratchpad` per thread in the `graph.scratchpad` namespace of a
`harness::store::Store`, keyed by `hex(thread_id)`, written
`load → merge → put` under a per-thread async mutex (the same single-process
caveat as the todo store).
