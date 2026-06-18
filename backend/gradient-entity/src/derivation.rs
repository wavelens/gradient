/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationId, OrganizationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationId,
    pub organization: OrganizationId,
    pub hash: String,
    pub name: String,
    pub architecture: super::server::Architecture,
    pub pname: Option<String>,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub allow_substitutes: bool,
    pub closure_size: Option<i64>,
    pub dep_closure_count: Option<i64>,
    pub created_at: NaiveDateTime,
}

impl Model {
    /// This derivation as a [`StorePath`](crate::StorePath) (`name` keeps `.drv`).
    pub fn as_store_path(&self) -> crate::StorePath {
        crate::StorePath::from_parts(self.hash.clone(), format!("{}.drv", self.name))
    }

    /// Canonical `<hash>-<name>.drv` base form (no `/nix/store/` prefix),
    /// matching the wire shape used by workers and the cache narinfo
    /// `References:` convention.
    pub fn drv_path(&self) -> String {
        self.as_store_path().base()
    }

    /// Full `/nix/store/<hash>-<name>.drv` path for dispatch + worker store ops.
    pub fn store_path(&self) -> String {
        self.as_store_path().full()
    }
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
