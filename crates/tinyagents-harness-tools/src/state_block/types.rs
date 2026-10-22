//! Domain types for the run-state block.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tinyagents_harness::store::Store;

/// Adds a freshly rendered run-state block to every model request.
///
/// See the [module docs](super) for what the block holds and why it is
/// request-only.
pub struct StateBlockMiddleware {
    /// The store the `todo` tool writes, read for the thread's plan.
    pub(super) store: Arc<dyn Store>,
    /// The plan and goal key for a run with no thread id: the scope the host's
    /// `todo` tool falls back to (for example a session id).
    pub(super) fallback_thread: Option<String>,
    /// Per-run ledgers, keyed by [`RunContext::instance_id`](tinyagents_harness::context::RunContext::instance_id).
    pub(super) runs: Mutex<HashMap<u64, RunLedger>>,
}

/// What one run has learned from its own tool calls, plus the block staged
/// for the request in flight.
#[derive(Debug, Default)]
pub(super) struct RunLedger {
    /// Tool rounds so far: model responses that asked for at least one tool.
    pub round: usize,
    /// The plan step last seen in progress, as (number, text), and the round
    /// it was first seen at.
    pub step: Option<(usize, String)>,
    pub step_since: usize,
    /// Calls between `before_tool` and `after_tool`, by call id.
    pub calls: HashMap<String, CallRecord>,
    /// The newest failures, oldest first.
    pub failures: VecDeque<Failure>,
    /// Executables a tool reported missing, with the round they were seen.
    pub missing: Vec<(String, usize)>,
    /// Paths written by write/edit tools, most recent last, distinct.
    pub files: Vec<String>,
    /// The block rendered for the request in flight, for re-application
    /// after request-rewriting middleware.
    pub staged: Option<String>,
}

/// What the ledger keeps of one tool call while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CallRecord {
    /// The tool's name.
    pub tool: String,
    /// A one-line summary of the arguments: the command when there is one.
    pub summary: String,
    /// The shell command, when the arguments carry one.
    pub command: Option<String>,
    /// Paths a filesystem tool targets.
    pub written_paths: Vec<String>,
}

/// One failed tool call, as the block reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Failure {
    /// The round it happened in.
    pub round: usize,
    /// The tool's name.
    pub tool: String,
    /// The call's argument summary.
    pub summary: String,
    /// Full shell command, when the failed call carried one.
    pub command: Option<String>,
    /// The error, shortened to one line.
    pub error: String,
}
