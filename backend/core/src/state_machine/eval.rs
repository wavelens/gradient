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
///        → Building → Waiting → Building (back-and-forth)
///        → Completed | Failed | Aborted
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

        // Terminal states — nothing can move away from these.
        let from_is_terminal = matches!(
            from,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        );
        if from_is_terminal {
            return Err(InvalidEvalTransition { from, to });
        }

        match (from.clone(), to.clone()) {
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
