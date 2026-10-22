//! Facts the harness derives from tool calls without any model: what failed,
//! which executables are missing, which files were written.
//!
//! Every rule here reads only the call's arguments and its result, so a fact
//! is never an interpretation. Facts correct themselves: a later successful
//! call that runs a missing executable clears it, and a later success of the
//! same command clears its failure.

use serde_json::Value;
use tinyagents_harness::summarization::{DefaultFileOpExtractor, FileOpExtractor, FileOperations};
use tinyinference_llm::tool::ToolCall;

use super::types::{CallRecord, Failure, RunLedger};

/// Failures kept, newest last.
pub(super) const MAX_FAILURES: usize = 3;
/// A failure older than this many rounds is dropped.
pub(super) const FAILURE_HORIZON_ROUNDS: usize = 4;
/// Written paths kept, most recent last.
pub(super) const MAX_FILES: usize = 6;
/// Missing executable names shown in each request.
pub(super) const MAX_MISSING: usize = 6;
/// Longest argument summary, in characters.
const MAX_SUMMARY_CHARS: usize = 100;
/// Longest error line, in characters.
const MAX_ERROR_CHARS: usize = 160;

/// Argument keys that carry a shell command.
const COMMAND_KEYS: &[&str] = &["command", "cmd", "script"];
/// What the ledger keeps of `call` while it runs.
pub(super) fn call_record(call: &ToolCall) -> CallRecord {
    let command = string_arg(&call.arguments, COMMAND_KEYS);
    let mut file_operations = FileOperations::default();
    DefaultFileOpExtractor.extract(call, &mut file_operations);
    let mut written_paths: Vec<String> = file_operations
        .modified()
        .into_iter()
        .map(|path| one_line(path, MAX_SUMMARY_CHARS))
        .collect();
    if matches!(call.name.as_str(), "apply_patch" | "patch") {
        for path in patch_paths(&call.arguments) {
            if !written_paths.contains(&path) {
                written_paths.push(path);
            }
        }
    }
    let summary = match &command {
        Some(command) => one_line(command, MAX_SUMMARY_CHARS),
        None => one_line(&call.arguments.to_string(), MAX_SUMMARY_CHARS),
    };
    CallRecord {
        tool: call.name.clone(),
        summary,
        command,
        written_paths,
    }
}

/// Folds a finished call into the ledger.
pub(super) fn record_result(
    ledger: &mut RunLedger,
    call: CallRecord,
    is_error: bool,
    output: &str,
) {
    let round = ledger.round;
    if is_error && is_search_miss(&call, output) {
        return;
    }
    if is_error {
        for name in missing_executables(output) {
            if let Some((_, seen)) = ledger.missing.iter_mut().find(|(known, _)| known == &name) {
                *seen = round;
            } else {
                ledger.missing.push((name, round));
                while ledger.missing.len() > MAX_MISSING {
                    ledger.missing.remove(0);
                }
            }
        }
        ledger.failures.push_back(Failure {
            round,
            tool: call.tool,
            summary: call.summary,
            command: call.command,
            error: error_line(output),
        });
        while ledger.failures.len() > MAX_FAILURES {
            ledger.failures.pop_front();
        }
        return;
    }
    if let Some(command) = &call.command {
        ledger
            .missing
            .retain(|(name, _)| !runs_executable(command, name));
    }
    ledger.failures.retain(|failure| {
        failure.tool != call.tool
            || match (&failure.command, &call.command) {
                (Some(failed), Some(succeeded)) => failed != succeeded,
                (None, None) => failure.summary != call.summary,
                _ => true,
            }
    });
    for path in call.written_paths {
        ledger.files.retain(|known| known != &path);
        ledger.files.push(path);
        if ledger.files.len() > MAX_FILES {
            ledger.files.remove(0);
        }
    }
}

/// Commands whose exit code 1 or 2 usually means "nothing matched", not a
/// failure.
const SEARCH_COMMANDS: &[&str] = &[
    "grep", "egrep", "fgrep", "rg", "find", "test", "[", "diff", "cmp", "which",
];

/// Whether an error is an exploration miss rather than a failure: exit code 1,
/// 2 or 141 (a pipe closed early, as `grep … | head` does) with nothing on
/// stderr, from a search command or from a compound
/// command (`;`, `&&`, `|`, or several lines) whose last step is usually such a probe. Those are
/// not worth a line in the block; a failure that explains itself on stderr (a
/// traceback), a 127 or a non-search command still is.
fn is_search_miss(call: &CallRecord, output: &str) -> bool {
    let Some(command) = call.command.as_deref() else {
        return false;
    };
    if !matches!(exit_code(output), Some(1 | 2 | 141)) || stderr_line(output).is_some() {
        return false;
    }
    let compound = command.trim().contains(['\n', ';', '|']) || command.contains("&&");
    let first = command.split_whitespace().next().map(executable_name);
    compound || first.is_some_and(|word| SEARCH_COMMANDS.contains(&word.as_str()))
}

/// Drops failures older than the horizon.
pub(super) fn age_out(ledger: &mut RunLedger) {
    let round = ledger.round;
    ledger
        .failures
        .retain(|failure| round.saturating_sub(failure.round) <= FAILURE_HORIZON_ROUNDS);
    ledger
        .missing
        .retain(|(_, seen)| round.saturating_sub(*seen) <= FAILURE_HORIZON_ROUNDS);
}

/// Paths named in apply_patch envelopes or unified-diff headers.
fn patch_paths(arguments: &Value) -> Vec<String> {
    let Value::Object(map) = arguments else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for value in map.values().filter_map(Value::as_str) {
        for line in value.lines() {
            let path = [
                "*** Update File:",
                "*** Add File:",
                "*** Delete File:",
                "+++ ",
            ]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(|path| path.strip_prefix("b/").unwrap_or(path).trim())
            .filter(|path| !path.is_empty() && *path != "/dev/null");
            if let Some(path) = path {
                let path = one_line(path, MAX_SUMMARY_CHARS);
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }
    paths
}

/// The error, shortened to one line: the exit code when the output states
/// one, else its first line, plus the first line of a `[stderr]` section.
pub(super) fn error_line(output: &str) -> String {
    let head = exit_code(output).map_or_else(
        || {
            output
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("")
                .to_string()
        },
        |code| format!("exit code {code}"),
    );
    let stderr = stderr_line(output);
    let line = match stderr {
        Some(stderr) if !head.contains(stderr) => format!("{head}; stderr: {stderr}"),
        _ => head,
    };
    one_line(&line, MAX_ERROR_CHARS)
}

/// The first non-empty line of the output's `[stderr]` section.
fn stderr_line(output: &str) -> Option<&str> {
    output.split_once("[stderr]").and_then(|(_, rest)| {
        rest.lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with('['))
    })
}

/// The executables an error output reports missing, from the shells' own
/// wording: bash's `X: command not found`, dash's `sh: 1: X: not found` and
/// zsh's `command not found: X`. The zsh form must end the line and the dash
/// form needs a `prefix:` before the name, so prose such as "command not
/// found: a required executable…" or "Error: not found" is not read as a name.
pub(super) fn missing_executables(output: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in output.lines() {
        // zsh first: its `zsh: command not found: X` also contains bash's
        // `: command not found`.
        let candidate = if let Some((_, after)) = line.split_once("command not found: ") {
            let after = after.trim();
            (!after.contains(char::is_whitespace)).then_some(after)
        } else if let Some((before, _)) = line.split_once(": command not found") {
            last_token(before)
        } else if let Some((before, _)) = line.split_once(": not found") {
            before.contains(':').then(|| last_token(before)).flatten()
        } else {
            None
        };
        if let Some(name) = candidate
            .map(executable_name)
            .filter(|name| is_executable_name(name))
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names
}

/// Whether `command` invokes `name` in executable position or checks it with
/// `which`/`type`, rather than merely mentioning it as an argument.
fn runs_executable(command: &str, name: &str) -> bool {
    command.split(['\n', ';', '&', '|', '(', ')']).any(|part| {
        let words: Vec<_> = part.split_whitespace().collect();
        let Some(first) = words.first().map(|word| executable_name(word)) else {
            return false;
        };
        if first == name {
            return true;
        }
        if first == "env" {
            return words
                .iter()
                .skip(1)
                .find(|word| !word.starts_with('-') && !word.contains('='))
                .is_some_and(|word| executable_name(word) == name);
        }
        if first == "command" {
            return match words.get(1).copied() {
                Some("-v" | "-V") => words
                    .iter()
                    .skip(2)
                    .any(|word| executable_name(word) == name),
                Some(word) => executable_name(word) == name,
                None => false,
            };
        }
        matches!(first.as_str(), "which" | "type")
            && words
                .iter()
                .skip(1)
                .any(|word| executable_name(word) == name)
    })
}

fn exit_code(output: &str) -> Option<i64> {
    let (_, rest) = output.split_once("exit code ")?;
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    digits.parse().ok()
}

fn last_token(text: &str) -> Option<&str> {
    text.rsplit(|c: char| c.is_whitespace() || c == ':')
        .find(|token| !token.is_empty())
}

/// The bare executable name: no directory, no quotes.
fn executable_name(token: &str) -> String {
    let token = token.trim_matches(|c| matches!(c, '\'' | '"' | '`'));
    token.rsplit('/').next().unwrap_or(token).to_string()
}

fn is_executable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        && !name.chars().all(|c| c.is_ascii_digit())
}

fn string_arg(arguments: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `text` collapsed to one line and cut to `max` characters.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
