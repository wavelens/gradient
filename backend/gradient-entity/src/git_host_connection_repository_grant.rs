/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use num_enum::{IntoPrimitive, TryFromPrimitive};
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{GitHostConnectionId, GitHostConnectionRepositoryGrantId, ProjectId, UserId};

#[repr(i16)]
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
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "snake_case")]
pub enum RepositoryGrantSource {
    #[default]
    #[sea_orm(num_value = 0)]
    Admin = 0,
    #[sea_orm(num_value = 1)]
    Upgrade = 1,
    #[sea_orm(num_value = 2)]
    StateFile = 2,
}

/// A connection owned by the instance or a team can act for project tasks only on repositories
/// that are granted here. Set ``repository_name`` to ``None`` to allow all repositories of the owner.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "git_host_connection_repository_grant")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: GitHostConnectionRepositoryGrantId,
    pub git_host_connection: GitHostConnectionId,
    pub project: ProjectId,
    #[sea_orm(column_type = "Text")]
    pub repository_owner: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub repository_name: Option<String>,
    pub source: RepositoryGrantSource,
    pub created_by: Option<UserId>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::git_host_connection::Entity",
        from = "Column::GitHostConnection",
        to = "super::git_host_connection::Column::Id",
        on_delete = "Cascade"
    )]
    GitHostConnection,
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id",
        on_delete = "SetNull"
    )]
    CreatedBy,
}

impl ActiveModelBehavior for ActiveModel {}
