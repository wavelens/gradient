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
/// The valid transition graph is:
/// ```text
/// Created → Queued
/// Queued  → Building
/// Building → Completed | Failed
/// * → Aborted          (except Completed, Substituted)
/// * → DependencyFailed (except Completed, Substituted, Failed)
/// ```
/// Terminal states (`Completed`, `Failed`, `Aborted`, `DependencyFailed`,
/// `Substituted`) cannot be transitioned away from.
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

        // Terminal states — nothing can move away from these.
        let from_is_terminal = matches!(
            from,
            BuildStatus::Completed
                | BuildStatus::Substituted
                | BuildStatus::Failed
                | BuildStatus::Aborted
                | BuildStatus::DependencyFailed
        );
        if from_is_terminal {
            return Err(InvalidBuildTransition { from, to });
        }

        match (from.clone(), to.clone()) {
            // Normal progression (still enforced for non-terminal states so
            // we don't accidentally skip Queued / Building in the UI).
            (BuildStatus::Created, BuildStatus::Queued) => Ok(to),
            (BuildStatus::Queued, BuildStatus::Building) => Ok(to),

            // Terminal transitions are allowed from ANY non-terminal source.
            // The `from_is_terminal` guard above already prevents moves out
            // of terminal states; here we deliberately accept shortcuts like
            //   Queued → Completed   (worker built so fast the `Building`
            //                          update was lost or never sent)
            //   Queued → Failed      (worker errored before reporting
            //                          `Building`)
            //   Created → Failed     (eval-side rejection before queueing)
            // Without these, a lost / out-of-order `Building` update would
            // strand the build in `Queued`/`Building` forever — the same
            // bug class as the silent Queued→Failed reject we hit earlier.
            (_, BuildStatus::Completed) => Ok(to),
            (_, BuildStatus::Failed) => Ok(to),
            (_, BuildStatus::Aborted) => Ok(to),
            (_, BuildStatus::DependencyFailed) => Ok(to),

            // Substituted is set inline by the eval handler when discovery
            // sees the output is already in the cache; no transition needed.
            // Anything else is a real bug.
            _ => Err(InvalidBuildTransition { from, to }),
        }
    }

    /// Returns `true` if `status` is a terminal (no further transitions allowed).
    pub fn is_terminal(status: &BuildStatus) -> bool {
        matches!(
            status,
            BuildStatus::Completed
                | BuildStatus::Substituted
                | BuildStatus::Failed
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
    fn build_sm_building_to_failed() {
        assert!(BuildStateMachine::validate(BuildStatus::Building, BuildStatus::Failed).is_ok());
    }

    #[test]
    fn build_sm_any_nonterminal_to_aborted() {
        for from in [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ] {
            assert!(
                BuildStateMachine::validate(from.clone(), BuildStatus::Aborted).is_ok(),
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
                BuildStateMachine::validate(from.clone(), BuildStatus::DependencyFailed).is_ok(),
                "{from:?} → DependencyFailed should be valid"
            );
        }
    }

    #[test]
    fn build_sm_terminal_rejects_all() {
        let terminals = [
            BuildStatus::Completed,
            BuildStatus::Substituted,
            BuildStatus::Failed,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
        ];
        for from in &terminals {
            // Try transitioning to every non-same status
            for to in [
                BuildStatus::Created,
                BuildStatus::Queued,
                BuildStatus::Building,
            ] {
                assert!(
                    BuildStateMachine::validate(from.clone(), to.clone()).is_err(),
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
            assert!(BuildStateMachine::validate(s.clone(), s).is_ok());
        }
    }

    #[test]
    fn build_sm_skip_queued_rejected() {
        assert!(BuildStateMachine::validate(BuildStatus::Created, BuildStatus::Building).is_err());
    }

    /// Terminal transitions (`Failed`, `Completed`, `Aborted`,
    /// `DependencyFailed`) are accepted from every non-terminal source.
    /// This is the regression guard for "JobFailed / JobCompleted arriving
    /// while the build is still `Queued` because the worker's `Building`
    /// update was lost or never sent" — without this, the build would be
    /// silently stranded in the wrong state.
    #[test]
    fn build_sm_any_nonterminal_to_any_terminal() {
        let from_states = [
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
        ];
        let terminal_states = [
            BuildStatus::Completed,
            BuildStatus::Failed,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
        ];
        for from in &from_states {
            for to in &terminal_states {
                assert!(
                    BuildStateMachine::validate(from.clone(), to.clone()).is_ok(),
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
            BuildStatus::Failed,
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
}
