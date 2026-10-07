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
`Timeout`; only the run's own deadline is `Timeout`. `Halted` is reported when
the repeat-progress guard stops the run: the guard still pauses (so `run.paused`
is set and no `RunCompleted` is emitted) and marks the run, and the loop
reports `Halted` with the guard's summary instead of `Paused`. A
`LimitExceeded` failure carries the `LimitKind` of the cap that tripped last.
`AgentRun::terminal` is set before `after_agent` middleware runs. The runtime's
`DriverOutcome::outcome` lets a session report a capped or deferred run as such
rather than `Completed`.

The loop itself yields exactly one outcome per run; `merge` is for hosts that
race several signals (a cancel against a deadline, a guard against a limit).

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

- `TurnStarted { turn }` fires before each model call (1-based, counting model-call
  attempts, so a recovery retry opens a new turn). `TurnCompleted { turn, tool_result_count, tool_call_ids }`
  fires when its tool batch has been folded into the transcript, when the turn
  produced the final answer, or when the run ended mid-turn (every started turn
  is closed, including on failure).
- `MessageAppended { role, index, call_id, message }` announces each message
  appended to the working transcript, in order, at turn boundaries and run
  exit (the seed input is not announced). `message` is populated only under
  the payload-capture policy (`model_io`, or `tool_io` for tool messages).
- `MessageRetracted { index }` is emitted (highest index first) when an
  announced message is popped from the transcript, for example an unusable
  reply dropped before a retry or recovery nudge. `TranscriptRewritten { len,
  reason }` is emitted when the transcript is rewritten in place (a tool-set
  change folded into, or inserted before, the leading system message). Folding
  these with `MessageAppended` reproduces the transcript's role sequence.
  Transcript mutations that are not appends must call
  `RunContext::retract_transcript` / `rebase_transcript`.
- Host-budget compression rewrites only the outgoing request, never the
  transcript, so it emits neither.
- `QueuedMessageApplied` now also reports `first_index` and, under
  `model_io`, the applied `messages`.

The graph driver does not emit the turn/message events yet.

Not wired: the block codec in `stream/frame.rs` is still unused by the loop;
the loop streams `MessageDelta`, not `ModelStreamItem` blocks.
