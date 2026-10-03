/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{RoleId, TeamId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "team")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: TeamId,
    #[sea_orm(unique)]
    pub name: String,
    pub display_name: String,
    pub oidc_group: Option<String>,
    pub scim_group: Option<String>,
    pub new_project_users: bool,
    pub new_project_workers: bool,
    pub new_project_role: Option<RoleId>,
    pub created_by: Option<UserId>,
    pub managed: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
