use super::*;

use crate::state_block::types::Failure;

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem::with_status(content, status)
}

#[test]
fn nothing_to_show_renders_nothing() {
    assert_eq!(render(None, None, &[], &RunLedger::default()), None);
}

#[test]
fn the_plan_marks_each_step_and_counts_rounds_on_the_active_one() {
    let mut cancelled = item("Benchmark", TodoStatus::Cancelled);
    cancelled.reason = Some("not asked for".to_string());
    let items = vec![
        item("Dump the schema", TodoStatus::Completed),
        item("Write the migration", TodoStatus::InProgress),
        item("Run the tests", TodoStatus::Pending),
        cancelled,
    ];
    let ledger = RunLedger {
        round: 7,
        step_since: 3,
        ..RunLedger::default()
    };
    let block = render(None, None, &items, &ledger).unwrap();
    assert!(block.starts_with(STATE_BLOCK_HEADER), "{block}");
    assert!(
        block.contains("Plan: step 2 of 4 in progress (4 tool rounds on it).\n  [x] 1. Dump the schema\n  [>] 2. Write the migration\n  [ ] 3. Run the tests\n  [-] 4. Benchmark (cancelled: not asked for)"),
        "{block}"
    );
}

#[test]
fn the_plan_header_covers_no_active_step_and_a_closed_plan() {
    let ledger = RunLedger::default();
    let open = [
        item("A", TodoStatus::Completed),
        item("B", TodoStatus::Pending),
    ];
    assert!(
        render(None, None, &open, &ledger)
            .unwrap()
            .contains("Plan: no step in progress; step 2 of 2 is next.")
    );
    let closed = [item("A", TodoStatus::Completed)];
    assert!(
        render(None, None, &closed, &ledger)
            .unwrap()
            .contains("Plan: all 1 steps closed.")
    );
}

#[test]
fn a_long_plan_is_cut_with_a_count() {
    let items: Vec<TodoItem> = (0..25)
        .map(|n| item(&format!("step {n}"), TodoStatus::Pending))
        .collect();
    let block = render(None, None, &items, &RunLedger::default()).unwrap();
    assert!(block.contains("  [ ] 20. step 19\n  … 5 more"), "{block}");
    assert!(!block.contains("step 20"), "{block}");
}

#[test]
fn a_long_plan_keeps_the_active_step_visible() {
    let mut items: Vec<TodoItem> = (0..25)
        .map(|n| item(&format!("step {n}"), TodoStatus::Completed))
        .collect();
    items[23].status = TodoStatus::InProgress;
    let block = render(None, None, &items, &RunLedger::default()).unwrap();
    assert!(block.contains("  [>] 24. step 23"), "{block}");
    assert!(block.contains("  … 5 more"), "{block}");
    assert!(!block.contains("  [x] 20."), "{block}");
}

#[test]
fn a_long_plan_keeps_the_next_pending_step_visible() {
    let mut items: Vec<TodoItem> = (0..25)
        .map(|n| item(&format!("step {n}"), TodoStatus::Completed))
        .collect();
    items[20].status = TodoStatus::Pending;
    let block = render(None, None, &items, &RunLedger::default()).unwrap();
    assert!(block.contains("  [ ] 21. step 20"), "{block}");
    assert!(!block.contains("  [x] 20."), "{block}");
}

#[test]
fn facts_render_without_a_plan() {
    let mut ledger = RunLedger {
        round: 4,
        ..RunLedger::default()
    };
    ledger.failures.push_back(Failure {
        round: 4,
        tool: "shell".to_string(),
        summary: "python3 x.py".to_string(),
        command: Some("python3 x.py".to_string()),
        error: "exit code 127".to_string(),
    });
    ledger.failures.push_back(Failure {
        round: 2,
        tool: "shell".to_string(),
        summary: "make".to_string(),
        command: Some("make".to_string()),
        error: "exit code 2".to_string(),
    });
    ledger.missing.push(("python3".to_string(), 4));
    ledger.files.push("/app/x.py".to_string());
    let block = render(None, None, &[], &ledger).unwrap();
    assert!(!block.contains("Plan:"), "{block}");
    assert!(
        block.contains(
            "Recent failures (oldest first):\n  - last round, shell `python3 x.py`: exit code 127\n  - 3 rounds ago, shell `make`: exit code 2\nCalls keep failing (2 recent failures)."
        ) && block.contains("\nReported not found: python3\nFiles written this run: /app/x.py"),
        "{block}"
    );
}

#[test]
fn the_goal_comes_first_with_its_status() {
    let store: std::sync::Arc<dyn tinyagents_harness::store::Store> =
        std::sync::Arc::new(tinyagents_harness::store::InMemoryStore::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut goal = runtime
        .block_on(crate::goals::store::set(
            &store,
            "t",
            "Migrate the auth schema",
            Some(1_000),
        ))
        .unwrap();
    goal.tokens_used = 400;
    let items = [item("Dump the schema", TodoStatus::InProgress)];
    let block = render(Some(&goal), None, &items, &RunLedger::default()).unwrap();
    assert!(
        block.contains(
            "\nGoal (active, ~600 tokens of budget left): Migrate the auth schema\nPlan: step 1 of 1"
        ),
        "{block}"
    );

    goal.status = ThreadGoalStatus::Complete;
    let alone = render(Some(&goal), None, &[], &RunLedger::default()).unwrap();
    assert!(
        alone.ends_with("\nGoal (complete): Migrate the auth schema"),
        "a goal alone is enough for a block: {alone}"
    );
}

#[test]
fn a_paused_goal_asks_the_model_to_decide() {
    let store: std::sync::Arc<dyn tinyagents_harness::store::Store> =
        std::sync::Arc::new(tinyagents_harness::store::InMemoryStore::default());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let mut goal = runtime
        .block_on(crate::goals::store::set(
            &store,
            "t",
            "Migrate the auth schema",
            None,
        ))
        .unwrap();
    goal.status = ThreadGoalStatus::Paused;
    let block = render(Some(&goal), None, &[], &RunLedger::default()).unwrap();
    assert!(
        block.ends_with(
            "\nGoal (paused): Migrate the auth schema\n  If the user's message continues it, call \
             goal_resume. If it starts a different task, call goal_set. Otherwise leave it paused."
        ),
        "{block}"
    );
}

#[test]
fn the_models_notes_show_after_the_plan_and_apart_from_the_harness_facts() {
    let pad = Scratchpad {
        facts: vec!["the table is auth_users".into()],
        blockers: vec!["ALTER without default timed out".into()],
        approach: Some("add the column nullable, backfill, then constrain".into()),
        last_task: Some(crate::scratchpad::TaskNotes {
            facts: vec!["staging has 2M rows".into()],
            blockers: vec!["pg_dump over ssh is too slow".into()],
        }),
        ..Scratchpad::default()
    };
    let ledger = RunLedger {
        files: vec!["/app/migrate.py".into()],
        ..RunLedger::default()
    };
    let items = vec![item("Write the migration", TodoStatus::InProgress)];
    let block = render(None, Some(&pad), &items, &ledger).unwrap();
    let at = |needle: &str| {
        block
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} missing:\n{block}"))
    };
    assert!(at("Plan:") < at("Current approach (your note): add the column nullable"));
    assert!(
        at("Current approach")
            < at("Verified facts (your notes, oldest first):\n  - the table is auth_users")
    );
    assert!(
        at("Verified facts")
            < at("Blockers (your notes, oldest first):\n  - ALTER without default")
    );
    assert!(
        at("Blockers")
            < at(
                "From your last finished task:\n  - fact: staging has 2M rows\n  - blocker: pg_dump"
            )
    );
    assert!(at("From your last finished task") < at("Files written this run"));
}

#[test]
fn notes_alone_make_a_block() {
    let pad = Scratchpad {
        facts: vec!["port is 8080".into()],
        ..Scratchpad::default()
    };
    let block = render(None, Some(&pad), &[], &RunLedger::default()).unwrap();
    assert!(block.contains("  - port is 8080"), "{block}");
    assert_eq!(
        render(
            None,
            Some(&Scratchpad::default()),
            &[],
            &RunLedger::default()
        ),
        None
    );
}

fn failed(round: usize) -> Failure {
    Failure {
        round,
        tool: "shell".into(),
        summary: "make".into(),
        command: Some("make".into()),
        error: "exit code 2".into(),
    }
}

fn failing_ledger(rounds: &[usize], step_since: usize, step: bool) -> RunLedger {
    RunLedger {
        round: 9,
        step_since,
        step: step.then(|| (1, "Build".to_string())),
        failures: rounds.iter().map(|round| failed(*round)).collect(),
        blocker_anchor: Some(None),
        ..RunLedger::default()
    }
}

#[test]
fn a_step_that_keeps_failing_asks_for_a_blocker() {
    let items = vec![item("Build", TodoStatus::InProgress)];
    let block = render(None, None, &items, &failing_ledger(&[7, 8], 6, true)).unwrap();
    assert!(
        block.contains("This step keeps failing (2 recent failures)")
            && block.contains("todo {\"blockers\": [\"what does not work and why\"]"),
        "{block}"
    );

    let one = render(None, None, &items, &failing_ledger(&[8], 6, true)).unwrap();
    assert!(
        !one.contains("keeps failing"),
        "one failure is not a pattern"
    );

    let before_step = render(None, None, &items, &failing_ledger(&[4, 5], 6, true)).unwrap();
    assert!(
        !before_step.contains("keeps failing"),
        "failures from an earlier step do not count"
    );
}

#[test]
fn a_blocker_recorded_since_the_step_began_ends_the_request() {
    let items = vec![item("Build", TodoStatus::InProgress)];
    let pad = Scratchpad {
        blockers: vec!["make with -j1 still fails: missing libpng".into()],
        ..Scratchpad::default()
    };
    let block = render(None, Some(&pad), &items, &failing_ledger(&[7, 8], 6, true)).unwrap();
    assert!(!block.contains("keeps failing"), "{block}");

    let mut stale = failing_ledger(&[7, 8], 6, true);
    stale.blocker_anchor = Some(Some("make with -j1 still fails: missing libpng".into()));
    let block = render(None, Some(&pad), &items, &stale).unwrap();
    assert!(
        block.contains("keeps failing"),
        "a blocker from before the step does not count: {block}"
    );
}

#[test]
fn without_a_plan_the_request_names_the_calls() {
    let block = render(None, None, &[], &failing_ledger(&[7, 8], 0, false)).unwrap();
    assert!(
        block.contains("Calls keep failing (2 recent failures)"),
        "{block}"
    );
}
