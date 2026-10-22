use std::sync::Arc;

use super::store::{self, MAX_BLOCKERS, MAX_FACTS, MAX_NOTE_CHARS, merge};
use super::*;
use crate::todos::{TodoItem, store as todo_store};
use tinyagents_harness::store::{InMemoryStore, Store};

fn store() -> Arc<dyn Store> {
    Arc::new(InMemoryStore::default())
}

fn notes(facts: &[&str], blockers: &[&str]) -> ScratchpadUpdate {
    ScratchpadUpdate {
        facts: facts.iter().map(|note| note.to_string()).collect(),
        blockers: blockers.iter().map(|note| note.to_string()).collect(),
        ..ScratchpadUpdate::default()
    }
}

#[test]
fn notes_are_one_line_deduplicated_and_capped_oldest_first() {
    let mut pad = Scratchpad::default();
    merge(
        &mut pad,
        &notes(
            &["the table is\n  auth_users", "THE TABLE IS AUTH_USERS", ""],
            &[],
        ),
    );
    assert_eq!(
        pad.facts,
        ["THE TABLE IS AUTH_USERS"],
        "the restated note wins once"
    );

    let many: Vec<String> = (0..MAX_FACTS + 3).map(|n| format!("fact {n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    merge(&mut pad, &notes(&many, &[]));
    assert_eq!(pad.facts.len(), MAX_FACTS);
    assert_eq!(pad.facts[0], "fact 3", "the oldest go first");

    let long = "x".repeat(MAX_NOTE_CHARS + 50);
    merge(&mut pad, &notes(&[], &[&long]));
    assert_eq!(pad.blockers[0].chars().count(), MAX_NOTE_CHARS);
    assert!(pad.blockers[0].ends_with('…'));
}

#[test]
fn blockers_are_capped_too() {
    let mut pad = Scratchpad::default();
    let many: Vec<String> = (0..MAX_BLOCKERS + 2)
        .map(|n| format!("tried {n}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    merge(&mut pad, &notes(&[], &many));
    assert_eq!(pad.blockers.len(), MAX_BLOCKERS);
}

#[test]
fn retract_removes_matching_notes_before_new_ones_land() {
    let mut pad = Scratchpad::default();
    merge(
        &mut pad,
        &ScratchpadUpdate {
            facts: vec!["port is 8080".into(), "python3 is installed".into()],
            blockers: vec!["pip install failed: no network".into()],
            approach: Some("serve on port 8080".into()),
            ..ScratchpadUpdate::default()
        },
    );
    merge(
        &mut pad,
        &ScratchpadUpdate {
            facts: vec!["port is 9090".into()],
            retract: vec!["PORT ".into(), "  ".into()],
            ..ScratchpadUpdate::default()
        },
    );
    assert_eq!(pad.facts, ["python3 is installed", "port is 9090"]);
    assert_eq!(pad.blockers, ["pip install failed: no network"]);
    assert_eq!(pad.approach, None, "the approach matched too");
}

#[test]
fn a_new_approach_replaces_the_old_and_an_empty_one_clears_it() {
    let mut pad = Scratchpad::default();
    let approach = |text: &str| ScratchpadUpdate {
        approach: Some(text.to_string()),
        ..ScratchpadUpdate::default()
    };
    merge(&mut pad, &approach("bisect the commits"));
    merge(&mut pad, &approach("read the changelog"));
    assert_eq!(pad.approach.as_deref(), Some("read the changelog"));
    merge(&mut pad, &approach("  "));
    assert_eq!(pad.approach, None);
    assert!(pad.is_empty());
}

#[tokio::test]
async fn apply_persists_per_thread() {
    let store = store();
    assert_eq!(store::get(&store, "a").await.unwrap(), None);
    let pad = store::apply(&store, "a", &notes(&["one"], &[]))
        .await
        .unwrap();
    assert_eq!(pad.thread_id, "a");
    assert!(!pad.updated_at.is_empty());
    assert_eq!(store::get(&store, "a").await.unwrap(), Some(pad));
    assert_eq!(
        store::get(&store, "b").await.unwrap(),
        None,
        "threads are apart"
    );
    assert!(
        store::apply(&store, "  ", &notes(&["x"], &[]))
            .await
            .is_err()
    );
    assert!(store::clear(&store, "a").await.unwrap());
    assert!(!store::clear(&store, "a").await.unwrap());
}

#[tokio::test]
async fn an_open_plan_keeps_its_notes_at_the_end_of_the_turn() {
    let store = store();
    todo_store::replace(&store, "t", vec![TodoItem::new("a"), TodoItem::new("b")])
        .await
        .unwrap();
    store::apply(&store, "t", &notes(&["one"], &[]))
        .await
        .unwrap();
    assert_eq!(store::close_finished_plan(&store, "t").await.unwrap(), None);
    todo_store::complete(&store, "t", 1).await.unwrap();
    assert_eq!(store::close_finished_plan(&store, "t").await.unwrap(), None);
    assert_eq!(
        store::get(&store, "t").await.unwrap().unwrap().facts,
        ["one"]
    );
}

#[tokio::test]
async fn a_finished_plan_hands_its_notes_over_once_and_keeps_them_as_the_last_task() {
    let store = store();
    todo_store::replace(&store, "t", vec![TodoItem::new("a"), TodoItem::new("b")])
        .await
        .unwrap();
    store::apply(
        &store,
        "t",
        &ScratchpadUpdate {
            approach: Some("one step at a time".into()),
            ..notes(
                &["the table is auth_users"],
                &["ALTER without default timed out"],
            )
        },
    )
    .await
    .unwrap();
    todo_store::complete(&store, "t", 1).await.unwrap();
    todo_store::cancel(&store, "t", 2, "not needed")
        .await
        .unwrap();

    let closed = store::close_finished_plan(&store, "t")
        .await
        .unwrap()
        .expect("the plan finished with notes");
    assert_eq!(closed.thread_id, "t");
    assert_eq!(closed.items.len(), 2);
    assert_eq!(closed.notes.facts, ["the table is auth_users"]);
    assert_eq!(closed.notes.blockers, ["ALTER without default timed out"]);

    let pad = store::get(&store, "t").await.unwrap().unwrap();
    assert!(pad.facts.is_empty() && pad.blockers.is_empty());
    assert_eq!(pad.approach, None, "the next task starts clean");
    assert_eq!(pad.last_task, Some(closed.notes));
    assert_eq!(
        store::close_finished_plan(&store, "t").await.unwrap(),
        None,
        "a second call hands nothing over"
    );
}

#[tokio::test]
async fn a_finished_plan_without_notes_or_no_plan_hands_nothing_over() {
    let store = store();
    store::apply(&store, "t", &notes(&["one"], &[]))
        .await
        .unwrap();
    assert_eq!(
        store::close_finished_plan(&store, "t").await.unwrap(),
        None,
        "no plan"
    );
    todo_store::replace(&store, "u", vec![TodoItem::new("a")])
        .await
        .unwrap();
    todo_store::complete(&store, "u", 1).await.unwrap();
    assert_eq!(
        store::close_finished_plan(&store, "u").await.unwrap(),
        None,
        "no notes"
    );
}

#[test]
fn retract_reaches_the_last_tasks_notes() {
    let mut pad = Scratchpad {
        last_task: Some(TaskNotes {
            facts: vec!["port is 8080".into()],
            blockers: Vec::new(),
        }),
        ..Scratchpad::default()
    };
    assert!(!pad.is_empty());
    merge(
        &mut pad,
        &ScratchpadUpdate {
            retract: vec!["8080".into()],
            ..ScratchpadUpdate::default()
        },
    );
    assert_eq!(pad.last_task, None);
}
