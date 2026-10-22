use super::*;

/// OpenHuman's shell tool, verbatim from a Terminal-Bench capture.
const SHELL_127: &str = "Command failed (exit code 127 \u{2014} command not found: a required \
executable or dependency is missing or not on PATH. Install/declare it, use an available \
alternative, or report the blocker \u{2014} do NOT re-run the same command)\n[stdout]\nexit=127\n\
0\n[stderr]\n/usr/bin/bash: line 2: exec: python3: not found\n";

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall::new("c1", name, arguments)
}

fn shell(command: &str) -> CallRecord {
    call_record(&call("shell", serde_json::json!({ "command": command })))
}

#[test]
fn a_record_keeps_the_command_and_only_a_write_tools_path() {
    let record = shell("python3   migrate.py\n--dry-run");
    assert_eq!(record.summary, "python3 migrate.py --dry-run");
    assert_eq!(
        record.command.as_deref(),
        Some("python3   migrate.py\n--dry-run")
    );
    assert!(record.written_paths.is_empty());

    let write = call_record(&call(
        "file_write",
        serde_json::json!({ "path": "/app/a.py", "content": "x" }),
    ));
    assert_eq!(write.written_paths, vec!["/app/a.py"]);
    assert_eq!(write.command, None);
    assert!(write.summary.contains("/app/a.py"), "{}", write.summary);

    let read = call_record(&call(
        "file_read",
        serde_json::json!({ "path": "/app/a.py" }),
    ));
    assert!(read.written_paths.is_empty(), "a read does not write");
}

#[test]
fn only_filesystem_tools_and_patch_envelopes_record_written_paths() {
    let issue = call_record(&call(
        "create_issue",
        serde_json::json!({ "path": "/issues/1", "title": "Bug" }),
    ));
    assert!(issue.written_paths.is_empty());

    let patch = call_record(&call(
        "apply_patch",
        serde_json::json!({
            "patch": "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-old\n+new\n*** Add File: src/new.rs\n+x\n*** End Patch"
        }),
    ));
    assert_eq!(patch.written_paths, vec!["src/main.rs", "src/new.rs"]);
}

#[test]
fn diff_text_from_non_patch_tools_is_not_treated_as_written_paths() {
    let shell = call_record(&call(
        "shell",
        serde_json::json!({"command": "cat <<'EOF'\n+++ b/src/main.rs\nEOF"}),
    ));
    let issue = call_record(&call(
        "create_issue",
        serde_json::json!({"body": "example diff:\n+++ b/src/main.rs"}),
    ));

    assert!(shell.written_paths.is_empty());
    assert!(issue.written_paths.is_empty());
}

#[test]
fn written_paths_are_bounded_before_they_enter_the_ledger() {
    let path = format!("/workspace/{}", "a".repeat(MAX_SUMMARY_CHARS * 2));
    let record = call_record(&call(
        "file_write",
        serde_json::json!({ "path": path, "content": "x" }),
    ));
    assert_eq!(record.written_paths.len(), 1);
    assert!(record.written_paths[0].chars().count() <= MAX_SUMMARY_CHARS);
}

#[test]
fn missing_executables_are_bounded_and_age_out() {
    let mut ledger = RunLedger::default();
    let output = (0..MAX_MISSING + 2)
        .map(|n| format!("bash: missing{n}: command not found"))
        .collect::<Vec<_>>()
        .join("\n");
    record_result(&mut ledger, shell("probe"), true, &output);
    assert_eq!(ledger.missing.len(), MAX_MISSING);
    assert_eq!(ledger.missing[0].0, "missing2");
    ledger.round = FAILURE_HORIZON_ROUNDS + 1;
    age_out(&mut ledger);
    assert!(ledger.missing.is_empty());
}

#[test]
fn repeated_missing_executables_refresh_their_age() {
    let mut ledger = RunLedger::default();
    record_result(
        &mut ledger,
        shell("python3 x.py"),
        true,
        "bash: python3: command not found",
    );
    ledger.round = FAILURE_HORIZON_ROUNDS;
    record_result(
        &mut ledger,
        shell("python3 x.py"),
        true,
        "bash: python3: command not found",
    );
    ledger.round += 1;
    age_out(&mut ledger);
    assert_eq!(
        ledger.missing,
        vec![("python3".to_string(), FAILURE_HORIZON_ROUNDS)]
    );
}

#[test]
fn unified_diff_headers_without_git_prefix_record_the_target_path() {
    let record = call_record(&call(
        "patch",
        serde_json::json!({ "diff": "--- /dev/null\n+++ src/main.rs\n@@ -0,0 +1 @@\n+fn main() {}" }),
    ));
    assert_eq!(record.written_paths, vec!["src/main.rs"]);
}

#[test]
fn the_error_line_is_the_exit_code_and_first_stderr_line() {
    assert_eq!(
        error_line(SHELL_127),
        "exit code 127; stderr: /usr/bin/bash: line 2: exec: python3: not found"
    );
    assert_eq!(
        error_line("\n  Tool `x` refused: path outside the workspace\nmore"),
        "Tool `x` refused: path outside the workspace"
    );
    assert!(error_line(&"e".repeat(500)).chars().count() <= 160);
}

#[test]
fn missing_executables_follow_the_shells_wording() {
    assert_eq!(missing_executables(SHELL_127), vec!["python3"]);
    assert_eq!(
        missing_executables("bash: jq: command not found\nzsh: command not found: rg"),
        vec!["jq", "rg"]
    );
    assert_eq!(
        missing_executables("sh: 1: /usr/bin/node: not found"),
        vec!["node"]
    );
    // Prose is not a name.
    assert!(missing_executables("Error: not found").is_empty());
    assert!(missing_executables("command not found: a required executable").is_empty());
    assert!(missing_executables("line 3: 404: not found").is_empty());
}

#[test]
fn a_success_of_the_same_command_clears_its_failure() {
    let mut ledger = RunLedger::default();
    record_result(&mut ledger, shell("make test"), true, "exit code 2");
    assert_eq!(ledger.failures.len(), 1);
    record_result(&mut ledger, shell("make"), false, "ok");
    assert_eq!(ledger.failures.len(), 1, "a different command leaves it");
    record_result(&mut ledger, shell("make test"), false, "ok");
    assert!(ledger.failures.is_empty());
}

#[test]
fn failure_correction_uses_the_full_command_not_its_short_display() {
    let prefix = format!("python3 -c '{}'", "x".repeat(120));
    let failed = format!("{prefix} fail");
    let succeeded = format!("{prefix} success");
    assert_eq!(shell(&failed).summary, shell(&succeeded).summary);
    let mut ledger = RunLedger::default();
    record_result(&mut ledger, shell(&failed), true, "exit code 1");
    record_result(&mut ledger, shell(&succeeded), false, "ok");
    assert_eq!(ledger.failures.len(), 1);
    record_result(&mut ledger, shell(&failed), false, "ok");
    assert!(ledger.failures.is_empty());
}

#[test]
fn a_missing_executable_clears_only_when_a_command_runs_it() {
    let mut ledger = RunLedger::default();
    record_result(&mut ledger, shell("python3 x.py"), true, SHELL_127);
    assert_eq!(ledger.missing, vec![("python3".to_string(), 0)]);
    record_result(&mut ledger, shell("echo python3"), false, "python3");
    assert_eq!(ledger.missing.len(), 1, "mentioning a name is not evidence");
    record_result(
        &mut ledger,
        shell("grep python3 requirements.txt"),
        false,
        "python3",
    );
    assert_eq!(
        ledger.missing.len(),
        1,
        "a search argument is not execution"
    );
    record_result(
        &mut ledger,
        shell("command -v python3"),
        false,
        "/usr/bin/python3",
    );
    assert!(ledger.missing.is_empty());
}

#[test]
fn failures_and_files_are_bounded_and_files_stay_distinct() {
    let mut ledger = RunLedger::default();
    for n in 0..5 {
        record_result(&mut ledger, shell(&format!("cmd{n}")), true, "exit code 1");
    }
    let kept: Vec<&str> = ledger.failures.iter().map(|f| f.summary.as_str()).collect();
    assert_eq!(kept, vec!["cmd2", "cmd3", "cmd4"]);

    for n in 0..8 {
        let write = call_record(&call(
            "edit_file",
            serde_json::json!({ "file_path": format!("/f{n}") }),
        ));
        record_result(&mut ledger, write, false, "ok");
    }
    let again = call_record(&call(
        "edit_file",
        serde_json::json!({ "file_path": "/f3" }),
    ));
    record_result(&mut ledger, again, false, "ok");
    assert_eq!(ledger.files, vec!["/f2", "/f4", "/f5", "/f6", "/f7", "/f3"]);
}

#[test]
fn old_failures_age_out() {
    let mut ledger = RunLedger::default();
    record_result(&mut ledger, shell("a"), true, "exit code 1");
    ledger.round = FAILURE_HORIZON_ROUNDS;
    age_out(&mut ledger);
    assert_eq!(ledger.failures.len(), 1);
    ledger.round += 1;
    age_out(&mut ledger);
    assert!(ledger.failures.is_empty());
}

#[test]
fn search_misses_are_not_failures() {
    let mut ledger = RunLedger::default();
    record_result(
        &mut ledger,
        shell("grep -n foo bar.txt"),
        true,
        "Command failed (exit code 1)",
    );
    record_result(
        &mut ledger,
        shell("echo a; postconf -n; grep x y"),
        true,
        "Command failed (exit code 2)",
    );
    record_result(
        &mut ledger,
        shell("ls /nope | head"),
        true,
        "Command failed (exit code 2)",
    );
    assert!(ledger.failures.is_empty(), "{:?}", ledger.failures);
    // A compound command that explains itself on stderr is a real failure.
    record_result(
        &mut ledger,
        shell("python3 -c 'import m'; echo done"),
        true,
        "Command failed (exit code 1)\n[stderr]\nTraceback (most recent call last):",
    );
    assert_eq!(ledger.failures.len(), 1);
    // A crash, a missing executable or a non-search failure still counts.
    record_result(
        &mut ledger,
        shell("make test"),
        true,
        "Command failed (exit code 2)",
    );
    record_result(&mut ledger, shell("python3 x.py"), true, SHELL_127);
    assert_eq!(ledger.failures.len(), 3);
}

#[test]
fn a_closed_pipe_and_a_multi_line_script_are_probes_too() {
    let mut ledger = RunLedger::default();
    record_result(
        &mut ledger,
        shell("grep -rn x /usr/lib | head -5"),
        true,
        "Command failed (exit code 141)",
    );
    record_result(
        &mut ledger,
        shell("mkdir -p /tmp/a\nls /tmp/a/missing"),
        true,
        "Command failed (exit code 2)",
    );
    assert!(ledger.failures.is_empty(), "{:?}", ledger.failures);
}
