/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, ProjectUserId, RoleId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectUserId,
    pub project: ProjectId,
    pub user: UserId,
    pub role: RoleId,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Project,
    User,
    Role,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Project => Entity::belongs_to(super::project::Entity)
                .from(Column::Project)
                .to(super::project::Column::Id)
                .into(),
            Self::User => Entity::belongs_to(super::user::Entity)
                .from(Column::User)
                .to(super::user::Column::Id)
                .into(),
            Self::Role => Entity::belongs_to(super::role::Entity)
                .from(Column::Role)
                .to(super::role::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
