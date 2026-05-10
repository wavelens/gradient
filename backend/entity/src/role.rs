/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, RoleId};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "role")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: RoleId,
    #[sea_orm(indexed)]
    pub name: String,
    pub organization: Option<OrganizationId>,
    pub permission: i64,
    /// True for roles created by `gradient-state.nix`. Managed roles are
    /// immutable through the role-management API, the same way built-in
    /// roles are.
    pub managed: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::organization::Entity",
        from = "Column::Organization",
        to = "super::organization::Column::Id"
    )]
    Organization,
}

impl ActiveModelBehavior for ActiveModel {}
