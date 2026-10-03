/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RoleId, UserId};

// A view over `project_user` and team grants; never written.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project_access")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub project: ProjectId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub user: UserId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub role: RoleId,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
