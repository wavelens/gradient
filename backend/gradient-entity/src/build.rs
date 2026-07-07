/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build status enum, shared by the global `derivation_build` anchor and the
//! per-eval `build_job`. The `build` table itself was removed; identity now
//! lives in `derivation_build` (global, build-once) + `build_job` (per-eval).

use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum BuildStatus {
    #[default]
    #[sea_orm(num_value = 0)]
    Created = 0,
    #[sea_orm(num_value = 1)]
    Queued = 1,
    #[sea_orm(num_value = 2)]
    Building = 2,
    #[sea_orm(num_value = 3)]
    Completed = 3,
    /// Terminal failure: the build will not be retried. Set when the builder
    /// exits non-zero, or a transient failure exhausts the retry budget.
    #[sea_orm(num_value = 4)]
    FailedPermanent = 4,
    #[sea_orm(num_value = 5)]
    Aborted = 5,
    #[sea_orm(num_value = 6)]
    DependencyFailed = 6,
    /// The derivation was already present in the Nix store at evaluation
    /// time; no actual work was performed in this evaluation.
    #[sea_orm(num_value = 7)]
    Substituted = 7,
    /// Non-terminal failure: an infrastructure error (OOM, disk full, network
    /// timeout, builder crash) that the scheduler will retry until the attempt
    /// budget is spent. Entry-point queries ignore builds in this state.
    #[sea_orm(num_value = 8)]
    FailedTransient = 8,
    /// Terminal failure: the build exceeded its wall-clock or silent timeout.
    #[sea_orm(num_value = 9)]
    FailedTimeout = 9,
}

impl BuildStatus {
    /// Maps internal-only states onto their API-facing equivalents.
    pub const fn for_api(self) -> Self {
        match self {
            Self::Created => Self::Queued,
            other => other,
        }
    }

    /// Any failure state (terminal or pending-retry).
    pub const fn is_failure(self) -> bool {
        matches!(
            self,
            Self::FailedPermanent
                | Self::FailedTransient
                | Self::FailedTimeout
                | Self::DependencyFailed
        )
    }

    /// A failure that counts as a final, user-visible failure (excludes the
    /// pending-retry `FailedTransient`).
    pub const fn is_terminal_failure(self) -> bool {
        matches!(
            self,
            Self::FailedPermanent | Self::FailedTimeout | Self::DependencyFailed
        )
    }

    /// Build-once success states, never re-queued by a new evaluation.
    pub const fn is_terminal_success(self) -> bool {
        matches!(self, Self::Completed | Self::Substituted)
    }

    pub const TERMINAL_SUCCESS: [Self; 2] = [Self::Completed, Self::Substituted];

    pub const TERMINAL_FAILURE: [Self; 3] = [
        Self::FailedPermanent,
        Self::DependencyFailed,
        Self::FailedTimeout,
    ];

    pub const FAILURE: [Self; 4] = [
        Self::FailedPermanent,
        Self::DependencyFailed,
        Self::FailedTransient,
        Self::FailedTimeout,
    ];

    /// States a fresh evaluation intent thaws back to `Created`: the terminal
    /// failures plus `Aborted` (retried, not permanent).
    pub const REQUEUEABLE: [Self; 4] = [
        Self::FailedPermanent,
        Self::Aborted,
        Self::DependencyFailed,
        Self::FailedTimeout,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Iterable;

    #[test]
    fn for_api_collapses_created_to_queued() {
        assert_eq!(BuildStatus::Created.for_api(), BuildStatus::Queued);
    }

    #[test]
    fn for_api_passes_through_other_states() {
        for status in [
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::Completed,
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTransient,
            BuildStatus::FailedTimeout,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
            BuildStatus::Substituted,
        ] {
            assert_eq!(status.for_api(), status);
        }
    }

    #[test]
    fn is_failure_covers_all_failure_states() {
        assert!(BuildStatus::FailedPermanent.is_failure());
        assert!(BuildStatus::FailedTransient.is_failure());
        assert!(BuildStatus::FailedTimeout.is_failure());
        assert!(BuildStatus::DependencyFailed.is_failure());
        assert!(!BuildStatus::Completed.is_failure());
        assert!(!BuildStatus::Building.is_failure());
    }

    #[test]
    fn terminal_failure_excludes_transient() {
        assert!(BuildStatus::FailedPermanent.is_terminal_failure());
        assert!(BuildStatus::FailedTimeout.is_terminal_failure());
        assert!(!BuildStatus::FailedTransient.is_terminal_failure());
    }

    /// Raw SQL composes fragments from these numbers; a renumber must fail CI
    /// (the m20260407 evaluation-status renumber corrupted every sweep once).
    #[test]
    fn numbering_is_pinned() {
        for (status, n) in [
            (BuildStatus::Created, 0),
            (BuildStatus::Queued, 1),
            (BuildStatus::Building, 2),
            (BuildStatus::Completed, 3),
            (BuildStatus::FailedPermanent, 4),
            (BuildStatus::Aborted, 5),
            (BuildStatus::DependencyFailed, 6),
            (BuildStatus::Substituted, 7),
            (BuildStatus::FailedTransient, 8),
            (BuildStatus::FailedTimeout, 9),
        ] {
            assert_eq!(i32::from(status), n);
        }
        assert_eq!(BuildStatus::iter().count(), 10);
    }

    #[test]
    fn semantic_sets_match_their_predicates() {
        let by = |pred: fn(BuildStatus) -> bool| -> Vec<BuildStatus> {
            BuildStatus::iter().filter(|s| pred(*s)).collect()
        };
        assert_eq!(
            by(BuildStatus::is_terminal_failure),
            BuildStatus::TERMINAL_FAILURE
        );
        assert_eq!(by(BuildStatus::is_failure), BuildStatus::FAILURE);
        assert_eq!(
            by(BuildStatus::is_terminal_success),
            BuildStatus::TERMINAL_SUCCESS
        );
        assert_eq!(
            by(|s| s.is_terminal_failure() || s == BuildStatus::Aborted),
            BuildStatus::REQUEUEABLE
        );
    }
}
