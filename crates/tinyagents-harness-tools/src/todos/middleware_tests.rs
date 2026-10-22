use super::*;

use tinyagents_harness::context::RunConfig;
use tinyagents_harness::store::InMemoryStore;

const THREAD: &str = "t";

fn ctx() -> RunContext<()> {
    RunContext::new(
        RunConfig::new("todo-seq")
            .with_thread(THREAD)
            .with_max_model_calls(20),
        (),
    )
}

fn plan(specs: &[&str]) -> Vec<TodoItem> {
    specs
        .iter()
        .map(|content| TodoItem::new(*content))
        .collect()
}

fn round(name: &str) -> ModelResponse {
    let mut response = ModelResponse::assistant(String::new());
    response.message.content = Vec::new();
    response.message.tool_calls = vec![ToolCall::new("c", name, serde_json::json!({}))];
    response.finish_reason = Some("tool_calls".to_string());
    response
}

fn answer() -> ModelResponse {
    let mut response = ModelResponse::assistant("Done.".to_string());
    response.finish_reason = Some("stop".to_string());
    response
}

async fn before(mw: &TodoSequenceMiddleware, ctx: &mut RunContext<()>, name: &str) -> Result<()> {
    let mut call = ToolCall::new("c", name, serde_json::json!({}));
    Middleware::<(), ()>::before_tool(mw, ctx, &(), &mut call).await
}

async fn after(
    mw: &TodoSequenceMiddleware,
    ctx: &mut RunContext<()>,
    response: &mut ModelResponse,
) {
    Middleware::<(), ()>::after_model(mw, ctx, &(), response)
        .await
        .unwrap();
}

/// Runs a tool round of `name`, returning the note it left on the result.
async fn tool_round(mw: &TodoSequenceMiddleware, ctx: &mut RunContext<()>, name: &str) -> String {
    after(mw, ctx, &mut round(name)).await;
    let mut result = ToolResult::success("ok");
    let invocation = ToolInvocationIdentity::new("c", name);
    Middleware::<(), ()>::after_tool(mw, ctx, &(), &invocation, &mut result)
        .await
        .unwrap();
    result.output().trim_start_matches("ok").trim().to_string()
}

#[tokio::test]
async fn a_plan_less_run_is_untouched() {
    let mw =
        TodoSequenceMiddleware::new(Arc::new(InMemoryStore::default())).with_reminder_rounds(2);
    let mut ctx = ctx();
    before(&mw, &mut ctx, "shell").await.unwrap();
    for _ in 0..5 {
        assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    }
    let mut response = answer();
    after(&mw, &mut ctx, &mut response).await;
    assert!(response.continue_turn.is_none());
}

#[tokio::test]
async fn tools_are_refused_while_no_step_is_in_progress() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    // A list written before the order rules: step 1 done, nothing started.
    let list = serde_json::json!({
        "threadId": THREAD,
        "items": [
            { "content": "A", "status": "completed" },
            { "content": "B", "status": "pending" }
        ],
        "updatedAt": "0"
    });
    let key: String = THREAD.bytes().map(|b| format!("{b:02x}")).collect();
    store.put(store::TODOS_NAMESPACE, &key, list).await.unwrap();
    let mw = TodoSequenceMiddleware::new(store.clone());
    let mut ctx = ctx();

    match before(&mw, &mut ctx, "shell").await {
        Err(TinyAgentsError::ToolFailed(message)) => {
            assert!(
                message.starts_with("Start the next step of your plan first"),
                "{message}"
            );
            assert!(message.contains("step 2 \"B\""), "{message}");
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    before(&mw, &mut ctx, "todo")
        .await
        .expect("`todo` is always allowed");

    store::complete(&store, THREAD, 2).await.unwrap();
    before(&mw, &mut ctx, "shell")
        .await
        .expect("nothing left to start");
}

#[tokio::test]
async fn the_finish_gate_holds_once_while_steps_are_open() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    store::replace(&store, THREAD, plan(&["A", "B"]))
        .await
        .unwrap();
    let mw = TodoSequenceMiddleware::new(store.clone());
    let mut ctx = ctx();

    // A response another middleware already continued is left alone, and does
    // not use up the hold.
    let mut continued = answer();
    continued.continue_turn = Some("other".to_string());
    after(&mw, &mut ctx, &mut continued).await;
    assert_eq!(continued.continue_turn.as_deref(), Some("other"));

    let mut response = answer();
    after(&mw, &mut ctx, &mut response).await;
    let held = response.continue_turn.expect("open steps hold the answer");
    assert!(
        held.contains("step 1 \"A\" (in progress), step 2 \"B\""),
        "{held}"
    );
    assert!(held.contains("todo {\"complete\": N}"), "{held}");

    let mut again = answer();
    after(&mw, &mut ctx, &mut again).await;
    assert!(again.continue_turn.is_none(), "the hold fires once per run");
}

#[tokio::test]
async fn the_finish_gate_is_quiet_when_every_step_is_closed() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    store::replace(&store, THREAD, plan(&["A", "B"]))
        .await
        .unwrap();
    store::complete(&store, THREAD, 1).await.unwrap();
    store::cancel(&store, THREAD, 2, "not needed")
        .await
        .unwrap();
    let mw = TodoSequenceMiddleware::new(store);
    let mut ctx = ctx();
    let mut response = answer();
    after(&mw, &mut ctx, &mut response).await;
    assert!(response.continue_turn.is_none());
}

#[tokio::test]
async fn the_reminder_fires_once_per_step_and_a_todo_call_restarts_the_count() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    store::replace(&store, THREAD, plan(&["A", "B"]))
        .await
        .unwrap();
    let mw = TodoSequenceMiddleware::new(store.clone()).with_reminder_rounds(3);
    let mut ctx = ctx();

    assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    // A `todo` call restarts the count (and never carries the note).
    assert_eq!(tool_round(&mw, &mut ctx, "todo").await, "");
    assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    let note = tool_round(&mw, &mut ctx, "shell").await;
    assert!(
        note.contains("Step 1 \"A\" has been in progress for 3 rounds")
            && note.contains("todo {\"complete\": 1}"),
        "{note}"
    );
    for _ in 0..5 {
        assert_eq!(
            tool_round(&mw, &mut ctx, "shell").await,
            "",
            "once per step"
        );
    }

    // The next step gets its own reminder.
    store::complete(&store, THREAD, 1).await.unwrap();
    assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    assert_eq!(tool_round(&mw, &mut ctx, "shell").await, "");
    assert!(
        tool_round(&mw, &mut ctx, "shell")
            .await
            .contains("Step 2 \"B\"")
    );
}

#[tokio::test]
async fn an_ignored_reminder_gates_other_tools_until_a_todo_call() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    store::replace(&store, THREAD, plan(&["A", "B"]))
        .await
        .unwrap();
    let mw = TodoSequenceMiddleware::new(store.clone()).with_reminder_rounds(2);
    let mut ctx = ctx();

    // Reminder at 2 rounds, gate at 4.
    for _ in 0..3 {
        tool_round(&mw, &mut ctx, "shell").await;
        assert!(before(&mw, &mut ctx, "shell").await.is_ok());
    }
    tool_round(&mw, &mut ctx, "shell").await;
    let error = before(&mw, &mut ctx, "shell")
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Update your plan first: step 1 \"A\"") && error.contains("todo {}"),
        "{error}"
    );
    assert!(
        before(&mw, &mut ctx, "todo").await.is_ok(),
        "todo is never gated"
    );

    // A bare `todo` read ("still on it") lifts the gate and restarts the count.
    tool_round(&mw, &mut ctx, "todo").await;
    assert!(before(&mw, &mut ctx, "shell").await.is_ok());
}

#[tokio::test]
async fn a_run_without_a_thread_follows_the_plan_under_the_fallback_key() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    store::replace(&store, "standalone", plan(&["A", "B"]))
        .await
        .unwrap();
    let mut threadless = RunContext::new(RunConfig::new("no-thread").with_max_model_calls(20), ());

    // Without the key the rules cannot see the plan and stay out of the way.
    let blind = TodoSequenceMiddleware::new(store.clone());
    let mut response = answer();
    after(&blind, &mut threadless, &mut response).await;
    assert!(response.continue_turn.is_none());

    let mw = TodoSequenceMiddleware::new(store).with_fallback_thread("standalone");
    let mut response = answer();
    after(&mw, &mut threadless, &mut response).await;
    let held = response
        .continue_turn
        .expect("the fallback key finds the open steps");
    assert!(held.contains("step 1 \"A\" (in progress)"), "{held}");
}
