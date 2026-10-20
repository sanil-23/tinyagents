use super::*;

fn spent() -> TurnRecovery {
    TurnRecovery {
        truncated_empty_retries_used: 2,
        truncated_empty_nudges_used: 1,
        empty_response_retries_used: 3,
        dropped_tool_call_nudges_used: 2,
        withheld_call_nudges_used: 1,
        truncated_tool_call_retries_used: 2,
        truncated_tool_names: ["save_workflow".to_string()].into(),
        truncated_repeat_names: ["save_workflow".to_string()].into(),
        boosted_max_tokens: Some(4096),
        truncation_base: Some(1024),
        reasoning_off_next: true,
        dead_calls_this_run: 3,
        reasoning_off_sticky: true,
    }
}

#[test]
fn boost_doubles_the_last_sent_cap_and_clamps_at_four_times_the_base() {
    let mut recovery = TurnRecovery::default();
    recovery.boost_max_tokens(Some(1000));
    assert_eq!(recovery.boosted_max_tokens, Some(2000));
    assert_eq!(recovery.truncation_base, Some(1000));
    recovery.boost_max_tokens(Some(2000));
    assert_eq!(recovery.boosted_max_tokens, Some(4000));
    recovery.boost_max_tokens(Some(4000));
    assert_eq!(
        recovery.boosted_max_tokens,
        Some(4000),
        "clamped at 4x base"
    );
    assert_eq!(recovery.truncation_base, Some(1000));
}

#[test]
fn boost_leaves_an_unset_cap_unset() {
    let mut recovery = TurnRecovery::default();
    recovery.boost_max_tokens(None);
    assert_eq!(recovery, TurnRecovery::default());
}

#[test]
fn nudge_cap_is_clamped_to_the_clock_affordable_cap() {
    let plan = TruncatedRetryPlan {
        current: Some(16_000),
        next: Some(16_000),
        base: Some(2_048),
        dead_tokens: 16_000,
        dead_ms: 16_000,
        remaining: Some(std::time::Duration::from_millis(6_000)),
        first_retry: false,
        constant_cap: false,
    };

    assert_eq!(plan.affordable_cap(), Some(3_000));
    assert_eq!(plan.nudge_cap(), Some(3_000));
    assert!(plan.another_nudge_fits());
}

#[test]
fn nudge_is_rejected_when_even_the_minimum_cap_exceeds_the_clock_budget() {
    let plan = TruncatedRetryPlan {
        current: Some(4_096),
        next: Some(4_096),
        base: Some(2_048),
        dead_tokens: 4_096,
        dead_ms: 4_096,
        remaining: Some(std::time::Duration::from_millis(2_000)),
        first_retry: false,
        constant_cap: false,
    };

    assert_eq!(plan.affordable_cap(), None);
    assert_eq!(plan.nudge_cap(), None);
    assert!(!plan.another_nudge_fits());
}

#[test]
fn uncapped_nudge_uses_the_dead_call_duration_as_its_clock_estimate() {
    let enough_time = TruncatedRetryPlan {
        current: None,
        next: None,
        base: None,
        dead_tokens: 0,
        dead_ms: 1_000,
        remaining: Some(std::time::Duration::from_millis(3_000)),
        first_retry: false,
        constant_cap: false,
    };
    let too_little_time = TruncatedRetryPlan {
        remaining: Some(std::time::Duration::from_millis(1_000)),
        ..enough_time
    };

    assert!(enough_time.another_nudge_fits());
    assert!(!too_little_time.another_nudge_fits());
}

#[test]
fn retry_clock_estimate_scales_with_the_candidate_cap_without_usage() {
    let plan = TruncatedRetryPlan {
        current: Some(4_000),
        next: Some(8_000),
        base: Some(4_000),
        dead_tokens: 0,
        dead_ms: 1_000,
        remaining: Some(std::time::Duration::from_millis(10_000)),
        first_retry: false,
        constant_cap: false,
    };

    assert_eq!(plan.expected_ms(), 2_000);
}

#[test]
fn reset_truncated_empty_clears_only_the_truncated_empty_state() {
    let mut recovery = spent();
    recovery.reset_truncated_empty();
    assert_eq!(recovery.truncated_empty_retries_used, 0);
    assert_eq!(recovery.truncated_empty_nudges_used, 0);
    assert_eq!(recovery.boosted_max_tokens, None);
    assert_eq!(recovery.truncation_base, None);
    // Untouched.
    assert_eq!(recovery.empty_response_retries_used, 3);
    assert_eq!(recovery.dropped_tool_call_nudges_used, 2);
    assert_eq!(recovery.withheld_call_nudges_used, 1);
    assert_eq!(recovery.truncated_tool_call_retries_used, 2);
}

#[test]
fn reset_after_tool_turn_clears_everything_but_the_run_wide_dead_call_state() {
    let mut recovery = spent();
    recovery.reset_after_tool_turn(false);
    // The run-wide dead-call count and sticky reasoning-off survive the
    // per-step reset: carrying them across steps is their whole job.
    assert_eq!(
        recovery,
        TurnRecovery {
            dead_calls_this_run: 3,
            reasoning_off_sticky: true,
            ..TurnRecovery::default()
        }
    );
}

#[test]
fn reset_after_a_truncated_tool_turn_keeps_the_retry_budget_and_boost() {
    let mut recovery = spent();
    recovery.reset_after_tool_turn(true);
    // The re-prompt counters clear...
    assert_eq!(recovery.dropped_tool_call_nudges_used, 0);
    assert_eq!(recovery.withheld_call_nudges_used, 0);
    assert_eq!(recovery.empty_response_retries_used, 0);
    // ...but the truncated turn keeps what its retry needs.
    assert_eq!(recovery.truncated_tool_call_retries_used, 2);
    assert_eq!(recovery.truncated_empty_retries_used, 2);
    assert_eq!(recovery.truncated_empty_nudges_used, 1);
    assert_eq!(recovery.boosted_max_tokens, Some(4096));
    assert_eq!(recovery.truncation_base, Some(1024));
}

#[test]
fn reset_after_final_clears_everything_but_the_run_wide_dead_call_state() {
    let mut recovery = spent();
    recovery.reset_after_final();
    // The run-wide dead-call count and sticky reasoning-off survive the
    // per-step reset: carrying them across steps is their whole job.
    assert_eq!(
        recovery,
        TurnRecovery {
            dead_calls_this_run: 3,
            reasoning_off_sticky: true,
            ..TurnRecovery::default()
        }
    );
}

#[test]
fn a_constant_cap_retries_at_the_same_cap_every_time() {
    let mut recovery = TurnRecovery::default();
    let first = recovery.truncated_retry_plan(Some(131_072), 131_072, 600_000, None, true);
    assert_eq!(first.next, Some(131_072));
    recovery.take_truncated_retry(Some(131_072), &first);

    let second = recovery.truncated_retry_plan(Some(131_072), 131_072, 600_000, None, true);
    assert_eq!(second.next, Some(131_072), "no doubling");
    assert!(
        second.worth_it(),
        "the second retry is still made at the same cap"
    );

    // The default policy doubles from the second retry.
    let doubling = recovery.truncated_retry_plan(Some(131_072), 131_072, 600_000, None, false);
    assert_eq!(doubling.next, Some(262_144));
}

#[test]
fn a_constant_cap_still_respects_the_clock() {
    let mut recovery = TurnRecovery::default();
    let first = recovery.truncated_retry_plan(Some(131_072), 131_072, 600_000, None, true);
    recovery.take_truncated_retry(Some(131_072), &first);
    // 600 s at the dead call's rate, with only 900 s left: more than half.
    let late = recovery.truncated_retry_plan(
        Some(131_072),
        131_072,
        600_000,
        Some(std::time::Duration::from_secs(900)),
        true,
    );
    assert!(!late.worth_it());
}
