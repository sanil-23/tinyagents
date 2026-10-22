use super::*;

use crate::state_block::types::Failure;

fn item(content: &str, status: TodoStatus) -> TodoItem {
    TodoItem::with_status(content, status)
}

#[test]
fn nothing_to_show_renders_nothing() {
    assert_eq!(render(None, &[], &RunLedger::default()), None);
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
    let block = render(None, &items, &ledger).unwrap();
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
        render(None, &open, &ledger)
            .unwrap()
            .contains("Plan: no step in progress; step 2 of 2 is next.")
    );
    let closed = [item("A", TodoStatus::Completed)];
    assert!(
        render(None, &closed, &ledger)
            .unwrap()
            .contains("Plan: all 1 steps closed.")
    );
}

#[test]
fn a_long_plan_is_cut_with_a_count() {
    let items: Vec<TodoItem> = (0..25)
        .map(|n| item(&format!("step {n}"), TodoStatus::Pending))
        .collect();
    let block = render(None, &items, &RunLedger::default()).unwrap();
    assert!(block.contains("  [ ] 20. step 19\n  … 5 more"), "{block}");
    assert!(!block.contains("step 20"), "{block}");
}

#[test]
fn a_long_plan_keeps_the_active_step_visible() {
    let mut items: Vec<TodoItem> = (0..25)
        .map(|n| item(&format!("step {n}"), TodoStatus::Completed))
        .collect();
    items[23].status = TodoStatus::InProgress;
    let block = render(None, &items, &RunLedger::default()).unwrap();
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
    let block = render(None, &items, &RunLedger::default()).unwrap();
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
    let block = render(None, &[], &ledger).unwrap();
    assert!(!block.contains("Plan:"), "{block}");
    assert!(
        block.contains(
            "Recent failures (oldest first):\n  - last round, shell `python3 x.py`: exit code 127\n  - 3 rounds ago, shell `make`: exit code 2\nReported not found: python3\nFiles written this run: /app/x.py"
        ),
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
    let block = render(Some(&goal), &items, &RunLedger::default()).unwrap();
    assert!(
        block.contains(
            "\nGoal (active, ~600 tokens of budget left): Migrate the auth schema\nPlan: step 1 of 1"
        ),
        "{block}"
    );

    goal.status = ThreadGoalStatus::Complete;
    let alone = render(Some(&goal), &[], &RunLedger::default()).unwrap();
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
    let block = render(Some(&goal), &[], &RunLedger::default()).unwrap();
    assert!(
        block.ends_with(
            "\nGoal (paused): Migrate the auth schema\n  If the user's message continues it, call \
             goal_resume. If it starts a different task, call goal_set. Otherwise leave it paused."
        ),
        "{block}"
    );
}
