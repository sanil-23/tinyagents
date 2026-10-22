//! CRUD for the per-thread todo list, on the harness
//! [`Store`](tinyagents_harness::store::Store).
//!
//! Each thread's list is a single serialized [`TodoList`] value under the
//! [`TODOS_NAMESPACE`] namespace, keyed by the hex-encoded thread id. Every
//! mutation runs `load → mutate → normalise → put` under a **per-thread async
//! mutex** ([`thread_lock`]) so the read-modify-write is atomic within the
//! process (the same single-process caveat as
//! [`GoalStore`](crate::goals::GoalStore)).
//!
//! The list is written wholesale ([`replace`]) and then ticked off one step at
//! a time ([`complete`], [`cancel`]); every write keeps the step order the
//! `sequence` module defines. Each mutator returns a [`TodosSnapshot`] — the
//! normalised items, a markdown rendering and the notes the write produced —
//! so an agent transcript and a UI stay in lock-step.

use std::sync::{Arc, OnceLock};

use tokio::sync::Mutex;

use super::sequence;
use super::types::{TodoItem, TodoList, TodosSnapshot, normalise_list, now_stamp, render_markdown};
use crate::thread_locks::ThreadLockMap;
use tinyagents_harness::error::{Result, TinyAgentsError};
use tinyagents_harness::store::Store;

/// The [`Store`] namespace holding one [`TodoList`] per thread.
pub const TODOS_NAMESPACE: &str = "graph.todos";

/// Serialises `load → mutate → put` per thread so a read-modify-write is atomic
/// within the process. Unused mutexes are reclaimed (see
/// [`ThreadLockMap`](crate::thread_locks::ThreadLockMap)) so the map
/// does not grow with every thread id ever seen.
fn thread_lock(thread_id: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<ThreadLockMap> = OnceLock::new();
    LOCKS
        .get_or_init(|| ThreadLockMap::new("todo lock map"))
        .lock_for(thread_id)
}

/// Hex-encodes the thread id into a [`Store`]-safe key.
fn key(thread_id: &str) -> String {
    thread_id
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn validate_thread_id(thread_id: &str) -> Result<String> {
    let trimmed = thread_id.trim();
    if trimmed.is_empty() {
        return Err(TinyAgentsError::Validation(
            "todo list thread_id must not be empty or whitespace".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Loads the raw items for `thread_id` (empty when the thread has no list).
async fn load_items(store: &Arc<dyn Store>, thread_id: &str) -> Result<Vec<TodoItem>> {
    Ok(get(store, thread_id)
        .await?
        .map(|list| list.items)
        .unwrap_or_default())
}

/// Load a list without normalising it, preserving the distinction between an
/// absent list and a present empty list.
pub async fn get(store: &Arc<dyn Store>, thread_id: &str) -> Result<Option<TodoList>> {
    let thread_id = validate_thread_id(thread_id)?;
    match store.get(TODOS_NAMESPACE, &key(&thread_id)).await? {
        Some(value) => Ok(Some(serde_json::from_value(value)?)),
        None => Ok(None),
    }
}

/// Delete a list value outright, returning whether one was present.
///
/// This differs from [`clear`], which persists a present, empty list.
pub async fn delete(store: &Arc<dyn Store>, thread_id: &str) -> Result<bool> {
    let thread_id = validate_thread_id(thread_id)?;
    let lock = thread_lock(&thread_id);
    let _guard = lock.lock().await;
    let list_key = key(&thread_id);
    let existed = store.get(TODOS_NAMESPACE, &list_key).await?.is_some();
    if existed {
        store.delete(TODOS_NAMESPACE, &list_key).await?;
    }
    Ok(existed)
}

/// Normalises and persists `items` for `thread_id`, returning the normalised set.
async fn save_items(
    store: &Arc<dyn Store>,
    thread_id: &str,
    items: Vec<TodoItem>,
) -> Result<Vec<TodoItem>> {
    let mut list = TodoList {
        thread_id: thread_id.to_string(),
        items,
        updated_at: now_stamp(),
    };
    normalise_list(&mut list);
    let value = serde_json::to_value(&list)?;
    store.put(TODOS_NAMESPACE, &key(thread_id), value).await?;
    Ok(list.items)
}

fn snapshot(thread_id: &str, items: Vec<TodoItem>, notes: Vec<String>) -> TodosSnapshot {
    let markdown = render_markdown(&items);
    TodosSnapshot {
        thread_id: thread_id.to_string(),
        items,
        markdown,
        notes,
    }
}

/// Runs `change` on the thread's current items under the thread lock and
/// persists the result. A refusal leaves the stored list untouched and comes
/// back as a [`Validation`](TinyAgentsError::Validation) error.
async fn mutate(
    store: &Arc<dyn Store>,
    thread_id: &str,
    change: impl FnOnce(&mut Vec<TodoItem>) -> std::result::Result<Vec<String>, String>,
) -> Result<TodosSnapshot> {
    let thread_id = validate_thread_id(thread_id)?;
    let lock = thread_lock(&thread_id);
    let _guard = lock.lock().await;
    let mut items = load_items(store, &thread_id).await?;
    let notes = change(&mut items).map_err(TinyAgentsError::Validation)?;
    let items = save_items(store, &thread_id, items).await?;
    Ok(snapshot(&thread_id, items, notes))
}

/// Snapshot the current list without mutating.
pub async fn list(store: &Arc<dyn Store>, thread_id: &str) -> Result<TodosSnapshot> {
    let thread_id = validate_thread_id(thread_id)?;
    let lock = thread_lock(&thread_id);
    let _guard = lock.lock().await;
    let items = load_items(store, &thread_id).await?;
    Ok(snapshot(&thread_id, items, Vec::new()))
}

/// Wholesale-replace the thread's list. Blank items are dropped first, so
/// step numbers count only the steps that are kept. The new list must keep the
/// step order against the one it replaces; with open steps and none in
/// progress, the first open step starts.
pub async fn replace(
    store: &Arc<dyn Store>,
    thread_id: &str,
    mut items: Vec<TodoItem>,
) -> Result<TodosSnapshot> {
    for item in &mut items {
        item.content = item.content.trim().to_string();
    }
    items.retain(|item| !item.content.is_empty());
    mutate(store, thread_id, move |current| {
        let notes = sequence::apply_write(current, &mut items)?;
        *current = items;
        Ok(notes)
    })
    .await
}

/// Complete step `step` (1-based) and start the next open one.
pub async fn complete(
    store: &Arc<dyn Store>,
    thread_id: &str,
    step: usize,
) -> Result<TodosSnapshot> {
    mutate(store, thread_id, |items| sequence::complete(items, step)).await
}

/// Complete several steps (1-based) top to bottom in one write, all or none,
/// and start the next open one.
pub async fn complete_steps(
    store: &Arc<dyn Store>,
    thread_id: &str,
    steps: &[usize],
) -> Result<TodosSnapshot> {
    mutate(store, thread_id, |items| {
        sequence::complete_steps(items, steps)
    })
    .await
}

/// Cancel `cancel` (with `reason`) and complete `complete` in one write, all
/// or none; see `sequence::close_steps` for the order.
pub async fn close_steps(
    store: &Arc<dyn Store>,
    thread_id: &str,
    complete: &[usize],
    cancel: &[usize],
    reason: &str,
) -> Result<TodosSnapshot> {
    mutate(store, thread_id, |items| {
        sequence::close_steps(items, complete, cancel, reason)
    })
    .await
}

/// Cancel step `step` (1-based) with `reason`; when it was the step in
/// progress, the next open one starts.
pub async fn cancel(
    store: &Arc<dyn Store>,
    thread_id: &str,
    step: usize,
    reason: &str,
) -> Result<TodosSnapshot> {
    mutate(store, thread_id, |items| {
        sequence::cancel(items, step, reason)
    })
    .await
}

/// Empty the list.
pub async fn clear(store: &Arc<dyn Store>, thread_id: &str) -> Result<TodosSnapshot> {
    let thread_id = validate_thread_id(thread_id)?;
    let lock = thread_lock(&thread_id);
    let _guard = lock.lock().await;
    let items = save_items(store, &thread_id, Vec::new()).await?;
    Ok(snapshot(&thread_id, items, Vec::new()))
}
