/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RoleId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "role")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: RoleId,
    #[sea_orm(indexed)]
    pub name: String,
    pub project: Option<ProjectId>,
    pub permission: i64,
    /// True for roles created by `gradient-state.nix`. Managed roles are
    /// immutable through the role-management API, the same way built-in
    /// roles are.
    pub managed: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
}

impl ActiveModelBehavior for ActiveModel {}
