/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{TeamId, TeamUserId, UserId};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, EnumIter, DeriveActiveEnum, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "lowercase")]
pub enum TeamRole {
    #[sea_orm(num_value = 0)]
    Admin,
    #[default]
    #[sea_orm(num_value = 1)]
    Member,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, EnumIter, DeriveActiveEnum, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
#[serde(rename_all = "lowercase")]
pub enum TeamMemberSource {
    #[default]
    #[sea_orm(num_value = 0)]
    Api,
    #[sea_orm(num_value = 1)]
    State,
    #[sea_orm(num_value = 2)]
    Group,
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "team_user")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: TeamUserId,
    pub team: TeamId,
    pub user: UserId,
    pub role: TeamRole,
    pub source: TeamMemberSource,
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
        belongs_to = "super::user::Entity",
        from = "Column::User",
        to = "super::user::Column::Id"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
