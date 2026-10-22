use super::*;

use tinyagents_harness::store::InMemoryStore;
use tinytools::ToolContent;

use crate::scratchpad::store as scratchpad_store;

struct ThreadContext;

impl ToolRunContext for ThreadContext {
    fn thread_id(&self) -> Option<&str> {
        Some("t")
    }
}

async fn run(tool: &TodoTool, args: Value) -> ToolResult {
    tool.execute_with_context(args, Default::default(), Some(&ThreadContext))
        .await
        .unwrap()
}

fn payload(result: &ToolResult) -> &Value {
    result
        .content
        .iter()
        .find_map(|block| match block {
            ToolContent::Json { data } => Some(data),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected a JSON payload: {}", result.output()))
}

fn tool() -> (TodoTool, Arc<dyn Store>) {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    (TodoTool::new(store.clone()), store)
}

#[tokio::test]
async fn notes_alone_are_saved_and_read_the_list() {
    let (tool, store) = tool();
    let result = run(
        &tool,
        json!({"facts": "the table is auth_users", "blockers": ["pip: no network"], "approach": "dump the schema"}),
    )
    .await;
    assert!(!result.is_error, "{}", result.output());
    let payload = payload(&result);
    assert_eq!(payload["todos"], json!([]), "a read of the (empty) list");
    assert_eq!(
        payload["scratchpad"],
        json!({"facts": ["the table is auth_users"], "blockers": ["pip: no network"], "approach": "dump the schema"})
    );
    let pad = scratchpad_store::get(&store, "t").await.unwrap().unwrap();
    assert_eq!(pad.facts, ["the table is auth_users"]);
}

#[tokio::test]
async fn notes_ride_along_with_every_plan_mode() {
    let (tool, store) = tool();
    let written = run(
        &tool,
        json!({"todos": [{"content": "a"}, {"content": "b"}], "approach": "a then b"}),
    )
    .await;
    assert!(!written.is_error, "{}", written.output());
    assert_eq!(payload(&written)["todos"].as_array().unwrap().len(), 2);

    let completed = run(&tool, json!({"complete": 1, "facts": ["a works"]})).await;
    assert!(!completed.is_error, "{}", completed.output());
    assert_eq!(
        payload(&completed)["scratchpad"]["facts"],
        json!(["a works"])
    );

    let cancelled = run(
        &tool,
        json!({"cancel": 2, "reason": "covered by a", "blockers": ["b is redundant"]}),
    )
    .await;
    assert!(!cancelled.is_error, "{}", cancelled.output());
    let pad = scratchpad_store::get(&store, "t").await.unwrap().unwrap();
    assert_eq!(pad.blockers, ["b is redundant"]);
    assert_eq!(pad.approach.as_deref(), Some("a then b"));
}

#[tokio::test]
async fn a_plain_plan_call_carries_no_scratchpad() {
    let (tool, _store) = tool();
    let result = run(&tool, json!({"todos": [{"content": "a"}]})).await;
    assert!(payload(&result).get("scratchpad").is_none());
}

#[tokio::test]
async fn a_refused_plan_call_still_saves_its_notes_and_says_so() {
    let (tool, store) = tool();
    let result = run(&tool, json!({"complete": 3, "facts": ["learned anyway"]})).await;
    assert!(result.is_error);
    assert!(
        result.output().contains("your notes were saved"),
        "{}",
        result.output()
    );
    let pad = scratchpad_store::get(&store, "t").await.unwrap().unwrap();
    assert_eq!(pad.facts, ["learned anyway"]);
}

#[tokio::test]
async fn malformed_notes_are_refused_before_anything_is_written() {
    let (tool, store) = tool();
    for args in [
        json!({"facts": 3}),
        json!({"blockers": [1, 2]}),
        json!({"approach": ["x"], "todos": [{"content": "a"}]}),
    ] {
        let result = run(&tool, args.clone()).await;
        assert!(result.is_error, "{args} should be refused");
    }
    assert_eq!(scratchpad_store::get(&store, "t").await.unwrap(), None);
    assert_eq!(
        crate::todos::store::get(&store, "t").await.unwrap(),
        None,
        "the plan was not written either"
    );
}

#[tokio::test]
async fn unknown_arguments_next_to_notes_are_still_refused() {
    let (tool, _store) = tool();
    let result = run(&tool, json!({"complete": 1, "facts": ["x"], "priority": 2})).await;
    assert!(result.is_error);
    assert!(result.output().contains("priority"), "{}", result.output());
}

#[test]
fn the_schema_and_description_advertise_the_notes() {
    let schema = parameters_schema();
    for name in NOTE_ARGS {
        assert!(
            schema["properties"].get(name).is_some(),
            "{name} in the schema"
        );
        assert!(
            TODO_DESCRIPTION.contains(&format!("`{name}`")),
            "{name} described"
        );
    }
}

async fn plan(tool: &TodoTool, steps: &[&str]) {
    let todos: Vec<Value> = steps.iter().map(|step| json!({"content": step})).collect();
    let result = run(tool, json!({ "todos": todos })).await;
    assert!(!result.is_error, "{}", result.output());
}

fn notes_of(result: &ToolResult) -> Vec<String> {
    payload(result)["notes"]
        .as_array()
        .map(|notes| {
            notes
                .iter()
                .map(|note| note.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn complete_takes_several_steps_in_order_all_or_none() {
    let (tool, store) = tool();
    plan(&tool, &["a", "b", "c", "d"]).await;

    let refused = run(&tool, json!({"complete": [1, 3]})).await;
    assert!(refused.is_error, "step 2 is still open");
    let list = crate::todos::store::get(&store, "t")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        list.items[0].status,
        TodoStatus::InProgress,
        "a refused batch changes nothing"
    );

    let done = run(&tool, json!({"complete": [2, 1]})).await;
    assert!(!done.is_error, "{}", done.output());
    let statuses: Vec<&str> = payload(&done)["todos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        ["completed", "completed", "in_progress", "pending"]
    );
    let notes = notes_of(&done);
    assert_eq!(
        notes
            .iter()
            .filter(|note| note.ends_with("is now in progress."))
            .count(),
        1,
        "only the step that is now current is announced: {notes:?}"
    );

    for bad in [
        json!({"complete": []}),
        json!({"complete": [0]}),
        json!({"complete": "two"}),
    ] {
        assert!(run(&tool, bad.clone()).await.is_error, "{bad} is refused");
    }
}

#[tokio::test]
async fn closing_several_steps_without_notes_asks_for_them() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c", "d"]).await;
    let one = run(&tool, json!({"complete": 1})).await;
    assert!(
        !notes_of(&one).iter().any(|note| note.contains("no notes")),
        "a single tick is not nagged"
    );
    let batch = run(&tool, json!({"complete": [2, 3]})).await;
    assert!(
        notes_of(&batch)
            .iter()
            .any(|note| note.contains("Steps 2, 3 were closed in one call with no notes")),
        "{:?}",
        notes_of(&batch)
    );
}

#[tokio::test]
async fn a_batch_with_facts_is_not_asked_again() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c"]).await;
    let batch = run(
        &tool,
        json!({"complete": [1, 2], "facts": ["a and b work"]}),
    )
    .await;
    assert!(
        !notes_of(&batch)
            .iter()
            .any(|note| note.contains("no notes"))
    );
}

#[tokio::test]
async fn a_rewrite_that_closes_several_steps_without_notes_asks_for_them() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c"]).await;
    let rewrite = run(
        &tool,
        json!({"todos": [
            {"content": "a", "status": "completed"},
            {"content": "b", "status": "completed"},
            {"content": "c", "status": "in_progress"}
        ]}),
    )
    .await;
    assert!(!rewrite.is_error, "{}", rewrite.output());
    assert!(
        notes_of(&rewrite)
            .iter()
            .any(|note| note.contains("Steps 1, 2 were closed in one call")),
        "{:?}",
        notes_of(&rewrite)
    );
}

fn statuses(result: &ToolResult) -> Vec<String> {
    payload(result)["todos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["status"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn complete_and_cancel_go_in_one_call() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c", "d"]).await;
    let result = run(
        &tool,
        json!({"complete": [1, 2], "cancel": 4, "reason": "not needed", "facts": ["a and b work"]}),
    )
    .await;
    assert!(!result.is_error, "{}", result.output());
    assert_eq!(
        statuses(&result),
        ["completed", "completed", "in_progress", "cancelled"]
    );
}

#[tokio::test]
async fn cancels_go_first_so_a_skipped_step_does_not_block_the_next() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c"]).await;
    // Step 1 is in progress: drop 2, finish 1 and 3.
    let result = run(
        &tool,
        json!({"complete": [1, 3], "cancel": 2, "reason": "covered by 1"}),
    )
    .await;
    assert!(!result.is_error, "{}", result.output());
    assert_eq!(statuses(&result), ["completed", "cancelled", "completed"]);
}

#[tokio::test]
async fn cancel_takes_a_list_with_one_reason() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c"]).await;
    let result = run(&tool, json!({"cancel": [2, 3], "reason": "out of scope"})).await;
    assert!(!result.is_error, "{}", result.output());
    assert_eq!(statuses(&result), ["in_progress", "cancelled", "cancelled"]);
}

#[tokio::test]
async fn a_list_or_number_sent_as_a_string_is_read() {
    let (tool, _store) = tool();
    plan(&tool, &["a", "b", "c"]).await;
    let result = run(&tool, json!({"complete": "[1, 2]"})).await;
    assert!(!result.is_error, "{}", result.output());
    assert_eq!(statuses(&result), ["completed", "completed", "in_progress"]);
    let result = run(&tool, json!({"complete": " 3 "})).await;
    assert!(!result.is_error, "{}", result.output());
    assert_eq!(statuses(&result), ["completed", "completed", "completed"]);
}

#[tokio::test]
async fn a_combined_call_is_all_or_nothing_and_checks_its_arguments() {
    let (tool, store) = tool();
    plan(&tool, &["a", "b", "c"]).await;
    for (args, message) in [
        (
            json!({"complete": 3, "cancel": 9, "reason": "x"}),
            "There is no step 9",
        ),
        (
            json!({"complete": 1, "cancel": 2}),
            "`cancel` needs a `reason`.",
        ),
        (
            json!({"complete": 1, "reason": "x"}),
            "`reason` goes with `cancel`",
        ),
        (
            json!({"complete": "two"}),
            "`complete` is a step number or a list of them",
        ),
        (
            json!({"cancel": [], "reason": "x"}),
            "`cancel` needs at least one step number",
        ),
        (json!({"complete": 1, "todos": []}), "unknown arguments"),
    ] {
        let result = run(&tool, args.clone()).await;
        assert!(
            result.is_error && result.output().starts_with(message),
            "{args}: {}",
            result.output()
        );
    }
    let list = crate::todos::store::get(&store, "t")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        list.items
            .iter()
            .map(|item| item.status)
            .collect::<Vec<_>>(),
        [
            TodoStatus::InProgress,
            TodoStatus::Pending,
            TodoStatus::Pending
        ],
        "a refused call writes nothing"
    );
}
