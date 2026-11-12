/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;

use crate::ids::{ProjectId, ProjectSshHostKeyId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project_ssh_host_key")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectSshHostKeyId,
    pub project: ProjectId,
    pub host: String,
    pub port: i32,
    #[sea_orm(column_type = "Text")]
    pub public_key: String,
    pub learned_at: NaiveDateTime,
    pub changed_at: Option<NaiveDateTime>,
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
}

impl Related<super::project::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
