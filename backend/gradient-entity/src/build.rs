/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
    #[sea_orm(num_value = 4)]
    FailedPermanent = 4,
    #[sea_orm(num_value = 5)]
    Aborted = 5,
    #[sea_orm(num_value = 6)]
    DependencyFailed = 6,
    /// `Substituted` is equal to `Completed` at every gate. The outputs were already valid or
    /// fetched by a passthrough from an upstream cache.
    #[sea_orm(num_value = 7)]
    Substituted = 7,
    #[sea_orm(num_value = 8)]
    FailedTransient = 8,
    #[sea_orm(num_value = 9)]
    FailedTimeout = 9,
    #[sea_orm(num_value = 10)]
    Skipped = 10,
}

impl BuildStatus {
    pub const fn for_api(self) -> Self {
        match self {
            Self::Created => Self::Queued,
            other => other,
        }
    }

    pub const fn is_failure(self) -> bool {
        matches!(
            self,
            Self::FailedPermanent
                | Self::FailedTransient
                | Self::FailedTimeout
                | Self::DependencyFailed
        )
    }

    pub const fn is_terminal_failure(self) -> bool {
        matches!(
            self,
            Self::FailedPermanent | Self::FailedTimeout | Self::DependencyFailed
        )
    }

    pub const fn is_terminal_success(self) -> bool {
        matches!(self, Self::Completed | Self::Substituted)
    }

    pub const PENDING: [Self; 2] = [Self::Created, Self::Queued];

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

    pub const ABORTABLE: [Self; 3] = [Self::Created, Self::Queued, Self::Building];

    pub const RETRYABLE: [Self; 3] = [Self::FailedPermanent, Self::FailedTimeout, Self::Aborted];

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
            BuildStatus::Skipped,
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

    /// Raw SQL is composing fragments from these numbers. A renumber must fail CI because the
    /// m20260407 evaluation-status renumber once corrupted every sweep.
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
            (BuildStatus::Skipped, 10),
        ] {
            assert_eq!(i32::from(status), n);
        }
        assert_eq!(BuildStatus::iter().count(), 11);
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
