/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;

use crate::ids::{ProjectGitHttpsCredentialId, ProjectId, UserId};

#[derive(Clone, Default, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project_git_https_credential")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectGitHttpsCredentialId,
    pub project: ProjectId,
    pub host: String,
    pub username: String,
    #[sea_orm(column_type = "Text")]
    pub password: String,
    pub updated_by: Option<UserId>,
    pub updated_at: NaiveDateTime,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectGitHttpsCredential")
            .field("id", &self.id)
            .field("project", &self.project)
            .field("host", &self.host)
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .field("updated_by", &self.updated_by)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UpdatedBy",
        to = "super::user::Column::Id",
        on_delete = "SetNull"
    )]
    UpdatedBy,
}

impl Related<super::project::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
