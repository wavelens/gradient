/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Pure retry and terminal-status policy for a build anchor.

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
use gradient_types::proto::BuildFailureKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureOutcome {
    Retry,
    Permanent,
    Timeout,
    /// Penalty-free re-queue (substitute miss): back to `Queued` without
    /// bumping `attempt`. Escalation to a real build is decided at dispatch.
    Requeue,
    /// The server ordered the job stopped. Terminal for this evaluation but not
    /// a verdict on the derivation, so the anchor lands on the requeueable
    /// `Aborted` rather than `FailedPermanent`.
    Aborted,
}

/// Decide what to do with a failed build given its classification and how many
/// attempts it has already had (`attempt` is the count *before* this failure).
pub(crate) fn decide_failure_outcome(
    kind: BuildFailureKind,
    attempt: i32,
    max_attempts: u32,
) -> FailureOutcome {
    match kind {
        BuildFailureKind::Timeout => FailureOutcome::Timeout,
        BuildFailureKind::Permanent => FailureOutcome::Permanent,
        BuildFailureKind::Aborted => FailureOutcome::Aborted,
        BuildFailureKind::SubstituteUnavailable => FailureOutcome::Requeue,
        // A missing input self-heals, so it retries in-eval like a transient
        // failure; the caller forces `Permanent` when the circuit trips.
        BuildFailureKind::InputsUnavailable | BuildFailureKind::Transient => {
            if (attempt + 1) < max_attempts as i32 {
                FailureOutcome::Retry
            } else {
                FailureOutcome::Permanent
            }
        }
        // Eval-only kind; a build never produces it, so treat it as terminal.
        BuildFailureKind::CorruptEvalCache => FailureOutcome::Permanent,
    }
}

/// Terminal success status for a build whose job completed. `Substituted` when
/// the daemon found the outputs already valid and ran no build (recorded on
/// `build.substituted`), else `Completed`. Decided at `JobCompleted`, after the
/// worker has pushed the output NARs, so a build never reaches a dispatch-ready
/// terminal state while its bytes are still absent from the cache - the #399
/// regression where a dependent dispatched into that window and failed
/// `InputsUnavailable`.
pub(crate) fn terminal_success_status(outputs_already_valid: bool) -> BuildStatus {
    if outputs_already_valid {
        BuildStatus::Substituted
    } else {
        BuildStatus::Completed
    }
}

/// Terminal `build_attempt.outcome` for a job that completed, mirroring
/// [`terminal_success_status`]. Without this the success path leaves the attempt
/// at `Running`, and `recover_interrupted_work` later rewrites every such row to
/// `Aborted` - so a healthy instance reports roughly half its attempts aborted.
pub(crate) fn terminal_success_outcome(outputs_already_valid: bool) -> AttemptOutcome {
    if outputs_already_valid {
        AttemptOutcome::Substituted
    } else {
        AttemptOutcome::Built
    }
}

/// Best-effort mapping from the worker's failure classification to a stored
/// `build_attempt.reason`. `Transient` has no single cause, so it stays `None`;
/// an abort is not a failure of the derivation and carries no reason at all.
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

/// How the attempt row is closed out. An abort is recorded as `Aborted`, so the
/// `deterministic_build_failure` predicate (`outcome = Failed AND reason =
/// BuilderNonzero`) cannot match it: stamping a user abort as a reproducible
/// builder exit excluded the anchor from `requeue_failed_anchors` forever, so no
/// later evaluation could ever rebuild it (#572).
pub(crate) fn attempt_outcome(kind: BuildFailureKind) -> AttemptOutcome {
    match kind {
        BuildFailureKind::Aborted => AttemptOutcome::Aborted,
        _ => AttemptOutcome::Failed,
    }
}

/// Circuit breaker for the `InputsUnavailable` self-heal. Each failed eval
/// reconciles the cache (purges the stale input) so the next eval rebuilds it; a
/// genuinely unrecoverable input turns that into a hot loop that churns the cache
/// forever. `prior_failures` is how many `InputsUnavailable` attempts this anchor
/// already has, so the self-heal runs for the first `max_loops` and the circuit
/// opens after - the build then fails fast without reconciling.
pub(crate) fn inputs_unavailable_circuit_open(prior_failures: i64, max_loops: u32) -> bool {
    prior_failures >= max_loops as i64
}

/// True when a `FailedTransient` build's exponential backoff window has elapsed
/// and it is due for re-queue. `attempt` is `>= 1` (it failed at least once);
/// window = `base_secs * 2^(attempt-1)`.
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

/// Cap a worker failure string before persisting it on `build_attempt`. The full
/// text already lands in the build log; the stored message is for quick surfacing,
/// so bound it on a char boundary to keep the row lean.
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
    use super::{
        FailureOutcome, attempt_outcome, attempt_reason, decide_failure_outcome,
        inputs_unavailable_circuit_open, retry_backoff_elapsed, terminal_success_outcome,
        terminal_success_status, truncate_failure_message,
    };
    use gradient_entity::build::BuildStatus;
    use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome};
    use gradient_types::proto::BuildFailureKind;

    /// The user pressed Abort: the worker stopped nix, nothing about the
    /// derivation failed. Reporting it as `Permanent` landed the anchor on
    /// `FailedPermanent` with `reason = BuilderNonzero`, which
    /// `deterministic_build_failure` reads as a reproducible builder exit and
    /// excludes from every requeue - the derivation could never be built again
    /// (#572).
    #[test]
    fn abort_is_not_a_deterministic_build_failure() {
        for attempt in [0, 1, 99] {
            assert_eq!(
                decide_failure_outcome(BuildFailureKind::Aborted, attempt, 3),
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

    /// The exception exists for a real reason: a builder that exited non-zero
    /// reproduces on rebuild, so thawing it loops the fleet. It must stay
    /// attached to that one case.
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
                "{kind:?} must not poison the anchor as a deterministic failure"
            );
        }
    }

    #[test]
    fn permanent_is_terminal_regardless_of_attempt() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Permanent, 0, 3),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn timeout_is_terminal() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Timeout, 0, 3),
            FailureOutcome::Timeout
        );
    }

    #[test]
    fn transient_retries_until_budget_then_permanent() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 0, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 1, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3),
            FailureOutcome::Permanent
        );
    }

    #[test]
    fn substitute_unavailable_requeues_penalty_free() {
        for attempt in [0, 5, 100] {
            assert_eq!(
                decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, attempt, 3),
                FailureOutcome::Requeue
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
    fn substitute_miss_requeues_but_real_failures_cap_at_three() {
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 0, 3),
            FailureOutcome::Requeue
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::SubstituteUnavailable, 99, 3),
            FailureOutcome::Requeue
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::Transient, 0, 3),
            FailureOutcome::Retry
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::Transient, 1, 3),
            FailureOutcome::Retry
        ));
        assert!(matches!(
            decide_failure_outcome(BuildFailureKind::Transient, 2, 3),
            FailureOutcome::Permanent
        ));
    }

    /// A missing input is self-healed (its producer is re-queued) and the build
    /// retries in-eval, so it behaves like a transient failure up to the attempt
    /// budget rather than failing permanently on the first miss.
    #[test]
    fn inputs_unavailable_retries_like_transient_then_permanent() {
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 0, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 1, 3),
            FailureOutcome::Retry
        );
        assert_eq!(
            decide_failure_outcome(BuildFailureKind::InputsUnavailable, 2, 3),
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
    fn terminal_status_is_substituted_only_when_outputs_were_already_valid() {
        assert_eq!(terminal_success_status(true), BuildStatus::Substituted);
        assert_eq!(terminal_success_status(false), BuildStatus::Completed);
    }

    /// Success must be recorded on the attempt, never left at `Running`: the
    /// recovery sweep turns a lingering `Running` row into `Aborted`.
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
}
