# Harness goals

A `ThreadGoal` is one durable objective per thread. It tracks status, optional
budget, token usage, and elapsed time. `GoalStore` owns persistence, lifecycle
observation, atomic turn accounting, and meter-update throttling.

## Host lifecycle

Construct `GoalStore` with a harness `Store`, optionally with a `GoalObserver`.
Its `set`, `pause`, `resume`, `complete`, `clear`, and `account_turn` methods
persist changes and notify the observer. Accounting takes `Duration`, preserving
subsecond turn time, and applies usage/status changes atomically within one
process. Budgeted goal updates publish once cumulative usage since the last
published update reaches five percent of the budget (at least one token).

Before a turn, call `prepare_turn(thread_id)`. It reloads the goal without
changing its status and returns `GoalTurnPreparation` containing the goal, an
optional context block, and an optional `GoalBudgetGuard`. Active and
budget-limited goals receive their usual context. A paused goal remains paused,
but its context tells the model to inspect the new message and call
`goal_resume` if the user continues that objective, use `goal_set` for a
different task, or leave the goal paused otherwise. A host that uses
`tinyagents-hooks` constructs `GoalBudgetStopHook::for_goal(store, goal)` from
the returned goal and installs it for that turn; the guard must remain attached
for the duration of that turn. Keeping hook construction in the host avoids a
crate cycle.

## Model tool

`GoalTool` is constructed directly with a `GoalToolKind` and shared `GoalStore`.
The model-facing kinds are `Get`, `Set`, `Complete`, and `Resume`; pause and
clear remain host controls. The model can resume a paused goal after it reads
the incoming message. The target thread comes from
`ToolExecutionContext::thread_id`, never a model argument.

## Persistence

The store keeps one serialized goal under the `graph.goals` namespace, keyed by
hex-encoded thread id. Mutations use a per-thread async mutex, so accounting is
atomic within one process. A shared store across multiple processes needs a
single writer until the `Store` contract offers compare-and-swap.
