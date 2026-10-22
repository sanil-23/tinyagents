use super::*;

use tinyagents_harness::store::InMemoryStore;

use crate::scratchpad::ScratchpadUpdate;
use crate::state_block::STATE_BLOCK_HEADER;
use crate::todos::TodoItem;

#[tokio::test]
async fn load_reads_goal_plan_and_notes_together() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    let empty = RunState::load(&store, "t").await;
    assert_eq!(empty.thread_id, "t");
    assert_eq!(empty.render(), None, "nothing saved, no block");

    goal_store::set(&store, "t", "Ship it", None).await.unwrap();
    todo_store::replace(&store, "t", vec![TodoItem::new("Build")])
        .await
        .unwrap();
    scratchpad_store::apply(
        &store,
        "t",
        &ScratchpadUpdate {
            facts: vec!["cargo is 1.90".into()],
            ..ScratchpadUpdate::default()
        },
    )
    .await
    .unwrap();

    let state = RunState::load(&store, "t").await;
    assert_eq!(state.goal.as_ref().unwrap().objective, "Ship it");
    assert_eq!(state.todos.len(), 1);
    assert_eq!(state.notes.as_ref().unwrap().facts, ["cargo is 1.90"]);
    let block = state.render().unwrap();
    assert!(block.starts_with(STATE_BLOCK_HEADER), "{block}");
    assert!(block.contains("Goal (active): Ship it"), "{block}");
    assert!(block.contains("[>] 1. Build"), "{block}");
    assert!(block.contains("  - cargo is 1.90"), "{block}");

    let json = serde_json::to_value(&state).unwrap();
    assert!(
        json.get("threadId").is_some() && json.get("todos").is_some(),
        "{json}"
    );
}

#[tokio::test]
async fn an_unreadable_scratchpad_leaves_the_goal_and_plan() {
    let store: Arc<dyn Store> = Arc::new(InMemoryStore::default());
    goal_store::set(&store, "t", "Ship it", None).await.unwrap();
    todo_store::replace(&store, "t", vec![TodoItem::new("Build")])
        .await
        .unwrap();
    // A record that no longer deserializes as a scratchpad ("t" hex-encoded).
    store
        .put(
            scratchpad_store::SCRATCHPAD_NAMESPACE,
            "74",
            serde_json::json!("not a scratchpad"),
        )
        .await
        .unwrap();
    assert!(scratchpad_store::get(&store, "t").await.is_err());

    let state = RunState::load(&store, "t").await;
    assert_eq!(state.goal.as_ref().unwrap().objective, "Ship it");
    assert_eq!(state.todos.len(), 1, "the plan still loads");
    assert!(state.notes.is_none(), "the unreadable notes are left out");
    let block = state.render().unwrap();
    assert!(block.contains("[>] 1. Build"), "{block}");
}
