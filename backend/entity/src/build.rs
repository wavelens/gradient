/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
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
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub evaluation: Uuid,
    pub status: BuildStatus,
    pub derivation_path: String,
    pub architecture: super::server::Architecture,
    pub server: Option<Uuid>,
    #[sea_orm(column_type = "Text")]
    pub log: Option<String>,
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
        belongs_to = "super::server::Entity",
        from = "Column::Server",
        to = "super::server::Column::Id"
    )]
    Server,
}

impl Related<super::build::Entity> for Entity {
    fn to() -> RelationDef {
        super::build_dependency::Relation::Dependency.def()
    }

    fn via() -> Option<RelationDef> {
        Some(super::build_dependency::Relation::Build.def().rev())
    }
}

impl ActiveModelBehavior for ActiveModel {}
