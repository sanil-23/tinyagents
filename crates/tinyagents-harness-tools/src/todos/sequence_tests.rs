use super::*;

use TodoStatus::{Cancelled, Completed, InProgress, Pending};

fn plan(specs: &[(&str, TodoStatus)]) -> Vec<TodoItem> {
    specs
        .iter()
        .map(|(content, status)| TodoItem::with_status(*content, *status))
        .collect()
}

fn statuses(items: &[TodoItem]) -> Vec<TodoStatus> {
    items.iter().map(|item| item.status).collect()
}

#[test]
fn creating_a_plan_starts_step_one() {
    let mut items = plan(&[("A", Pending), ("B", Pending)]);
    let notes = apply_write(&[], &mut items).unwrap();
    assert_eq!(statuses(&items), [InProgress, Pending]);
    assert_eq!(notes, ["Step 1 \"A\" is now in progress."]);
}

#[test]
fn several_in_progress_keep_the_first_and_say_so() {
    let mut items = plan(&[("A", InProgress), ("B", InProgress), ("C", InProgress)]);
    let notes = apply_write(&[], &mut items).unwrap();
    assert_eq!(statuses(&items), [InProgress, Pending, Pending]);
    assert!(
        notes[0].contains("step(s) 2, 3 back to pending"),
        "{notes:?}"
    );
}

#[test]
fn whole_list_writes_keep_the_order() {
    let previous = plan(&[("A", InProgress), ("B", Pending), ("C", Pending)]);
    for (written, expected) in [
        // Starting step 2 while step 1 is open.
        (
            plan(&[("A", Pending), ("B", InProgress), ("C", Pending)]),
            Err("Finish or cancel step 1 first."),
        ),
        // Completing step 3 out of order.
        (
            plan(&[("A", InProgress), ("B", Pending), ("C", Completed)]),
            Err(
                "Finish or cancel steps 1, 2 first, in order: todo {\"complete\": [N, ...]} for the ones that are done, or todo {\"cancel\": N, \"reason\": \"...\"}.",
            ),
        ),
        // Ticking the step in progress and starting the next is the happy path.
        (
            plan(&[("A", Completed), ("B", InProgress), ("C", Pending)]),
            Ok(vec![Completed, InProgress, Pending]),
        ),
        // The step in progress may be completed while an earlier one is still
        // open (it was already the step being worked).
        (
            plan(&[("X", Pending), ("A", Completed), ("C", Pending)]),
            Ok(vec![InProgress, Completed, Pending]),
        ),
    ] {
        let mut items = written.clone();
        match (apply_write(&previous, &mut items), expected) {
            (Err(message), Err(expected)) => assert_eq!(message, expected, "{written:?}"),
            (Ok(_), Ok(expected)) => assert_eq!(statuses(&items), expected, "{written:?}"),
            (got, expected) => panic!("{written:?}: got {got:?}, expected {expected:?}"),
        }
    }
}

#[test]
fn complete_ticks_the_step_and_starts_the_next() {
    let mut items = plan(&[("A", InProgress), ("B", Pending), ("C", Pending)]);
    let notes = complete(&mut items, 1).unwrap();
    assert_eq!(statuses(&items), [Completed, InProgress, Pending]);
    assert_eq!(notes, ["Step 2 \"B\" is now in progress."]);

    complete(&mut items, 2).unwrap();
    let notes = complete(&mut items, 3).unwrap();
    assert_eq!(statuses(&items), [Completed, Completed, Completed]);
    assert_eq!(notes, ["Every step is closed."]);
    assert_eq!(
        complete(&mut items, 3).unwrap()[0],
        "Step 3 was already completed."
    );
}

#[test]
fn complete_out_of_order_or_out_of_range_is_refused() {
    let mut items = plan(&[("A", InProgress), ("B", Pending), ("C", Pending)]);
    let before = items.clone();
    assert_eq!(
        complete(&mut items, 3).unwrap_err(),
        "Finish or cancel steps 1, 2 first, in order: todo {\"complete\": [N, ...]} for the ones that are done, or todo {\"cancel\": N, \"reason\": \"...\"}."
    );
    for step in [0, 4] {
        assert!(
            complete(&mut items, step)
                .unwrap_err()
                .starts_with(&format!("There is no step {step}")),
        );
    }
    assert_eq!(items, before, "a refusal changes nothing");
}

#[test]
fn complete_a_pending_step_when_everything_above_is_closed() {
    // A list written before the order rules, with nothing in progress.
    let mut items = plan(&[("A", Completed), ("B", Pending), ("C", Pending)]);
    complete(&mut items, 2).unwrap();
    assert_eq!(statuses(&items), [Completed, Completed, InProgress]);
}

#[test]
fn cancel_needs_a_reason_and_records_it() {
    let mut items = plan(&[("A", InProgress), ("B", Pending), ("C", Pending)]);
    assert_eq!(
        cancel(&mut items, 1, "  ").unwrap_err(),
        "`cancel` needs a `reason`."
    );

    // A later step may be dropped while step 1 is still being worked.
    let notes = cancel(&mut items, 3, "not needed").unwrap();
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(statuses(&items), [InProgress, Pending, Cancelled]);
    assert_eq!(items[2].reason.as_deref(), Some("not needed"));

    // Cancelling the step in progress starts the next open one.
    let notes = cancel(&mut items, 1, "blocked").unwrap();
    assert_eq!(statuses(&items), [Cancelled, InProgress, Cancelled]);
    assert_eq!(notes, ["Step 2 \"B\" is now in progress."]);

    assert_eq!(
        cancel(&mut items, 1, "again").unwrap_err(),
        "Step 1 is already cancelled."
    );
    assert!(
        complete(&mut items, 1)
            .unwrap_err()
            .contains("was cancelled")
    );
}

#[test]
fn complete_steps_closes_in_order_and_announces_only_the_new_current_step() {
    let mut items = plan(&[("A", InProgress), ("B", Pending), ("C", Pending)]);
    let notes = complete_steps(&mut items, &[2, 1, 2]).unwrap();
    assert_eq!(statuses(&items), [Completed, Completed, InProgress]);
    assert_eq!(notes, ["Step 3 \"C\" is now in progress."]);
    assert!(complete_steps(&mut items, &[]).is_err());
}
