/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildId, DerivationId, EvaluationId};

#[repr(i32)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum BuildStatus {
    #[sea_orm(num_value = 0)]
    Created = 0,
    #[sea_orm(num_value = 1)]
    Queued = 1,
    #[sea_orm(num_value = 2)]
    Building = 2,
    #[sea_orm(num_value = 3)]
    Completed = 3,
    #[sea_orm(num_value = 4)]
    Failed = 4,
    #[sea_orm(num_value = 5)]
    Aborted = 5,
    #[sea_orm(num_value = 6)]
    DependencyFailed = 6,
    /// The derivation was already present in the Nix store at evaluation
    /// time; no actual work was performed in this evaluation. Distinct from
    /// `Completed` (which means "we ran the build and it succeeded").
    #[sea_orm(num_value = 7)]
    Substituted = 7,
}

impl BuildStatus {
    /// Maps internal-only states onto their API-facing equivalents.
    /// `Created` is a transient pre-queue state the scheduler flips to
    /// `Queued` almost immediately, so it is collapsed to `Queued` for clients.
    pub const fn for_api(self) -> Self {
        match self {
            Self::Created => Self::Queued,
            other => other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BuildId,
    pub evaluation: EvaluationId,
    pub derivation: DerivationId,
    pub status: BuildStatus,
    pub log_id: Option<BuildId>,
    pub build_time_ms: Option<i64>,
    /// Worker identity (the `worker_id` string sent in `InitConnection`) that
    /// executed this build. `None` for builds that never reached a worker
    /// (still queued, aborted before dispatch, or pre-migration rows).
    pub worker: Option<String>,
    /// Points at another build sharing the same derivation whose result this
    /// build follows. `None` for leaders (and plain builds). Followers are
    /// skipped by the dispatcher; the leader's terminal status, log_id,
    /// build_time_ms, and worker are copied to followers when the leader
    /// finishes. Same-organization only - followers always share a `derivation`
    /// row with their leader.
    pub via: Option<BuildId>,
    /// `true` when the build's outputs are known to be available from an
    /// upstream cache (cache.nixos.org etc.) but are not yet in the gradient
    /// cache. The dispatcher hands these jobs to a worker which downloads
    /// from upstream, recompresses, and pushes to our cache instead of
    /// running an actual `nix build`. Always `false` for `Substituted`
    /// rows (those are already in our cache) and for plain rebuild jobs.
    pub external_cached: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id"
    )]
    Evaluation,
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

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
            BuildStatus::Failed,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
            BuildStatus::Substituted,
        ] {
            assert_eq!(status.for_api(), status);
        }
    }
}
