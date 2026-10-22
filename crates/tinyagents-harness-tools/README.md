# `tinyagents-harness-tools`

This crate provides reusable tool implementations and stateful support for
`tinyagents-harness`. It keeps product policy and presentation outside the
agent loop while making common harness capabilities available to Rust hosts.

## Features

The default feature set enables `todos`, `goals`, and `skills`. The `subagents`
feature enables the generic job and message tools and brings in
`tinyagents-orchestration` and its task registry dependencies. Time,
clarification, plan-exit, per-call binding, and thread-lock support are
available without optional features. The `state-block` feature enables the
run-state middleware and its required `goals` and `todos` modules.

## Public modules and tools

- `goals` provides `GoalStore`, `GoalTool`, observer notifications, atomic
  turn accounting, budget guards, and goal context. A host calls
  `GoalStore::prepare_turn` and composes a budget stop hook for the turn.
- `todos` provides `TodoTool`, the persistent sequence model, and
  `TodoSequenceMiddleware`.
- `state_block` provides `StateBlockMiddleware`, which rebuilds the current
  goal, plan, and tool-call facts in each model request.
- `skills` provides `UseSkillTool`, `PackCatalog`, `PackRegistryHandle`, and
  `UseSkillDispatch` for binding a selected tool from a named pack.
- `subagents` provides `DriverSubAgentTool`, `SubAgentJobsTool`, and
  `SubAgentMessageTool` over the orchestration crate's shared
  `SubAgentJobRegistry`.
- `thread_locks` provides the public `ThreadLockMap` concurrency primitive.
- The crate root exports `CurrentTimeTool`, `ResolveTimeTool`,
  `AskClarificationTool`, `PlanExitTool`, and the `per_call` binding adapter.

Subagent registry policy stays in `tinyagents-orchestration`; this crate owns
the reusable tool declarations. The harness owns tool registration,
admission, schema preparation, discovery, and dispatch infrastructure.
