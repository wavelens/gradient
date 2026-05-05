/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, OrganizationUserId, RoleId, UserId};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "organization_user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: OrganizationUserId,
    pub organization: OrganizationId,
    pub user: UserId,
    pub role: RoleId,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Organization,
    User,
    Role,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Organization => Entity::belongs_to(super::organization::Entity)
                .from(Column::Organization)
                .to(super::organization::Column::Id)
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
