/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RoleId, TeamId, TeamProjectId};

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    EnumIter,
    DeriveActiveEnum,
    Deserialize,
    Serialize,
    utoipa::ToSchema,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum TeamGrantSource {
    #[default]
    #[sea_orm(num_value = 0)]
    Manual,
    #[sea_orm(num_value = 1)]
    Owner,
    #[sea_orm(num_value = 2)]
    State,
    #[sea_orm(num_value = 3)]
    EveryProjectWorkerGrant,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "team_project")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: TeamProjectId,
    pub team: TeamId,
    pub project: ProjectId,
    pub role: Option<RoleId>,
    pub includes_users: bool,
    pub includes_workers: bool,
    pub grant_source: TeamGrantSource,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::team::Entity",
        from = "Column::Team",
        to = "super::team::Column::Id"
    )]
    Team,
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
}

impl ActiveModelBehavior for ActiveModel {}
