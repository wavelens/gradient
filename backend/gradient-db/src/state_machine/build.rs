/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct InvalidBuildTransition {
    pub from: BuildStatus,
    pub to: BuildStatus,
}

impl fmt::Display for InvalidBuildTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid build status transition: {:?} to {:?}",
            self.from, self.to
        )
    }
}

impl std::error::Error for InvalidBuildTransition {}

pub struct BuildStateMachine;

impl BuildStateMachine {
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

            // A `Skipped` build is never queued from here.
            // The thaw back to `Created` is re-opening the gate.
            (BuildStatus::Created, BuildStatus::Skipped) => Ok(to),
            (BuildStatus::Skipped, BuildStatus::Created) => Ok(to),
            (BuildStatus::Queued, BuildStatus::Building) => Ok(to),
            (BuildStatus::Queued, BuildStatus::Created) => Ok(to),

            (BuildStatus::FailedTransient, BuildStatus::Queued) => Ok(to),

            // A substitute miss is re-queueing a `Building` attempt without an `attempt` bump.
            (BuildStatus::Building, BuildStatus::Queued) => Ok(to),
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
    fn build_sm_skipped_is_entered_and_left_through_created_only() {
        assert!(BuildStateMachine::validate(BuildStatus::Created, BuildStatus::Skipped).is_ok());
        assert!(BuildStateMachine::validate(BuildStatus::Skipped, BuildStatus::Created).is_ok());
        assert!(BuildStateMachine::validate(BuildStatus::Skipped, BuildStatus::Queued).is_err());
        assert!(BuildStateMachine::validate(BuildStatus::Queued, BuildStatus::Skipped).is_err());
        assert!(!BuildStateMachine::is_terminal(&BuildStatus::Skipped));
    }

    #[test]
    fn build_sm_queued_to_building() {
        assert!(BuildStateMachine::validate(BuildStatus::Queued, BuildStatus::Building).is_ok());
    }

    #[test]
    fn build_sm_queued_to_created_for_unpromotion() {
        assert!(BuildStateMachine::validate(BuildStatus::Queued, BuildStatus::Created).is_ok());
        assert!(BuildStateMachine::validate(BuildStatus::Building, BuildStatus::Created).is_err());
    }

    #[test]
    fn build_sm_building_to_substituted() {
        assert!(
            BuildStateMachine::validate(BuildStatus::Building, BuildStatus::Substituted).is_ok()
        );
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
                    "{from:?} to {to:?} should be rejected"
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
                    "{from:?} to {to:?} should be valid (terminal shortcut)"
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
            BuildStateMachine::validate(BuildStatus::Building, BuildStatus::FailedTransient)
                .is_ok()
        );
    }

    #[test]
    fn build_sm_building_to_queued_for_substitute_requeue() {
        assert!(BuildStateMachine::validate(BuildStatus::Building, BuildStatus::Queued).is_ok());
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
        assert!(!BuildStateMachine::is_terminal(
            &BuildStatus::FailedTransient
        ));
    }

    #[test]
    fn build_sm_failed_permanent_and_timeout_are_terminal() {
        assert!(BuildStateMachine::is_terminal(
            &BuildStatus::FailedPermanent
        ));
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
