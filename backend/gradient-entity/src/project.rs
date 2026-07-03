/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, OrganizationId, ProjectId, UserId};

/// What happens to a project's in-flight evaluation when a new one triggers.
#[repr(i16)]
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    DeriveActiveEnum,
    EnumIter,
    Deserialize,
    Serialize,
    IntoPrimitive,
    TryFromPrimitive,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    #[sea_orm(num_value = 0)]
    HardAbort = 0,
    #[default]
    #[sea_orm(num_value = 1)]
    SoftAbort = 1,
    #[sea_orm(num_value = 2)]
    All = 2,
    #[sea_orm(num_value = 3)]
    Skip = 3,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectId,
    pub organization: OrganizationId,
    #[sea_orm(indexed)]
    pub name: String,
    pub active: bool,
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub repository: String,
    pub wildcard: String,
    pub last_evaluation: Option<EvaluationId>,
    pub last_check_at: NaiveDateTime,
    pub force_evaluation: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub keep_evaluations: i32,
    pub concurrency: ConcurrencyPolicy,
    pub sign_cache: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::organization::Entity",
        from = "Column::Organization",
        to = "super::organization::Column::Id"
    )]
    Organization,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::LastEvaluation",
        to = "super::evaluation::Column::Id"
    )]
    LastEvaluation,
}

impl ActiveModelBehavior for ActiveModel {}
