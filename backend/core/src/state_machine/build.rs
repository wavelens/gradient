/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::build::BuildStatus;
use std::fmt;

/// Error returned when a [`BuildStatus`] transition is invalid.
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidBuildTransition {
    pub from: BuildStatus,
    pub to: BuildStatus,
}

impl fmt::Display for InvalidBuildTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid build status transition: {:?} → {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidBuildTransition {}

/// Validates and enforces [`BuildStatus`] state transitions.
///
/// ```text
/// Created → Queued
/// Queued  → Building
/// Building → Completed | Substituted | FailedPermanent | FailedTransient | FailedTimeout
/// FailedTransient → Queued | FailedPermanent
/// * → Aborted          (except terminal states)
/// * → DependencyFailed (except terminal states)
/// ```
/// Terminal states (`Completed`, `FailedPermanent`, `FailedTimeout`, `Aborted`,
/// `DependencyFailed`, `Substituted`) cannot be transitioned away from.
pub struct BuildStateMachine;

impl BuildStateMachine {
    /// Returns `Ok(to)` if the transition is valid, `Err` otherwise.
    pub fn validate(
        from: BuildStatus,
        to: BuildStatus,
    ) -> Result<BuildStatus, InvalidBuildTransition> {
        if from == to {
            return Ok(to);
        }

        let from_is_terminal = matches!(
            from,
            BuildStatus::Completed
                | BuildStatus::Substituted
                | BuildStatus::FailedPermanent
                | BuildStatus::FailedTimeout
                | BuildStatus::Aborted
                | BuildStatus::DependencyFailed
        );
        if from_is_terminal {
            return Err(InvalidBuildTransition { from, to });
        }

        match (from, to) {
            (BuildStatus::Created, BuildStatus::Queued) => Ok(to),
            (BuildStatus::Queued, BuildStatus::Building) => Ok(to),

            // FailedTransient can be retried (back to Queued) or promoted to permanent.
            (BuildStatus::FailedTransient, BuildStatus::Queued) => Ok(to),
            (_, BuildStatus::FailedPermanent) => Ok(to),
            (_, BuildStatus::FailedTransient) => Ok(to),
            (_, BuildStatus::FailedTimeout) => Ok(to),
            (_, BuildStatus::Completed) => Ok(to),
            (_, BuildStatus::Substituted) => Ok(to),
            (_, BuildStatus::Aborted) => Ok(to),
            (_, BuildStatus::DependencyFailed) => Ok(to),

            _ => Err(InvalidBuildTransition { from, to }),
        }
    }

    /// Returns `true` if `status` is a terminal (no further transitions allowed).
    pub fn is_terminal(status: &BuildStatus) -> bool {
        matches!(
            status,
            BuildStatus::Completed
                | BuildStatus::Substituted
                | BuildStatus::FailedPermanent
                | BuildStatus::FailedTimeout
                | BuildStatus::Aborted
                | BuildStatus::DependencyFailed
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_sm_created_to_queued() {
        assert!(BuildStateMachine::validate(BuildStatus::Created, BuildStatus::Queued).is_ok());
    }

    #[test]
    fn build_sm_queued_to_building() {
        assert!(BuildStateMachine::validate(BuildStatus::Queued, BuildStatus::Building).is_ok());
    }

    #[test]
    fn build_sm_building_to_completed() {
        assert!(BuildStateMachine::validate(BuildStatus::Building, BuildStatus::Completed).is_ok());
    }

    #[test]
    fn build_sm_building_to_substituted() {
        assert!(
            BuildStateMachine::validate(BuildStatus::Building, BuildStatus::Substituted).is_ok()
        );
    }

    #[test]
    fn build_sm_building_to_failed() {
        assert!(
            BuildStateMachine::validate(BuildStatus::Building, BuildStatus::FailedPermanent).is_ok()
        );
    }

    #[test]
    fn build_sm_any_nonterminal_to_aborted() {
        for from in [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ] {
            assert!(
                BuildStateMachine::validate(from, BuildStatus::Aborted).is_ok(),
                "{from:?} → Aborted should be valid"
            );
        }
    }

    #[test]
    fn build_sm_any_nonterminal_to_dep_failed() {
        for from in [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ] {
            assert!(
                BuildStateMachine::validate(from, BuildStatus::DependencyFailed).is_ok(),
                "{from:?} → DependencyFailed should be valid"
            );
        }
    }

    #[test]
    fn build_sm_terminal_rejects_all() {
        let terminals = [
            BuildStatus::Completed,
            BuildStatus::Substituted,
            BuildStatus::FailedPermanent,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
        ];
        for from in &terminals {
            for to in [
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ] {
                assert!(
                    BuildStateMachine::validate(*from, to).is_err(),
                    "{from:?} → {to:?} should be rejected"
                );
            }
        }
    }

    #[test]
    fn build_sm_same_state_ok() {
        for s in [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::Completed,
        ] {
            assert!(BuildStateMachine::validate(s, s).is_ok());
        }
    }

    #[test]
    fn build_sm_skip_queued_rejected() {
        assert!(BuildStateMachine::validate(BuildStatus::Created, BuildStatus::Building).is_err());
    }

    /// Terminal transitions (`FailedPermanent`, `Completed`, `Aborted`,
    /// `DependencyFailed`) are accepted from every non-terminal source.
    #[test]
    fn build_sm_any_nonterminal_to_any_terminal() {
        let from_states = [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ];
        let terminal_states = [
            BuildStatus::Completed,
            BuildStatus::FailedPermanent,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
        ];
        for from in &from_states {
            for to in &terminal_states {
                assert!(
                    BuildStateMachine::validate(*from, *to).is_ok(),
                    "{from:?} → {to:?} should be valid (terminal shortcut)"
                );
            }
        }
    }

    #[test]
    fn build_sm_is_terminal() {
        for s in [
            BuildStatus::Completed,
            BuildStatus::Substituted,
            BuildStatus::FailedPermanent,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
        ] {
            assert!(
                BuildStateMachine::is_terminal(&s),
                "{s:?} should be terminal"
            );
        }
        for s in [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ] {
            assert!(
                !BuildStateMachine::is_terminal(&s),
                "{s:?} should not be terminal"
            );
        }
    }

    #[test]
    fn build_sm_building_to_failed_transient() {
        assert!(
            BuildStateMachine::validate(BuildStatus::Building, BuildStatus::FailedTransient).is_ok()
        );
    }

    #[test]
    fn build_sm_failed_transient_to_queued_for_retry() {
        assert!(
            BuildStateMachine::validate(BuildStatus::FailedTransient, BuildStatus::Queued).is_ok()
        );
    }

    #[test]
    fn build_sm_failed_transient_to_permanent_when_exhausted() {
        assert!(
            BuildStateMachine::validate(BuildStatus::FailedTransient, BuildStatus::FailedPermanent)
                .is_ok()
        );
    }

    #[test]
    fn build_sm_failed_transient_is_not_terminal() {
        assert!(!BuildStateMachine::is_terminal(&BuildStatus::FailedTransient));
    }

    #[test]
    fn build_sm_failed_permanent_and_timeout_are_terminal() {
        assert!(BuildStateMachine::is_terminal(&BuildStatus::FailedPermanent));
        assert!(BuildStateMachine::is_terminal(&BuildStatus::FailedTimeout));
    }

    #[test]
    fn build_sm_terminal_failure_rejects_requeue() {
        assert!(
            BuildStateMachine::validate(BuildStatus::FailedPermanent, BuildStatus::Queued).is_err()
        );
        assert!(
            BuildStateMachine::validate(BuildStatus::FailedTimeout, BuildStatus::Queued).is_err()
        );
    }
}
