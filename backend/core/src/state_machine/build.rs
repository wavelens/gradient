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
    pub fn validate(from: BuildStatus, to: BuildStatus) -> Result<BuildStatus, InvalidBuildTransition> {
        if from == to {
            return Ok(to);
        }

        // Terminal states — nothing can move away from these.
        let from_is_terminal = matches!(
            from,
            BuildStatus::Completed | BuildStatus::Substituted | BuildStatus::Failed
                | BuildStatus::Aborted | BuildStatus::DependencyFailed
        );
        if from_is_terminal {
            return Err(InvalidBuildTransition { from, to });
        }

        match (from.clone(), to.clone()) {
            // Normal progression
            (BuildStatus::Created, BuildStatus::Queued) => Ok(to),
            (BuildStatus::Queued, BuildStatus::Building) => Ok(to),
            (BuildStatus::Building, BuildStatus::Completed) => Ok(to),
            (BuildStatus::Building, BuildStatus::Failed) => Ok(to),

            // Abort is allowed from any non-terminal state
            (_, BuildStatus::Aborted) => Ok(to),
            // DependencyFailed is allowed from any non-terminal state
            (_, BuildStatus::DependencyFailed) => Ok(to),

            _ => Err(InvalidBuildTransition { from, to }),
        }
    }

    /// Returns `true` if `status` is a terminal (no further transitions allowed).
    pub fn is_terminal(status: &BuildStatus) -> bool {
        matches!(
            status,
            BuildStatus::Completed | BuildStatus::Substituted | BuildStatus::Failed
                | BuildStatus::Aborted | BuildStatus::DependencyFailed
        )
    }
}
