/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::evaluation::EvaluationStatus;
use std::fmt;

/// Error returned when an [`EvaluationStatus`] transition is invalid.
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidEvalTransition {
    pub from: EvaluationStatus,
    pub to: EvaluationStatus,
}

impl fmt::Display for InvalidEvalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid evaluation status transition: {:?} → {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidEvalTransition {}

/// Validates and enforces [`EvaluationStatus`] state transitions.
///
/// The valid transition graph is:
/// ```text
/// Queued → Fetching → EvaluatingFlake → EvaluatingDerivation
///        → Building ⇄ Waiting
///        → Completed | Failed | Aborted
/// {Queued,Fetching,EvaluatingFlake,EvaluatingDerivation} → Waiting
/// Waiting → Queued (recovery once a worker becomes available)
/// * → Aborted (from any non-terminal state)
/// * → Failed  (from any non-terminal state)
/// ```
/// Terminal states (`Completed`, `Failed`, `Aborted`) cannot be
/// transitioned away from.
pub struct EvalStateMachine;

impl EvalStateMachine {
    /// Returns `Ok(to)` if the transition is valid, `Err` otherwise.
    pub fn validate(
        from: EvaluationStatus,
        to: EvaluationStatus,
    ) -> Result<EvaluationStatus, InvalidEvalTransition> {
        if from == to {
            return Ok(to);
        }

        // Terminal states - nothing can move away from these.
        let from_is_terminal = matches!(
            from,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        );
        if from_is_terminal {
            return Err(InvalidEvalTransition { from, to });
        }

        match (from, to) {
            // Normal progression through evaluation phases
            (EvaluationStatus::Queued, EvaluationStatus::Fetching) => Ok(to),
            (EvaluationStatus::Queued, EvaluationStatus::EvaluatingFlake) => Ok(to),
            (EvaluationStatus::Fetching, EvaluationStatus::EvaluatingFlake) => Ok(to),
            (EvaluationStatus::EvaluatingFlake, EvaluationStatus::EvaluatingDerivation) => Ok(to),
            (EvaluationStatus::EvaluatingDerivation, EvaluationStatus::Building) => Ok(to),
            (EvaluationStatus::EvaluatingDerivation, EvaluationStatus::Completed) => Ok(to),

            // Build phase scheduling
            (EvaluationStatus::Building, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::Waiting, EvaluationStatus::Building) => Ok(to),
            (EvaluationStatus::Building, EvaluationStatus::Completed) => Ok(to),

            // Pre-build phases stall into Waiting when no worker can pick up
            // the eval job; recovery routes through Queued so the dispatch
            // loop replays the normal progression.
            (EvaluationStatus::Queued, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::Fetching, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::EvaluatingFlake, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::EvaluatingDerivation, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::Waiting, EvaluationStatus::Queued) => Ok(to),

            // Terminal transitions from any non-terminal state
            (_, EvaluationStatus::Failed) => Ok(to),
            (_, EvaluationStatus::Aborted) => Ok(to),
            (_, EvaluationStatus::Completed) => Ok(to),

            _ => Err(InvalidEvalTransition { from, to }),
        }
    }

    /// Returns `true` if `status` is a terminal (no further transitions allowed).
    pub fn is_terminal(status: &EvaluationStatus) -> bool {
        matches!(
            status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_sm_happy_path() {
        let chain = [
            (EvaluationStatus::Queued, EvaluationStatus::Fetching),
            (
                EvaluationStatus::Fetching,
                EvaluationStatus::EvaluatingFlake,
            ),
            (
                EvaluationStatus::EvaluatingFlake,
                EvaluationStatus::EvaluatingDerivation,
            ),
            (
                EvaluationStatus::EvaluatingDerivation,
                EvaluationStatus::Building,
            ),
            (EvaluationStatus::Building, EvaluationStatus::Completed),
        ];
        for (from, to) in chain {
            assert!(
                EvalStateMachine::validate(from, to).is_ok(),
                "{from:?} → {to:?} failed"
            );
        }
    }

    #[test]
    fn eval_sm_building_waiting_cycle() {
        assert!(
            EvalStateMachine::validate(EvaluationStatus::Building, EvaluationStatus::Waiting)
                .is_ok()
        );
        assert!(
            EvalStateMachine::validate(EvaluationStatus::Waiting, EvaluationStatus::Building)
                .is_ok()
        );
    }

    #[test]
    fn eval_sm_any_nonterminal_to_failed() {
        let nonterminals = [
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ];
        for from in nonterminals {
            assert!(
                EvalStateMachine::validate(from, EvaluationStatus::Failed).is_ok(),
                "{from:?} → Failed failed"
            );
        }
    }

    #[test]
    fn eval_sm_any_nonterminal_to_aborted() {
        let nonterminals = [
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ];
        for from in nonterminals {
            assert!(
                EvalStateMachine::validate(from, EvaluationStatus::Aborted).is_ok(),
                "{from:?} → Aborted failed"
            );
        }
    }

    #[test]
    fn eval_sm_pre_build_states_can_enter_waiting() {
        let pre_build = [
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ];
        for from in pre_build {
            assert!(
                EvalStateMachine::validate(from, EvaluationStatus::Waiting).is_ok(),
                "{from:?} → Waiting should be allowed"
            );
        }
    }

    #[test]
    fn eval_sm_waiting_recovers_to_queued() {
        assert!(
            EvalStateMachine::validate(EvaluationStatus::Waiting, EvaluationStatus::Queued).is_ok()
        );
    }

    #[test]
    fn eval_sm_waiting_cannot_skip_into_pre_build_phases() {
        // Recovery from Waiting goes via Queued; jumping straight to a later
        // pre-build phase would let us bypass the dispatch path.
        for to in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            assert!(
                EvalStateMachine::validate(EvaluationStatus::Waiting, to).is_err(),
                "Waiting → {to:?} should be rejected"
            );
        }
    }

    #[test]
    fn eval_sm_terminal_rejects_all() {
        for from in [
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ] {
            for to in [
                EvaluationStatus::Queued,
                EvaluationStatus::Building,
                EvaluationStatus::Fetching,
            ] {
                assert!(
                    EvalStateMachine::validate(from, to).is_err(),
                    "{from:?} → {to:?} should be rejected"
                );
            }
        }
    }

    #[test]
    fn eval_sm_skip_fetching_ok() {
        // Queued → EvaluatingFlake is explicitly allowed (line 65)
        assert!(
            EvalStateMachine::validate(EvaluationStatus::Queued, EvaluationStatus::EvaluatingFlake)
                .is_ok()
        );
    }

    #[test]
    fn eval_sm_same_state_ok() {
        for s in [
            EvaluationStatus::Queued,
            EvaluationStatus::Building,
            EvaluationStatus::Fetching,
        ] {
            assert!(EvalStateMachine::validate(s, s).is_ok());
        }
    }

    #[test]
    fn eval_sm_is_terminal() {
        for s in [
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ] {
            assert!(
                EvalStateMachine::is_terminal(&s),
                "{s:?} should be terminal"
            );
        }
        for s in [
            EvaluationStatus::Queued,
            EvaluationStatus::Building,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Waiting,
        ] {
            assert!(
                !EvalStateMachine::is_terminal(&s),
                "{s:?} should not be terminal"
            );
        }
    }
}
