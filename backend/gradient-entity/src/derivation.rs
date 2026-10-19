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
    /// A stub row only named by a batch is `false` until walked. `true` is set once outputs,
    /// dependency edges and input sources are recorded.
    pub walked: bool,
    /// The walk is pruning on `walked AND unwalked_inputs = 0`. See
    /// `gradient_db::graph::walk_completeness`.
    pub unwalked_inputs: i32,
    pub closure_size: Option<i64>,
    pub ifd: bool,
    pub created_at: NaiveDateTime,
}

impl Model {
    pub fn as_store_path(&self) -> crate::StorePath {
        crate::StorePath::from_stored(self.hash.clone(), format!("{}.drv", self.name))
    }

    pub fn drv_path(&self) -> String {
        self.as_store_path().base()
    }

    pub fn history_name(&self) -> &str {
        match self.pname.as_deref() {
            Some(pname) if !pname.is_empty() => pname,
            _ => without_version(&self.name),
        }
    }

    pub fn store_path(&self) -> String {
        self.as_store_path().full()
    }
}

fn without_version(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((prefix, version)) if version.starts_with(|c: char| c.is_ascii_digit()) => prefix,
        _ => name,
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

    #[test]
    fn history_name_drops_the_version_of_a_name_without_pname() {
        let stub = |name: &str| Model {
            name: name.into(),
            pname: None,
            ..Default::default()
        };
        assert_eq!(stub("acl-2.3.2").history_name(), "acl");
        assert_eq!(
            stub("python3.14-torch-2.9.0").history_name(),
            "python3.14-torch"
        );
        assert_eq!(stub("bash-static").history_name(), "bash-static");
    }
}
