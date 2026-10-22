# Goals moved to harness-tools

Per-thread goals are generic harness functionality, not graph executor policy.
The `GoalStore`, observer, atomic accounting, budget guard, and host-facing turn
preparation API live in `crates/tinyagents-harness-tools/src/goals/`. See
[`Goals-and-Todos`](../../../wiki/Goals-and-Todos.md).
