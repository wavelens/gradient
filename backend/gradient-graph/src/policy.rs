/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_wire::types::BuildFailureKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Substitution {
    pub cache_available: bool,
    pub misses: i64,
    pub threshold: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureOutcome {
    Retry,
    Permanent,
    Timeout,
    Requeue,
    Exhausted,
    Aborted,
}

pub(crate) fn decide_failure_outcome(
    kind: BuildFailureKind,
    attempt: i32,
    max_attempts: u32,
    substitution: Substitution,
) -> FailureOutcome {
    let cache_available = substitution.cache_available;
    match kind {
        BuildFailureKind::Timeout => FailureOutcome::Timeout,
        BuildFailureKind::Permanent => FailureOutcome::Permanent,
        BuildFailureKind::Aborted => FailureOutcome::Aborted,
        BuildFailureKind::SubstituteUnavailable => requeue_or_exhaust(substitution),
        BuildFailureKind::InputsUnavailable | BuildFailureKind::Transient => {
            if (attempt + 1) < max_attempts as i32 {
                FailureOutcome::Retry
            } else if cache_available {
                requeue_or_exhaust(substitution)
            } else {
                FailureOutcome::Permanent
            }
        }
        BuildFailureKind::CorruptEvalCache => FailureOutcome::Permanent,
    }
}

/// Both re-queuing arms are recording the same reason and must read the same budget here.
/// A re-queue is deliberately not bumping the attempt counter.
/// Reading the budget on one arm only let a failing passthrough loop forever.
const fn requeue_or_exhaust(substitution: Substitution) -> FailureOutcome {
    if substitution.misses + 1 < substitution.threshold {
        FailureOutcome::Requeue
    } else {
        FailureOutcome::Exhausted
    }
}

pub(crate) const fn spends_substitute_budget(kind: BuildFailureKind) -> bool {
    matches!(
        kind,
        BuildFailureKind::SubstituteUnavailable
            | BuildFailureKind::InputsUnavailable
            | BuildFailureKind::Transient
    )
}

pub(crate) fn terminal_success_status(outputs_already_valid: bool) -> BuildStatus {
    if outputs_already_valid {
        BuildStatus::Substituted
    } else {
        BuildStatus::Completed
    }
}

pub(crate) fn terminal_success_outcome(outputs_already_valid: bool) -> AttemptOutcome {
    if outputs_already_valid {
        AttemptOutcome::Substituted
    } else {
        AttemptOutcome::Built
    }
}

/// A `Requeue` and its closing `Exhausted` are always recording `SubstituteUnavailable`.
/// The miss budget is counting exactly those rows and is the only stop for the re-queue loop.
/// A `None` reason, as a bare `Transient` would give, is requeuing forever.
pub(crate) fn attempt_reason_for(
    kind: BuildFailureKind,
    outcome: FailureOutcome,
) -> Option<AttemptFailureReason> {
    match outcome {
        FailureOutcome::Requeue | FailureOutcome::Exhausted => {
            Some(AttemptFailureReason::SubstituteUnavailable)
        }
        _ => attempt_reason(kind),
    }
}

pub(crate) fn attempt_reason(kind: BuildFailureKind) -> Option<AttemptFailureReason> {
    match kind {
        BuildFailureKind::SubstituteUnavailable => {
            Some(AttemptFailureReason::SubstituteUnavailable)
        }
        BuildFailureKind::InputsUnavailable => Some(AttemptFailureReason::InputsUnavailable),
        BuildFailureKind::Permanent => Some(AttemptFailureReason::BuilderNonzero),
        BuildFailureKind::Timeout => Some(AttemptFailureReason::WallClockTimeout),
        BuildFailureKind::Transient
        | BuildFailureKind::CorruptEvalCache
        | BuildFailureKind::Aborted => None,
    }
}

pub(crate) fn attempt_outcome(kind: BuildFailureKind) -> AttemptOutcome {
    match kind {
        BuildFailureKind::Aborted => AttemptOutcome::Aborted,
        _ => AttemptOutcome::Failed,
    }
}

pub(crate) fn inputs_unavailable_circuit_open(prior_failures: i64, max_loops: u32) -> bool {
    prior_failures >= max_loops as i64
}

pub(crate) fn retry_failed_eval(kind: BuildFailureKind, attempts: u64, max_attempts: u32) -> bool {
    kind == BuildFailureKind::Transient && attempts < u64::from(max_attempts)
}

pub fn retry_backoff_elapsed(
    attempt: i32,
    failed_at: chrono::NaiveDateTime,
    now: chrono::NaiveDateTime,
    base_secs: u64,
) -> bool {
    let shift = (attempt.max(1) - 1).min(16) as u32;
    let window = base_secs.saturating_mul(1u64 << shift);
    (now - failed_at).num_seconds() >= window as i64
}

pub(crate) fn truncate_failure_message(error: &str) -> String {
    const MAX: usize = 8 * 1024;
    if error.len() <= MAX {
        return error.to_string();
    }

    let end = (0..=MAX)
        .rev()
        .find(|&i| error.is_char_boundary(i))
        .unwrap_or(0);
    format!("{} [truncated]", &error[..end])
}

#[cfg(test)]
mod tests {
    fn sub(cache_available: bool, misses: i64) -> Substitution {
        Substitution {
            cache_available,
            misses,
            threshold: 2,
        }
    }
    use super::{
        FailureOutcome, Substitution, attempt_outcome, attempt_reason, attempt_reason_for,
        decide_failure_outcome, inputs_unavailable_circuit_open, retry_backoff_elapsed,
        retry_failed_eval, spends_substitute_budget, terminal_success_outcome,
        terminal_success_status, truncate_failure_message,
    };
    use gradient_entity::build::BuildStatus;
    use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
    use gradient_wire::types::BuildFailureKind;

    #[test]
    fn abort_is_not_a_deterministic_build_failure() {
        for attempt in [0, 1, 99] {
            assert_eq!(
                decide_failure_outcome(BuildFailureKind::Aborted, attempt, 3, sub(false, 0)),
                FailureOutcome::Aborted,
                "an abort is never a build verdict, at any attempt count"
            );
        }
        assert_eq!(
            attempt_outcome(BuildFailureKind::Aborted),
            AttemptOutcome::Aborted
        );
        assert_eq!(attempt_reason(BuildFailureKind::Aborted), None);
    }

    #[test]
    fn only_a_real_builder_exit_records_builder_nonzero() {
        assert_eq!(
            attempt_reason(BuildFailureKind::Permanent),
            Some(AttemptFailureReason::BuilderNonzero)
        );
        assert_eq!(
            attempt_outcome(BuildFailureKind::Permanent),
            AttemptOutcome::Failed
        );
        for kind in [
            BuildFailureKind::Transient,
            BuildFailureKind::Timeout,
            BuildFailureKind::SubstituteUnavailable,
            BuildFailureKind::InputsUnavailable,
            BuildFailureKind::CorruptEvalCache,
            BuildFailureKind::Aborted,
        ] {
            assert_ne!(
                attempt_reason(kind),
                Some(AttemptFailureReason::BuilderNonzero),
                "{kind:?} must not poison the shared build as a deterministic failure"
            );
        }
    }

    #[test]
    fn permanent_is_terminal_regardless_of_attempt() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Permanent, 0, 3, sub(false, 0)),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn timeout_is_terminal() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Timeout, 0, 3, sub(false, 0)),
            FailureOutcome::Timeout
        );
    }

    #[test]
    fn transient_retries_until_budget_then_permanent() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 0, 3, sub(false, 0)),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 1, 3, sub(false, 0)),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3, sub(false, 0)),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn substitute_unavailable_requeues_penalty_free() {
        for attempt in [0, 5, 100] {
            assert_eq!(
                decide_failure_outcome(
                    BuildFailureKind::SubstituteUnavailable,
                    attempt,
                    3,
                    sub(true, 0)
                ),
                FailureOutcome::Requeue
            );
        }
    }

    #[test]
    fn a_substitute_miss_requeues_below_the_threshold_and_exhausts_at_it() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 0, 3, sub(true, 0)),
            FailureOutcome::Requeue
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 0, 3, sub(true, 1)),
            FailureOutcome::Exhausted
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 99, 3, sub(true, 5)),
            FailureOutcome::Exhausted
        );
    }

    #[test]
    fn a_transient_passthrough_requeue_is_bounded_by_the_same_budget() {
        for kind in [
            BuildFailureKind::Transient,
            BuildFailureKind::InputsUnavailable,
        ] {
            assert_eq!(
                decide_failure_outcome(kind, 2, 3, sub(true, 0)),
                FailureOutcome::Requeue,
                "{kind:?} must still get its first penalty-free re-queue"
            );
            assert_eq!(
                decide_failure_outcome(kind, 2, 3, sub(true, 1)),
                FailureOutcome::Exhausted,
                "{kind:?} re-queued past the budget"
            );
            assert_eq!(
                decide_failure_outcome(kind, 2, 3, sub(true, 5)),
                FailureOutcome::Exhausted,
                "{kind:?} re-queued past the budget"
            );
        }
    }

    #[test]
    fn exactly_the_kinds_that_can_requeue_spend_the_budget() {
        for kind in [
            BuildFailureKind::Transient,
            BuildFailureKind::InputsUnavailable,
            BuildFailureKind::SubstituteUnavailable,
        ] {
            assert!(spends_substitute_budget(kind), "{kind:?}");
            assert_eq!(
                decide_failure_outcome(kind, 99, 3, sub(true, 0)),
                FailureOutcome::Requeue,
                "{kind:?} cannot reach a re-queue at all"
            );
        }

        for kind in [
            BuildFailureKind::Permanent,
            BuildFailureKind::Timeout,
            BuildFailureKind::Aborted,
            BuildFailureKind::CorruptEvalCache,
        ] {
            assert!(!spends_substitute_budget(kind), "{kind:?}");
            assert!(
                !matches!(
                    decide_failure_outcome(kind, 99, 3, sub(true, 0)),
                    FailureOutcome::Requeue | FailureOutcome::Exhausted
                ),
                "{kind:?} re-queues against a budget nothing counts for it"
            );
        }
    }

    #[test]
    fn backoff_grows_per_attempt() {
        let t0 = chrono::NaiveDateTime::default();
        assert!(!retry_backoff_elapsed(
            1,
            t0,
            t0 + chrono::Duration::seconds(29),
            30
        ));
        assert!(retry_backoff_elapsed(
            1,
            t0,
            t0 + chrono::Duration::seconds(30),
            30
        ));
        assert!(!retry_backoff_elapsed(
            2,
            t0,
            t0 + chrono::Duration::seconds(59),
            30
        ));
        assert!(retry_backoff_elapsed(
            2,
            t0,
            t0 + chrono::Duration::seconds(60),
            30
        ));
    }

    #[test]
    fn inputs_unavailable_retries_like_transient_then_permanent() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 0, 3, sub(false, 0)),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 1, 3, sub(false, 0)),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 2, 3, sub(false, 0)),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn inputs_unavailable_circuit_opens_after_max_loops() {
        assert!(!inputs_unavailable_circuit_open(0, 3));
        assert!(!inputs_unavailable_circuit_open(1, 3));
        assert!(!inputs_unavailable_circuit_open(2, 3));
        assert!(inputs_unavailable_circuit_open(3, 3));
        assert!(inputs_unavailable_circuit_open(7, 3));
    }

    #[test]
    fn truncate_failure_message_bounds_long_input_on_char_boundary() {
        assert_eq!(truncate_failure_message("short error"), "short error");
        let long = "é".repeat(8 * 1024);
        let out = truncate_failure_message(&long);
        assert!(out.len() <= 8 * 1024 + " [truncated]".len());
        assert!(out.ends_with(" [truncated]"));
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn an_exhausted_substitute_requeues_instead_of_failing_the_derivation() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3, sub(true, 0)),
            FailureOutcome::Requeue
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3, sub(false, 0)),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn a_cache_available_shared_build_still_retries_before_its_budget_is_spent() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 0, 3, sub(true, 0)),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 1, 3, sub(true, 0)),
            FailureOutcome::Retry
        );
    }

    #[test]
    fn every_requeue_records_the_reason_its_miss_budget_counts() {
        for kind in [
            BuildFailureKind::Transient,
            BuildFailureKind::InputsUnavailable,
            BuildFailureKind::SubstituteUnavailable,
        ] {
            for outcome in [FailureOutcome::Requeue, FailureOutcome::Exhausted] {
                assert_eq!(
                    attempt_reason_for(kind, outcome),
                    Some(AttemptFailureReason::SubstituteUnavailable),
                    "{kind:?} as {outcome:?} recorded no counted reason"
                );
            }
        }
        assert_eq!(
            attempt_reason_for(BuildFailureKind::Permanent, FailureOutcome::Permanent),
            attempt_reason(BuildFailureKind::Permanent)
        );
    }

    #[test]
    fn terminal_status_is_substituted_only_when_outputs_were_already_valid() {
        assert_eq!(terminal_success_status(true), BuildStatus::Substituted);
        assert_eq!(terminal_success_status(false), BuildStatus::Completed);
    }

    #[test]
    fn terminal_outcome_records_success_and_never_stays_running() {
        assert_eq!(terminal_success_outcome(true), AttemptOutcome::Substituted);
        assert_eq!(terminal_success_outcome(false), AttemptOutcome::Built);
        for already_valid in [true, false] {
            assert_ne!(
                terminal_success_outcome(already_valid),
                AttemptOutcome::Running
            );
            assert_ne!(
                terminal_success_outcome(already_valid),
                AttemptOutcome::Aborted
            );
        }
    }

    #[test]
    fn an_outage_requeues_an_evaluation_until_its_attempts_run_out() {
        assert!(retry_failed_eval(BuildFailureKind::Transient, 1, 3));
        assert!(retry_failed_eval(BuildFailureKind::Transient, 2, 3));
        assert!(!retry_failed_eval(BuildFailureKind::Transient, 3, 3));
        assert!(!retry_failed_eval(BuildFailureKind::Permanent, 1, 3));
    }
}
