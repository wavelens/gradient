/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum BuildStatus {
    #[sea_orm(num_value = 0)]
    Created,
    #[sea_orm(num_value = 1)]
    Queued,
    #[sea_orm(num_value = 2)]
    Building,
    #[sea_orm(num_value = 3)]
    Completed,
    #[sea_orm(num_value = 4)]
    Failed,
    #[sea_orm(num_value = 5)]
    Aborted,
    #[sea_orm(num_value = 6)]
    DependencyFailed,
    /// The derivation was already present in the Nix store at evaluation
    /// time; no actual work was performed in this evaluation. Distinct from
    /// `Completed` (which means "we ran the build and it succeeded").
    #[sea_orm(num_value = 7)]
    Substituted,
}

impl BuildStatus {
    pub const fn num_value(&self) -> i32 {
        match self {
            Self::Created => 0,
            Self::Queued => 1,
            Self::Building => 2,
            Self::Completed => 3,
            Self::Failed => 4,
            Self::Aborted => 5,
            Self::DependencyFailed => 6,
            Self::Substituted => 7,
        }
    }

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
    pub id: Uuid,
    pub evaluation: Uuid,
    pub derivation: Uuid,
    pub status: BuildStatus,
    pub log_id: Option<Uuid>,
    pub build_time_ms: Option<i64>,
    /// Worker identity (the `worker_id` string sent in `InitConnection`) that
    /// executed this build. `None` for builds that never reached a worker
    /// (still queued, aborted before dispatch, or pre-migration rows).
    pub worker: Option<String>,
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
            assert_eq!(status.clone().for_api(), status);
        }
    }
}
