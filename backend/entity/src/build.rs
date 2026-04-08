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

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub evaluation: Uuid,
    pub derivation: Uuid,
    pub status: BuildStatus,
    pub server: Option<Uuid>,
    pub log_id: Option<Uuid>,
    pub build_time_ms: Option<i64>,
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
    #[sea_orm(
        belongs_to = "super::server::Entity",
        from = "Column::Server",
        to = "super::server::Column::Id"
    )]
    Server,
}

impl ActiveModelBehavior for ActiveModel {}
