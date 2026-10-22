use super::*;

use tinyagents_harness::context::RunConfig;
use tinyagents_harness::store::InMemoryStore;
use tinyinference_llm::message::Message;

use crate::state_block::STATE_BLOCK_HEADER;

const THREAD: &str = "t";

fn ctx() -> RunContext<()> {
    RunContext::new(RunConfig::new("state-block").with_thread(THREAD), ())
}

fn request() -> ModelRequest {
    ModelRequest {
        messages: vec![Message::system("persona"), Message::user("do the task")],
        ..Default::default()
    }
}

/// The block on a fresh request, if any.
async fn block(mw: &StateBlockMiddleware, ctx: &mut RunContext<()>) -> Option<String> {
    let mut request = request();
    Middleware::<(), ()>::before_model_dispatch(mw, ctx, &(), &mut request)
        .await
        .unwrap();
    let base = self::request().messages.len();
    assert!(request.messages.len() <= base + 1, "at most one block");
    request.messages.get(base).map(Message::text)
}

/// One tool round: the model asks for `name` with `arguments`, and it returns
/// `output` (an error when `is_error`).
async fn round(
    mw: &StateBlockMiddleware,
    ctx: &mut RunContext<()>,
    name: &str,
    arguments: serde_json::Value,
    is_error: bool,
    output: &str,
) {
    let mut call = ToolCall::new("c", name, arguments);
    let mut response = ModelResponse::assistant(String::new());
    response.message.tool_calls = vec![call.clone()];
    Middleware::<(), ()>::after_model(mw, ctx, &(), &mut response)
        .await
        .unwrap();
    Middleware::<(), ()>::before_tool(mw, ctx, &(), &mut call)
        .await
        .unwrap();
    let mut result = if is_error {
        ToolResult::error(output)
    } else {
        ToolResult::success(output)
    };
    let invocation = ToolInvocationIdentity::new("c", name);
    Middleware::<(), ()>::after_tool(mw, ctx, &(), &invocation, &mut result)
        .await
        .unwrap();
    Middleware::<(), ()>::after_tool_output_screened(
        mw,
        ctx,
        &(),
        &invocation,
        &result,
        &result.output(),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_run_with_no_plan_and_no_facts_gets_no_block() {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let mut ctx = ctx();
    assert_eq!(block(&mw, &mut ctx).await, None);
    round(
        &mw,
        &mut ctx,
        "shell",
        serde_json::json!({"command": "ls"}),
        false,
        "a b",
    )
    .await;
    assert_eq!(block(&mw, &mut ctx).await, None);
}

#[tokio::test]
async fn the_block_follows_the_plan_and_the_tool_results() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    todo_store::replace(
        &store,
        THREAD,
        vec![
            TodoItem::new("Write the migration"),
            TodoItem::new("Run the tests"),
        ],
    )
    .await
    .unwrap();
    let mw = StateBlockMiddleware::new(store.clone());
    let mut ctx = ctx();

    let first = block(&mw, &mut ctx).await.expect("a plan shows");
    assert!(first.starts_with(STATE_BLOCK_HEADER), "{first}");
    assert!(!first.contains("Goal"), "no goal set yet: {first}");
    // A goal set mid-run shows on the next call.
    goal_store::set(&store, THREAD, "Ship the migration", None)
        .await
        .unwrap();
    let first = block(&mw, &mut ctx).await.unwrap();
    assert!(
        first.contains("\nGoal (active): Ship the migration\nPlan:"),
        "{first}"
    );
    assert!(
        first.contains("step 1 of 2 in progress (0 tool rounds on it)"),
        "{first}"
    );

    round(
        &mw,
        &mut ctx,
        "shell",
        serde_json::json!({"command": "python3 migrate.py"}),
        true,
        "Command failed (exit code 127)\n[stderr]\nbash: python3: command not found",
    )
    .await;
    let failed = block(&mw, &mut ctx).await.unwrap();
    assert!(failed.contains("(1 tool round on it)"), "{failed}");
    assert!(
        failed.contains("  - last round, shell `python3 migrate.py`: exit code 127; stderr: bash: python3: command not found"),
        "{failed}"
    );
    assert!(failed.contains("Reported not found: python3"), "{failed}");

    // `todo` calls are not ledger facts, but a tick moves the plan on.
    round(
        &mw,
        &mut ctx,
        "todo",
        serde_json::json!({"complete": 1}),
        false,
        "{}",
    )
    .await;
    todo_store::complete(&store, THREAD, 1).await.unwrap();
    // The loop renders before every model call; that is when the new step is seen.
    block(&mw, &mut ctx).await.unwrap();
    round(
        &mw,
        &mut ctx,
        "shell",
        serde_json::json!({"command": "python3 migrate.py"}),
        false,
        "migrated",
    )
    .await;
    let fixed = block(&mw, &mut ctx).await.unwrap();
    assert!(fixed.contains("[x] 1. Write the migration"), "{fixed}");
    assert!(
        fixed.contains("step 2 of 2 in progress (1 tool round on it)"),
        "{fixed}"
    );
    assert!(
        !fixed.contains("Recent failures"),
        "the success cleared it: {fixed}"
    );
    assert!(!fixed.contains("not found"), "python3 ran: {fixed}");
}

#[tokio::test]
async fn nested_tool_results_are_recorded_in_the_run_state() {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let ctx = ctx();
    let call = ToolCall::new(
        "parent/0",
        "shell",
        serde_json::json!({"command": "make test"}),
    );
    let result = ToolResult::error("exit code 2");
    Middleware::<(), ()>::observe_nested_result(&mw, &ctx, &(), &call, &result).await;

    let mut ctx = ctx;
    let rendered = block(&mw, &mut ctx)
        .await
        .expect("nested failure is visible");
    assert!(
        rendered.contains("shell `make test`: nested tool returned an error"),
        "{rendered}"
    );
}

#[tokio::test]
async fn only_security_screened_tool_output_enters_the_ledger() {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let mut ctx = ctx();
    let mut call = ToolCall::new("c", "shell", serde_json::json!({"command": "make"}));
    let mut response = ModelResponse::assistant(String::new());
    response.message.tool_calls = vec![call.clone()];
    Middleware::<(), ()>::after_model(&mw, &mut ctx, &(), &mut response)
        .await
        .unwrap();
    Middleware::<(), ()>::before_tool(&mw, &mut ctx, &(), &mut call)
        .await
        .unwrap();
    let invocation = ToolInvocationIdentity::new("c", "shell");
    let mut raw = ToolResult::error("secret-token-from-stderr");
    Middleware::<(), ()>::after_tool(&mw, &mut ctx, &(), &invocation, &mut raw)
        .await
        .unwrap();
    let mut screened = ToolResult::error("secret-token-in-canonical-content");
    screened.markdown_formatted = Some("safe markdown summary".to_string());
    let model_output = screened.output_for_llm(true);
    Middleware::<(), ()>::after_tool_output_screened(
        &mw,
        &mut ctx,
        &(),
        &invocation,
        &screened,
        &model_output,
    )
    .await
    .unwrap();

    let rendered = block(&mw, &mut ctx).await.unwrap();
    assert!(rendered.contains("safe markdown summary"), "{rendered}");
    assert!(!rendered.contains("secret-token-from-stderr"), "{rendered}");
    assert!(
        !rendered.contains("secret-token-in-canonical-content"),
        "{rendered}"
    );
}

#[tokio::test]
async fn truncated_state_block_is_removed_before_restoring_full_copy() {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let mut ctx = ctx();
    round(
        &mw,
        &mut ctx,
        "file_write",
        serde_json::json!({"path": "/a"}),
        false,
        "ok",
    )
    .await;
    let mut request = request();
    Middleware::<(), ()>::before_model_dispatch(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();
    let Message::System(system) = request.messages.last_mut().unwrap() else {
        panic!("state block is a system instruction in the default profile");
    };
    system.content[0] = ContentBlock::Text(
        "[run state] truncated old state that no longer contains its original tail".into(),
    );

    Middleware::<(), ()>::after_model_rewrite(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();

    let blocks = request
        .messages
        .iter()
        .filter(|message| message.text().contains(STATE_BLOCK_HEADER))
        .count();
    assert_eq!(blocks, 1);
    assert!(
        request
            .messages
            .last()
            .unwrap()
            .text()
            .contains("Files written this run: /a")
    );
    assert!(
        !request
            .messages
            .iter()
            .any(|message| message.text().contains("truncated old state"))
    );
}

#[tokio::test]
async fn a_rewritten_request_gets_the_block_back_once() {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let mut ctx = ctx();
    round(
        &mw,
        &mut ctx,
        "file_write",
        serde_json::json!({"path": "/a"}),
        false,
        "ok",
    )
    .await;
    let mut request = request();
    Middleware::<(), ()>::before_model_dispatch(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();
    let with_block = request.messages.len();

    // Already there: left alone.
    Middleware::<(), ()>::after_model_rewrite(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();
    assert_eq!(request.messages.len(), with_block);

    // A rewrite dropped it: added back.
    request.messages.pop();
    Middleware::<(), ()>::after_model_rewrite(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();
    assert_eq!(request.messages.len(), with_block);
    assert!(
        request
            .messages
            .last()
            .unwrap()
            .text()
            .contains("Files written this run: /a")
    );
}

#[tokio::test]
async fn the_ledger_ends_with_the_run() {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let mut ctx = ctx();
    round(
        &mw,
        &mut ctx,
        "shell",
        serde_json::json!({"command": "x"}),
        true,
        "exit code 1",
    )
    .await;
    assert!(block(&mw, &mut ctx).await.is_some());
    let mut run = AgentRun::default();
    Middleware::<(), ()>::after_agent(&mw, &mut ctx, &(), &mut run)
        .await
        .unwrap();
    assert_eq!(block(&mw, &mut ctx).await, None);
}

#[tokio::test]
async fn a_run_without_a_thread_reads_the_fallback_key() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    todo_store::replace(
        &store,
        "standalone",
        vec![TodoItem::new("Configure postfix")],
    )
    .await
    .unwrap();
    let mw = StateBlockMiddleware::new(store.clone());
    let mut threadless = RunContext::new(RunConfig::new("no-thread"), ());
    assert_eq!(block(&mw, &mut threadless).await, None, "no key, no plan");

    let mw = StateBlockMiddleware::new(store).with_fallback_thread("standalone");
    let shown = block(&mw, &mut threadless)
        .await
        .expect("the fallback key finds the plan");
    assert!(shown.contains("[>] 1. Configure postfix"), "{shown}");
}

/// A note another middleware pushes in `before_model`, after the block went in.
fn late_note(request: &mut ModelRequest) {
    request.messages.push(Message::user("web budget exhausted"));
}

async fn block_after_a_late_note(
    profile: Option<tinyinference_llm::model::ModelProfile>,
) -> ModelRequest {
    let mw = StateBlockMiddleware::new(Arc::new(InMemoryStore::default()));
    let mut ctx = ctx();
    ctx.model_profile = profile;
    round(
        &mw,
        &mut ctx,
        "shell",
        serde_json::json!({"command": "make"}),
        true,
        "exit code 2",
    )
    .await;
    let mut request = request();
    Middleware::<(), ()>::before_model_dispatch(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();
    late_note(&mut request);
    Middleware::<(), ()>::after_model_rewrite(&mw, &mut ctx, &(), &mut request)
        .await
        .unwrap();
    request
}

#[tokio::test]
async fn the_block_stays_last_when_a_later_note_is_pushed() {
    for profile in [
        None,
        Some(tinyinference_llm::model::ModelProfile {
            hoists_system_messages: true,
            mid_conversation_system_messages: true,
            ..Default::default()
        }),
    ] {
        let hoists = profile.is_some();
        let request = block_after_a_late_note(profile).await;
        let carrying = request
            .messages
            .iter()
            .filter(|message| message.text().contains(STATE_BLOCK_HEADER))
            .count();
        assert_eq!(carrying, 1, "exactly one block (hoists={hoists})");
        let last = request.messages.last().unwrap().text();
        assert!(
            last.contains(STATE_BLOCK_HEADER),
            "block is last (hoists={hoists}): {last}"
        );
        assert!(
            request
                .messages
                .iter()
                .any(|m| m.text().contains("web budget exhausted")),
            "the other note is kept (hoists={hoists})"
        );
        assert!(
            last.rfind(STATE_BLOCK_HEADER) > last.rfind("web budget exhausted"),
            "the block follows same-message guidance (hoists={hoists}): {last}"
        );
    }
}
