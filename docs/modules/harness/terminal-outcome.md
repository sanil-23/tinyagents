# Terminal outcomes and lifecycle events

`TerminalOutcome` is the typed explanation for how an agent run ended. It is
attached to completed and failed run events and is available on `AgentRun`.
The `reason` identifies the specific cause, while `class` provides a stable
coarse-grained category for hosts that need only success, timeout,
cancellation, failure, or suspension.

Outcomes preserve the legacy human-readable message. Timeout outcomes also
record whether the deadline landed before a provider call, during one, or
after a call. Provider-start state is tracked separately so a host can tell a
first-call failure from a preflight failure.

The direct and graph loop drivers publish the outcome on `AgentRun` before
running `after_agent` middleware. This lets middleware observe the same
classification as the terminal event. Session hooks receive the typed outcome
before the legacy terminal notification.

Turn lifecycle events announce appended messages and retract only messages
that were previously announced. Initial input messages are treated as a seed
prefix and are not re-announced on the first turn.
