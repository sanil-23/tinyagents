# Terminal outcome and turn/message lifecycle

## Terminal outcome

Every run ends with a structured `TerminalOutcome` (`tinyagents_harness::terminal`):

| Field | Meaning |
| --- | --- |
| `reason` | `Completed`, `LimitReached(Option<LimitKind>)`, `Timeout`, `Cancelled`, `Paused`, `Deferred`, `Halted`, `ProviderFailed(Option<FailoverReason>)`, `ToolFailed`, `Internal` |
| `class` | `Success`, `Timeout`, `Cancellation`, `Failure`, `Suspended` (always derived from `reason`) |
| `timeout_phase` | `BeforeProvider`, `Provider` or `AfterTurn` for timeouts |
| `provider_started` | whether any provider call had started |
| `message` | the legacy human-readable string (mirrors `RunFailed::error`) |

It is delivered on `AgentEvent::RunCompleted { outcome }`,
`AgentEvent::RunFailed { outcome }`, `AgentRun::terminal` (also on the partial
run from `invoke_collecting_partial`), and to session hooks through
`SessionHooks::on_terminal_outcome` (`tinyagents-runtime`). The string fields
are unchanged, so existing hosts keep working. A paused or deferred run emits
no `RunCompleted`; its outcome is on `AgentRun::terminal` (`Suspended`).

`CallTimeout` (one wedged call) is `ProviderFailed(Some(Timeout))` with class
`Timeout`; only the run's own deadline is `Timeout`. `Halted` is built by hosts
with `TerminalOutcome::halted(summary)`: the repeat-progress guard pauses the
run, so the loop cannot tell it apart from a steering pause.

### Merge precedence

`TerminalOutcome::merge` keeps the more authoritative of two competing
outcomes (earlier wins ties; `provider_started` is OR-ed):

1. external cancel
2. run deadline
3. idle / provider-call timeout
4. limits
5. no-progress / repeat guard
6. other failures (provider, tool, internal)
7. suspension (paused, deferred)
8. success

## Turn and message lifecycle

- `TurnStarted { turn }` fires before each model call (1-based, matches the
  `-model-N` call id). `TurnCompleted { turn, tool_result_count, tool_call_ids }`
  fires when its tool batch has been folded into the transcript, when the turn
  produced the final answer, or when the run ended mid-turn (every started turn
  is closed, including on failure).
- `MessageAppended { role, index, call_id, message }` announces each message
  appended to the working transcript, in order, at turn boundaries and run
  exit (the seed input is not announced). `message` is populated only under
  the payload-capture policy (`model_io`, or `tool_io` for tool messages).
- `QueuedMessageApplied` now also reports `first_index` and, under
  `model_io`, the applied `messages`.

Not wired: the block codec in `stream/frame.rs` is still unused by the loop;
the loop streams `MessageDelta`, not `ModelStreamItem` blocks.
