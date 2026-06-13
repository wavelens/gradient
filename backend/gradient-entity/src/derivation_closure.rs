/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Materialised build-time dependency closure of a derivation. A row
//! `(root_derivation, dep_derivation)` means "`dep_derivation` is in the
//! transitive build closure of `root_derivation`" (root itself excluded).
//! Content-addressed, so it is computed once per root and reused across evals.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{DerivationClosureId, DerivationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_closure")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DerivationClosureId,
    pub root_derivation: DerivationId,
    pub dep_derivation: DerivationId,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Root,
    Dep,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Root => Entity::belongs_to(super::derivation::Entity)
                .from(Column::RootDerivation)
                .to(super::derivation::Column::Id)
                .into(),
            Self::Dep => Entity::belongs_to(super::derivation::Entity)
                .from(Column::DepDerivation)
                .to(super::derivation::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
