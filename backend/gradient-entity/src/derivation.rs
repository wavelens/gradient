/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::DerivationId;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationId,
    pub hash: String,
    pub name: String,
    pub architecture: super::server::Architecture,
    pub pname: Option<String>,
    pub prefer_local_build: bool,
    pub is_fixed_output: bool,
    pub allow_substitutes: bool,
    /// The full record is in: outputs, every declared dependency edge and
    /// input source. A row a batch only named (a stub) is `false` until walked.
    pub walked: bool,
    /// Direct inputs whose subtree is not recorded. The walk prunes on
    /// `walked AND unwalked_inputs = 0`; see `gradient_db::walk_completeness`.
    pub unwalked_inputs: i32,
    pub closure_size: Option<i64>,
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

    /// Identity its build history is recorded and predicted under: `pname`, else
    /// the `name` for derivations that declare none.
    pub fn history_name(&self) -> &str {
        self.pname.as_deref().unwrap_or(&self.name)
    }

    /// Full `/nix/store/<hash>-<name>.drv` path for dispatch + worker store ops.
    pub fn store_path(&self) -> String {
        self.as_store_path().full()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_name_falls_back_to_the_name_without_pname() {
        let named = Model {
            name: "hello-2.12".into(),
            pname: Some("hello".into()),
            ..Default::default()
        };
        let unnamed = Model {
            name: "etc".into(),
            pname: None,
            ..Default::default()
        };
        assert_eq!(named.history_name(), "hello");
        assert_eq!(unnamed.history_name(), "etc");
    }
}
