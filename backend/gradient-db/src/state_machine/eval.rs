/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct InvalidEvalTransition {
    pub from: EvaluationStatus,
    pub to: EvaluationStatus,
}

impl fmt::Display for InvalidEvalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid evaluation status transition: {:?} -> {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidEvalTransition {}

pub struct EvalStateMachine;

impl EvalStateMachine {
    pub fn validate(
        from: EvaluationStatus,
        to: EvaluationStatus,
    ) -> Result<EvaluationStatus, InvalidEvalTransition> {
        if from == to {
            return Ok(to);
        }

        let from_is_terminal = matches!(
            from,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        );
        if from_is_terminal {
            return Err(InvalidEvalTransition { from, to });
        }

        match (from, to) {
            (EvaluationStatus::Queued, EvaluationStatus::Fetching) => Ok(to),
            (EvaluationStatus::Queued, EvaluationStatus::EvaluatingFlake) => Ok(to),
            (EvaluationStatus::Fetching, EvaluationStatus::EvaluatingFlake) => Ok(to),
            (EvaluationStatus::EvaluatingFlake, EvaluationStatus::EvaluatingDerivation) => Ok(to),
            (EvaluationStatus::EvaluatingDerivation, EvaluationStatus::Building) => Ok(to),
            (EvaluationStatus::EvaluatingDerivation, EvaluationStatus::Completed) => Ok(to),

            (EvaluationStatus::Building, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::Waiting, EvaluationStatus::Building) => Ok(to),
            (EvaluationStatus::Building, EvaluationStatus::Completed) => Ok(to),

            // Recovery from `Waiting` is routing through `Queued` to replay the normal progression.
            (EvaluationStatus::Queued, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::Fetching, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::EvaluatingFlake, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::EvaluatingDerivation, EvaluationStatus::Waiting) => Ok(to),
            (EvaluationStatus::Waiting, EvaluationStatus::Queued) => Ok(to),

            (_, EvaluationStatus::Failed) => Ok(to),
            (_, EvaluationStatus::Aborted) => Ok(to),
            (_, EvaluationStatus::Completed) => Ok(to),

            _ => Err(InvalidEvalTransition { from, to }),
        }
    }

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
                "{from:?} -> {to:?} failed"
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
                "{from:?} -> Failed failed"
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
                "{from:?} -> Aborted failed"
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
                "{from:?} -> Waiting should be allowed"
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
        for to in [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
        ] {
            assert!(
                EvalStateMachine::validate(EvaluationStatus::Waiting, to).is_err(),
                "Waiting -> {to:?} should be rejected"
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
                    "{from:?} -> {to:?} should be rejected"
                );
            }
        }
    }

    #[test]
    fn eval_sm_skip_fetching_ok() {
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
